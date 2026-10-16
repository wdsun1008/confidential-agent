import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from owned_network_cleanup import cleanup_empty_case_vpc
from cloud_case import Case


class OwnedNetworkCleanupTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.work = Path(self.temporary.name)
        self.state = self.work / 'state'
        directory = self.state / 'services/example/shelter/deploy/owned'
        directory.mkdir(parents=True)
        (directory / 'terraform.tfstate').write_text(json.dumps({'resources': [
            {'mode': 'managed', 'type': 'alicloud_vpc', 'instances': [
                {'attributes': {'id': 'vpc-owned'}}]}]}))
        (directory / '.terraform').mkdir()
        (directory / '.terraform/terraform.tfstate').write_text('{"backend":{"type":"local"}}')
        (directory.parents[2] / 'state.json').write_text('{"phase":"active"}')
        (self.work / 'cloud-resource-ledger.json').write_text(json.dumps([
            {'type': 'alicloud_vpc', 'id': 'vpc-owned', 'service': 'example'}]))
        self.group = {'SecurityGroupId': 'sg-cloud', 'VpcId': 'vpc-owned', 'ServiceManaged': False,
                      'SecurityGroupName': 'CloudSecurityTeamDefaultSecurityGroupName',
                      'Description': 'Created by Alibaba Cloud Security Team. For any questions, join DingTalk group 61280001578.',
                      'CreationTime': '2026-10-15T01:02:00Z'}
        self.actions = []
        self.compute = 0

    def api(self, product, action, **parameters):
        self.actions.append(action)
        if action in ['DescribeInstances', 'DescribeNetworkInterfaces']:
            return {'TotalCount': self.compute}
        if action == 'DescribeVpcs':
            return {'Vpcs': {'Vpc': [{'VpcId': 'vpc-owned', 'CreationTime': '2026-10-15T01:00:00Z'}]}}
        if action == 'DescribeSecurityGroups':
            return ({'TotalCount': 0} if 'SecurityGroupId' in parameters
                    else {'SecurityGroups': {'SecurityGroup': [self.group]}})
        self.assertEqual(action, 'DeleteSecurityGroup')
        self.assertEqual(parameters, {'SecurityGroupId': 'sg-cloud'})
        return {}

    def test_deletes_only_recorded_empty_vpc_cloud_group_and_preserves_evidence(self):
        self.assertTrue(cleanup_empty_case_vpc(self.work, self.state, 'example', self.api))
        self.assertIn('DeleteSecurityGroup', self.actions)
        rows = json.loads((self.work / 'cloud-resource-ledger.json').read_text())
        self.assertEqual(rows[-1]['id'], 'sg-cloud')
        evidence = list(self.work.glob('owned-network-cleanup-*.json'))
        self.assertEqual(len(evidence), 1)
        self.assertTrue(json.loads(evidence[0].read_text())['deleted'])

    def test_preserves_compute_and_unrecognized_or_preexisting_security_groups(self):
        for change in [{'SecurityGroupName': 'user-group'}, {'ServiceManaged': True},
                       {'CreationTime': '2026-10-14T01:02:00Z'}, {'VpcId': 'vpc-foreign'}]:
            with self.subTest(change=change):
                original = dict(self.group)
                self.group.update(change)
                self.actions.clear()
                self.assertFalse(cleanup_empty_case_vpc(self.work, self.state, 'example', self.api))
                self.assertNotIn('DeleteSecurityGroup', self.actions)
                self.group = original
        self.compute = 1
        self.actions.clear()
        self.assertFalse(cleanup_empty_case_vpc(self.work, self.state, 'example', self.api))
        self.assertNotIn('DeleteSecurityGroup', self.actions)
        self.compute = 0
        (self.work / 'cloud-resource-ledger.json').write_text('[]')
        self.actions.clear()
        self.assertFalse(cleanup_empty_case_vpc(self.work, self.state, 'example', self.api))
        self.assertEqual(self.actions, [])

    def test_native_destroy_retries_after_owned_dependency_cleanup_before_trustee_deletion(self):
        with patch.dict('os.environ', {'E2E_WORK_DIR': str(self.work), 'E2E_STATE_DIR': str(self.state)}):
            case = Case()
        case.resources = [{'service': 'example', 'state_dir': str(self.state)}]
        case.trustee_work = self.work / 'trustee'
        case.api = self.api
        with patch.object(case, 'record_resources'), patch.object(case, 'command') as command, \
                patch.object(case, 'trustee_action') as trustee, patch('cloud_case.run'):
            command.side_effect = [RuntimeError('VPC has cloud-created security group'), None]
            self.assertEqual(case.cleanup(), [])
            self.assertEqual(command.call_count, 2)
            self.assertEqual(command.call_args.args[:3], ('destroy-example-network-retry', 'destroy', 'example'))
            trustee.assert_called_once_with('destroy')
            self.assertTrue(case.checks['owned_cloud_security_group_cleanup'])
