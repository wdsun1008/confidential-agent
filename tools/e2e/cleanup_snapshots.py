#!/usr/bin/env python3.11
"""Track and remove automatic snapshots of this test's imported ECS images."""

import argparse
import json
import subprocess
from pathlib import Path


def ecs(region, action, **options):
    command = ["aliyun", "--region", region, "ecs", action, "--RegionId", region]
    for key, value in options.items():
        command.extend(["--" + key, value])
    result = subprocess.run(command, capture_output=True, text=True, timeout=60)
    if result.returncode:
        # CLI diagnostics may contain request credentials; keep them out of logs.
        raise RuntimeError(f"{action} failed (exit {result.returncode})")
    return json.loads(result.stdout)


def record(entries, default_region):
    for image in list(entries):
        if image["type"] not in ("alicloud_image", "alicloud_image_import"):
            continue
        region = image.get("region", default_region)
        data = ecs(region, "DescribeImages", ImageId=image["id"], Status="ALL")
        for detail in data.get("Images", {}).get("Image", []):
            if detail["ImageId"] != image["id"]:
                raise RuntimeError("DescribeImages returned an unexpected image")
            for disk in detail.get("DiskDeviceMappings", {}).get("DiskDeviceMapping", []):
                snapshot = disk.get("SnapshotId")
                if not snapshot:
                    continue
                item = {"type": "alicloud_snapshot", "id": snapshot,
                        "image_id": image["id"], "image_name": detail["ImageName"],
                        "region": region, "service": image.get("service", "")}
                if item not in entries:
                    entries.append(item)


def cleanup(entries, default_region):
    for item in entries:
        if item["type"] != "alicloud_snapshot":
            continue
        region = item.get("region", default_region)
        options = {"SnapshotIds": json.dumps([item["id"]])}
        data = ecs(region, "DescribeSnapshots", **options)
        snapshots = data.get("Snapshots", {}).get("Snapshot", [])
        if not snapshots:
            if data.get("TotalCount") != 0:
                raise RuntimeError("Cannot verify snapshot absence")
            continue
        images = ecs(region, "DescribeImages", ImageId=item["image_id"], Status="ALL")
        if images.get("TotalCount") != 0:
            raise RuntimeError(f"Image still exists for snapshot {item['id']}")
        for snapshot in snapshots:
            # Only automatic import snapshots, with an exact recorded association.
            if (snapshot.get("SnapshotId") != item["id"]
                    or snapshot.get("SnapshotName") != "SnapshotForImage-" + item["image_name"]
                    or snapshot.get("Description") != item["image_name"]
                    or snapshot.get("SourceDiskId") != "mock-d-" + item["id"]
                    or snapshot.get("Usage") != "none"):
                raise RuntimeError(f"Refusing to delete unverified or used snapshot {item['id']}")
            ecs(region, "DeleteSnapshot", SnapshotId=item["id"])
        if ecs(region, "DescribeSnapshots", **options).get("TotalCount") != 0:
            raise RuntimeError(f"Snapshot deletion not confirmed: {item['id']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("record", "cleanup"))
    parser.add_argument("ledger", type=Path)
    parser.add_argument("region")
    args = parser.parse_args()
    if not args.ledger.exists():
        return
    entries = json.loads(args.ledger.read_text())
    if args.action == "record":
        record(entries, args.region)
        args.ledger.write_text(json.dumps(entries, indent=2) + "\n")
    else:
        cleanup(entries, args.region)


if __name__ == "__main__":
    main()
