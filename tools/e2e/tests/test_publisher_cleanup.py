import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import publish_fixtures


class PublisherCleanupTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        service = root / 'state/services/builder'
        service.mkdir(parents=True)
        (service / 'state.json').write_text(json.dumps({
            'service_id': 'builder', 'build': {'published': {'release': {
                'image_id': 'm-owned', 'region': 'test-region', 'variant': 'release',
            }}},
        }))
        (root / 'case-result.json').write_text(json.dumps({'passed': False, 'cleanup_complete': False}))
        self.events = []
        self.case = SimpleNamespace(work=root, state=root / 'state', region='test-region',
                                    command=Mock(side_effect=lambda *args, **kwargs: self.events.append('unpublish')))

    def result(self):
        return json.loads((self.case.work / 'case-result.json').read_text())

    def snapshot_command(self, command, *args, **kwargs):
        action = command[2]
        self.events.append(action)
        ledger = json.loads((self.case.work / 'cloud-resource-ledger.json').read_text())
        self.assertIn({'type': 'alicloud_image_import', 'id': 'm-owned',
                       'region': 'test-region', 'service': 'builder'}, ledger)

    def test_snapshot_inventory_precedes_unpublish_and_cleanup_does_not_promote_failure(self):
        with patch.object(publish_fixtures, 'run', side_effect=self.snapshot_command):
            publish_fixtures.cleanup(self.case)
        self.assertEqual(self.events, ['record', 'unpublish', 'cleanup'])
        self.assertTrue(self.result()['cleanup_complete'])
        self.assertFalse(self.result()['passed'])

    def test_failed_inventory_keeps_images_available_for_a_cleanup_retry(self):
        with patch.object(publish_fixtures, 'run', side_effect=RuntimeError('inventory unavailable')):
            with self.assertRaisesRegex(RuntimeError, 'publication cleanup failed'):
                publish_fixtures.cleanup(self.case)
        self.case.command.assert_not_called()
        self.assertFalse(self.result()['cleanup_complete'])

    def test_snapshot_cleanup_failure_is_not_reported_as_success(self):
        def run(command, *args, **kwargs):
            self.snapshot_command(command)
            if command[2] == 'cleanup':
                raise RuntimeError('snapshot still exists')
        with patch.object(publish_fixtures, 'run', side_effect=run):
            with self.assertRaises(RuntimeError):
                publish_fixtures.cleanup(self.case)
        self.assertEqual(self.events, ['record', 'unpublish', 'cleanup'])
        self.assertFalse(self.result()['cleanup_complete'])
        self.assertTrue((self.case.work / 'cloud-resource-ledger.json').is_file())


if __name__ == '__main__':
    unittest.main()
