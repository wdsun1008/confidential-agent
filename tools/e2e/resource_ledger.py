"""Preserve exact owned Terraform resources before an E2E service is destroyed."""
import json
from pathlib import Path


def read_ledger(path):
    path = Path(path)
    return json.loads(path.read_text()) if path.exists() else []


def merge_ledgers(path, entries):
    path = Path(path)
    rows = read_ledger(path)
    for entry in entries:
        if entry not in rows:
            rows.append(entry)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(rows, indent=2) + "\n")
    temporary.replace(path)


def record_service(state_dir, service, ledger):
    service_dir = Path(state_dir) / "services" / service
    state = service_dir / "state.json"
    deployment = (json.loads(state.read_text()).get("deploy") or {}) if state.exists() else {}
    directory = deployment.get("terraform_dir")
    # An interrupted apply may precede CA's confirmation of terraform_dir.
    # Its provider state still belongs to this service's native Shelter tree.
    directories = set(service_dir.glob("shelter/deploy/*"))
    if directory:
        directories.add(Path(directory))
    # Shelter's local backend uses state/shelter.tfstate by default. Include
    # native backend states and retained operation states within each owned
    # deployment directory, rather than assuming Terraform's flat default.
    states = {path for root in directories if root.is_dir()
              for path in root.rglob("*.tfstate")}
    entries = []
    for terraform in sorted(states):
        for resource in json.loads(terraform.read_text()).get("resources", []):
            # Data sources can refer to shared images and other unowned resources.
            if resource.get("mode") != "managed":
                continue
            for instance in resource.get("instances", []):
                attributes = instance.get("attributes", {})
                if not attributes.get("id"):
                    continue
                entry = {"type": resource["type"], "id": attributes["id"], "service": service}
                for name in ["region", "bucket", "key", "image_id", "system_disk_id", "vpc_id"]:
                    if attributes.get(name):
                        entry[name] = attributes[name]
                entries.append(entry)
    merge_ledgers(ledger, entries)
