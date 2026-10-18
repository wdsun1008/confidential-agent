import json
import os
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from rekor_fixture import configure_spec, rekor_url
from render_case import render_matrix_rekor_block, render_rekor_block


class RekorFixtureTest(unittest.TestCase):
    def test_custom_backend_is_explicitly_allowed_for_guest_peer_verification(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, E2E_REKOR_URL="https://isolated.example"):
            path = Path(directory) / "native.yaml"
            original = "build:\n  files:\n    - source: /app\n      target: /app\nattestation:\n  reference_values: rekor\n  rekor:\n    required: true\n"
            path.write_text(original)
            configure_spec(path)
            first = path.read_text()
            configure_spec(path)
            self.assertEqual(path.read_text(), first)
            self.assertIn("target: /app", first)
            self.assertIn("/confidential-agentd.service.d/30-e2e-rekor.conf", first)
            self.assertEqual((path.parent / ".e2e-rekor-trust.conf").read_text(),
                             '[Service]\nEnvironment="CA_TRUSTED_REKOR_URLS=https://rekor.sigstore.dev,https://isolated.example"\n')

    def test_legacy_command_runner_updates_native_init_specs_before_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "native.json"
            path.write_text(json.dumps({"attestation": {"reference_values": "rekor", "rekor": {"required": True, "cosign_key": "/producer/key"}}}))
            check = "import json,sys; r=json.load(open(sys.argv[-1]))['attestation']['rekor']; assert r=={'required':True,'cosign_key':'/producer/key','rekor_url':'https://isolated.example'}"
            environment = dict(os.environ, E2E_WORK_DIR=directory, E2E_REKOR_URL="https://isolated.example")
            subprocess.run([sys.executable, str(Path(__file__).resolve().parents[1] / "command_runner.py"),
                            "build", "--", sys.executable, "-c", check, "--spec", str(path)],
                           env=environment, check=True, timeout=10)

    def test_native_init_spec_preserves_signing_and_required_verification(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, E2E_REKOR_URL="http://192.0.2.1:3000/"):
            path = Path(directory) / "init.yaml"
            original = "attestation:\n  reference_values: rekor\n  rekor:\n    cosign_key: /private/key\n    required: true\n"
            path.write_text(original)
            configure_spec(path)
            self.assertEqual(path.read_text(), original.replace("  rekor:\n", '  rekor:\n    rekor_url: "http://192.0.2.1:3000"\n'))
            configure_spec(path)
            self.assertEqual(path.read_text().count("rekor_url:"), 1)

    def test_json_consumer_remains_keyless_and_sample_stays_sample(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, E2E_REKOR_URL="https://rekor.example"):
            path = Path(directory) / "consumer.json"
            path.write_text(json.dumps({"attestation": {"reference_values": "rekor"}}))
            configure_spec(path)
            self.assertEqual(json.loads(path.read_text())["attestation"]["rekor"], {"rekor_url": "https://rekor.example"})
            path.write_text('{"attestation":{"reference_values":"sample"}}')
            configure_spec(path)
            self.assertEqual(path.read_text(), '{"attestation":{"reference_values":"sample"}}')

    def test_default_does_not_rewrite_generated_specs(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {}, clear=True):
            path = Path(directory) / "app.yaml"
            path.write_text("unmodified fixture\n")
            configure_spec(path)
            self.assertEqual(path.read_text(), "unmodified fixture\n")
            self.assertEqual(rekor_url(), "https://rekor.sigstore.dev")

    def test_legacy_and_matrix_templates_keep_required_real_rekor(self):
        context = {"REFERENCE_VALUES": "rekor", "COSIGN_KEY": "/key", "SLSA_GENERATOR": "/generator",
                   "MATRIX_REAL_REFERENCE_VALUES": "rekor", "MATRIX_REAL_COSIGN_KEY": "/key",
                   "MATRIX_REAL_SLSA_GENERATOR": "/generator", "E2E_REKOR_URL": "https://rekor.example"}
        for render in [render_rekor_block, render_matrix_rekor_block]:
            result = render(context)
            self.assertIn("rekor_url: 'https://rekor.example'", result)
            self.assertIn("required: true", result)
            self.assertIn("cosign_key: '/key'", result)

    def test_rejects_credentials_and_malformed_urls(self):
        for value in ["file:///tmp/rekor", "https://user:secret@rekor.example", "https://rekor.example?key=secret", "https://rekor.example\n"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                rekor_url(value)


if __name__ == "__main__":
    unittest.main()
