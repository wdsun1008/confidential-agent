import copy
import importlib.util
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "cleanup_snapshots", Path(__file__).resolve().parents[1] / "cleanup_snapshots.py"
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class SnapshotCleanupTest(unittest.TestCase):
    def setUp(self):
        self.snapshot = {"SnapshotId": "s-owned", "SnapshotName": "SnapshotForImage-test",
                         "Description": "test", "SourceDiskId": "mock-d-s-owned", "Usage": "none"}
        self.entry = {"type": "alicloud_snapshot", "id": "s-owned", "image_id": "m-owned",
                      "image_name": "test", "region": "test-region", "service": "test"}
        self.present = True
        self.image_present = False
        self.deleted = []

    def api(self, region, action, **options):
        self.assertEqual(region, "test-region")
        if action == "DescribeImages":
            self.assertEqual(options, {"ImageId": "m-owned", "Status": "ALL"})
            return {"TotalCount": int(self.image_present)}
        if action == "DescribeSnapshots":
            self.assertEqual(options, {"SnapshotIds": '["s-owned"]'})
            return {"TotalCount": int(self.present),
                    "Snapshots": {"Snapshot": [self.snapshot] if self.present else []}}
        self.assertEqual(action, "DeleteSnapshot")
        self.assertEqual(options, {"SnapshotId": "s-owned"})  # Never force deletion.
        self.deleted.append(options["SnapshotId"])
        self.present = False
        return {}

    def test_capture_exact_association_and_idempotence(self):
        entries = [{"type": "alicloud_image_import", "id": "m-owned",
                    "region": "test-region", "service": "test"}]
        image = {"ImageId": "m-owned", "ImageName": "test",
                 "DiskDeviceMappings": {"DiskDeviceMapping": [{"SnapshotId": "s-owned"}]}}
        with patch.object(module, "ecs", return_value={"Images": {"Image": [image]}}):
            module.record(entries, "fallback")
            module.record(entries, "fallback")
        self.assertEqual(entries[1:], [self.entry])

    def test_delete_and_verify_then_allow_repeated_cleanup(self):
        with patch.object(module, "ecs", side_effect=self.api):
            module.cleanup([self.entry], "fallback")
            module.cleanup([self.entry], "fallback")
        self.assertEqual(self.deleted, ["s-owned"])

    def test_refuse_live_image_or_used_or_unrelated_snapshot(self):
        pristine = copy.deepcopy(self.snapshot)
        for key, value in [("Usage", "image"), ("Description", "other"),
                           ("SourceDiskId", "d-real"), ("SnapshotName", "other"),
                           ("SnapshotId", "s-other"), (None, None)]:
            with self.subTest(key=key):
                self.snapshot = copy.deepcopy(pristine)
                self.image_present = key is None
                if key:
                    self.snapshot[key] = value
                with patch.object(module, "ecs", side_effect=self.api):
                    with self.assertRaises(RuntimeError):
                        module.cleanup([self.entry], "fallback")
                self.assertFalse(self.deleted)

    def test_missing_count_does_not_claim_absence(self):
        with patch.object(module, "ecs", return_value={}):
            with self.assertRaises(RuntimeError):
                module.cleanup([self.entry], "fallback")


if __name__ == "__main__":
    unittest.main()
