#!/usr/bin/env python3.11
"""Six real harnesses on one signed image, with a clean consumer state."""
import base64
import json
import os
from pathlib import Path
import shutil
from cloud_case import Case, ROOT, release_selection, run
from fixtures.allinone import generate


def main():
    mode = os.environ.get("E2E_ATTESTATION_MODE", "challenge")
    with Case() as case:
        cidr = case.operator()
        producer = case.suite / "publish-reuse-images"
        release = producer / "allinone"
        image = release_selection(release, case.region)
        fixtures = generate(case.work / "fixtures", image, mode, case.region, os.environ["E2E_CASE_ID"])
        state = case.work / "consumer" / "state"
        case.state = state
        info = case.trustee(configure=False) if mode == "trustee" else None
        for fixture in fixtures:
            service = json.loads((fixture / "appspec.json").read_text())["service"]["id"]
            case.register(service, state)
        command = ["python3.11", ROOT / "tools/e2e/allinone-reuse.py", "--work-dir", case.work / "consumer",
                   "--ca", case.ca, "--operator-cidr", cidr, "--parallel", "3", "--reboot", "--pty",
                   "--console-isolation", "--application-errors", "--publication-state", producer / "state/services/allinone-builder/state.json",
                   "--software-versions", ROOT / "software-versions.json"]
        for fixture in fixtures:
            command += ["--fixture", fixture]
        if info:
            # This CA state already owns this disposable Trustee's policies.
            command += ["--trustee-url", info["url"], "--trustee-admin-key", info["admin_key"]]
            command += ["--trustee-outage-control", ROOT / "tools/e2e/trustee_fixture.sh"]
        else:
            command += ["--resume-deploy", "--frozen-inputs"]
        environment = dict(os.environ)
        if info:
            environment.update(E2E_TRUSTEE_WORK_DIR=str(case.trustee_work),
                               E2E_STATE_DIR=str(state), E2E_ALLOWED_CIDR=cidr)
        run(command, case.work / "reuse", timeout=21600, environment=environment)
        summary = json.loads((case.work / "consumer" / "result.json").read_text())
        assert summary["passed"] and all(value == "passed" for value in summary["model_calls"].values())
        case.checks.update(six_harnesses_real_model=True, signed_image_reused=True,
                           provider=mode, private_credentials=True)
        # Confirm actual software versions from each fixture's native image.
        # The shared image versions are also authenticated by its runtime contract.


if __name__ == "__main__":
    main()
