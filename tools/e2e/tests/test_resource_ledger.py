import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from cloud_case import Case
from resource_ledger import merge_ledgers, read_ledger, record_service


class ResourceLedgerTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.state = self.root / 'state'

    def provider_state(self, service, confirmed=True, filename='state/shelter.tfstate'):
        directory = self.state / 'services' / service
        terraform = directory / 'shelter/deploy/owned'
        terraform.mkdir(parents=True)
        (directory / 'state.json').write_text(json.dumps({'deploy': {
            'terraform_dir': str(terraform) if confirmed else None}}))
        path = terraform / filename
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({'resources': [
            {'mode': 'managed', 'type': 'alicloud_instance',
             'instances': [{'attributes': {'id': 'i-' + service}}]},
            {'mode': 'data', 'type': 'alicloud_image',
             'instances': [{'attributes': {'id': 'm-shared'}}]},
        ]}))
        return path

    def test_early_destroy_and_later_inventory_preserve_both_services_and_snapshots(self):
        first = self.provider_state('first')
        self.provider_state('second')
        ledger = self.root / 'cloud-resource-ledger.json'
        snapshot = {'type': 'alicloud_snapshot', 'id': 's-owned', 'image_id': 'm-owned'}
        merge_ledgers(ledger, [snapshot])
        record_service(self.state, 'first', ledger)
        first.write_text('{"resources": []}')
        record_service(self.state, 'first', ledger)
        record_service(self.state, 'second', ledger)
        rows = read_ledger(ledger)
        self.assertEqual({row['id'] for row in rows}, {'i-first', 'i-second', 's-owned'})
        self.assertEqual(len(rows), 3)
        self.assertIn(snapshot, rows)

    def test_interrupted_apply_records_owned_provider_state_before_ca_confirmation(self):
        self.provider_state('interrupted', confirmed=False)
        ledger = self.root / 'ledger.json'
        record_service(self.state, 'interrupted', ledger)
        self.assertEqual(read_ledger(ledger), [
            {'type': 'alicloud_instance', 'id': 'i-interrupted', 'service': 'interrupted'}])

    def test_flat_terraform_state_remains_supported(self):
        self.provider_state('flat', filename='terraform.tfstate')
        ledger = self.root / 'ledger.json'
        record_service(self.state, 'flat', ledger)
        self.assertEqual(read_ledger(ledger), [
            {'type': 'alicloud_instance', 'id': 'i-flat', 'service': 'flat'}])

    def test_parent_case_merges_nested_consumer_evidence_after_child_destroy(self):
        provider = self.provider_state('nested')
        nested = self.root / 'consumer/nested/cloud-resource-ledger.json'
        record_service(self.state, 'nested', nested)
        provider.write_text('{"resources": []}')
        with patch.dict(os.environ, {'E2E_WORK_DIR': str(self.root), 'E2E_STATE_DIR': str(self.state)}):
            case = Case()
        case.register('nested')
        with patch('cloud_case.run'):
            case.record_resources()
            case.record_resources()
        self.assertEqual(read_ledger(self.root / 'cloud-resource-ledger.json'), read_ledger(nested))
        self.assertEqual(read_ledger(nested), [
            {'type': 'alicloud_instance', 'id': 'i-nested', 'service': 'nested'}])


if __name__ == '__main__':
    unittest.main()
