import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[3]
FLOW = ROOT / 'tools/e2e/cases/openclaw-vllm/flow.sh'


class LegacyVllmReleaseTest(unittest.TestCase):
    def probe(self, status, variant='release'):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            (work / 'status-live.json').write_text(json.dumps(status))
            environment = dict(os.environ, WORK_DIR=directory, STATE_DIR=directory,
                               TEST_VARIANT=variant, FLOW=str(FLOW))
            result = subprocess.run(['bash', '-c', '''
set -euo pipefail
source "$FLOW"
wait_status_json_ready() { [[ "$1" == 0 && "$2" == 7200 ]]; }
state_value() { printf '%s' "$TEST_VARIANT"; }
record() { :; }
ssh_guest() { touch "$WORK_DIR/unexpected-ssh"; return 99; }
wait_for_ssh() { touch "$WORK_DIR/unexpected-ssh"; return 99; }
verify_release_readiness
'''], env=environment, capture_output=True, text=True)
            self.assertFalse((work / 'unexpected-ssh').exists())
            return result.returncode

    def test_release_ready_without_debug_ssh(self):
        self.assertEqual(self.probe([{'daemon': {
            'mesh_ready': True, 'app_ready': True, 'debug_ssh_ready': False}}]), 0)

    def test_wrong_variant_or_incomplete_live_status_is_rejected(self):
        ready = {'daemon': {'mesh_ready': True, 'app_ready': True}}
        for status, variant in [([], 'release'), ([ready, ready], 'release'),
                                ([ready], 'debug'), ([{'daemon': {
                                    'mesh_ready': True, 'app_ready': True,
                                    'debug_ssh_ready': True}}], 'release'),
                                ([{'daemon': {'mesh_ready': True}}], 'release')]:
            with self.subTest(status=status, variant=variant):
                self.assertNotEqual(self.probe(status, variant), 0)

    def test_release_entrypoint_overrides_suite_build_both_default(self):
        result = subprocess.run(['bash', '-c', '''
set -euo pipefail
source "$ROOT_DIR/tools/e2e/cases/openclaw-vllm-release/flow.sh"
test "$E2E_OPENCLAW_VLLM_BUILD_VARIANTS" = release
'''], env=dict(os.environ, ROOT_DIR=str(ROOT),
               E2E_OPENCLAW_VLLM_BUILD_VARIANTS='release,debug'), capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_standalone_gpu_target_overrides_cpu_defaults_and_rejects_cpu(self):
        environment = dict(os.environ, FLOW=str(FLOW), ZONE_ID='cn-beijing-i',
                           E2E_INSTANCE_TYPE='ecs.g9i.xlarge')
        environment.pop('E2E_GPU_INSTANCE_TYPE', None)
        environment.pop('E2E_GPU_ZONE_ID', None)
        command = ['bash', '-c', '''
set -euo pipefail
source "$FLOW"
configure_openclaw_vllm_target
printf '%s %s' "$INSTANCE_TYPE" "$ZONE_ID"
''']
        rejected = subprocess.run(command, env=environment, capture_output=True, text=True)
        self.assertNotEqual(rejected.returncode, 0)
        accepted = subprocess.run(command, env=dict(environment,
            E2E_GPU_INSTANCE_TYPE='ecs.gn8v-tee.4xlarge', E2E_GPU_ZONE_ID='cn-beijing-l'),
            capture_output=True, text=True)
        self.assertEqual(accepted.returncode, 0, accepted.stderr)
        self.assertEqual(accepted.stdout, 'ecs.gn8v-tee.4xlarge cn-beijing-l')


if __name__ == '__main__':
    unittest.main()
