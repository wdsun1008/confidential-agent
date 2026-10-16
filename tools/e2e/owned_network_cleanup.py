"""Remove a cloud-created default group only from an otherwise empty case VPC."""
import json
from pathlib import Path
import time

from resource_ledger import merge_ledgers, read_ledger


def cleanup_empty_case_vpc(work, state_dir, service, api):
    work = Path(work)
    shelter = Path(state_dir) / 'services' / service / 'shelter'
    states = [p for p in shelter.rglob('*.tfstate')
              if p.name in ['terraform.tfstate', 'shelter.tfstate']
              and '.terraform' not in p.relative_to(shelter).parts]
    if len(states) != 1:
        return False
    resources = [(resource, instance.get('attributes') or {})
                 for resource in json.loads(states[0].read_text()).get('resources', [])
                 if resource.get('mode') == 'managed'
                 for instance in resource.get('instances', [])]
    if len(resources) != 1 or resources[0][0]['type'] != 'alicloud_vpc':
        return False
    vpc = resources[0][1]['id']
    ledger = work / 'cloud-resource-ledger.json'
    if not any(row['type'] == 'alicloud_vpc' and row['id'] == vpc
               and row.get('service') == service for row in read_ledger(ledger)):
        return False
    if api('ecs', 'DescribeInstances', VpcId=vpc).get('TotalCount') != 0:
        return False
    if api('ecs', 'DescribeNetworkInterfaces', VpcId=vpc).get('TotalCount') != 0:
        return False
    vpcs = api('vpc', 'DescribeVpcs', VpcId=vpc)['Vpcs']['Vpc']
    groups = api('ecs', 'DescribeSecurityGroups', VpcId=vpc)['SecurityGroups']['SecurityGroup']
    if len(vpcs) != 1 or vpcs[0]['VpcId'] != vpc or len(groups) != 1:
        return False
    group = groups[0]
    if (group.get('VpcId') != vpc or group.get('ServiceManaged') is not False
            or group.get('SecurityGroupName') != 'CloudSecurityTeamDefaultSecurityGroupName'
            or group.get('Description') != 'Created by Alibaba Cloud Security Team. For any questions, join DingTalk group 61280001578.'
            or not group.get('CreationTime') or not vpcs[0].get('CreationTime')
            or group['CreationTime'] < vpcs[0]['CreationTime']):
        return False
    record = {'type': 'alicloud_security_group', 'id': group['SecurityGroupId'],
              'service': service, 'vpc_id': vpc,
              'ownership': 'cloud-created group in otherwise empty case-owned VPC'}
    merge_ledgers(ledger, [record])
    evidence = work / ('owned-network-cleanup-' + str(time.time_ns()) + '.json')
    evidence.write_text(json.dumps({'resource': record, 'compute_and_interfaces_absent': True,
                                   'saved_state': str(states[0]), 'deleted': False}, indent=2) + '\n')
    api('ecs', 'DeleteSecurityGroup', SecurityGroupId=record['id'])
    if api('ecs', 'DescribeSecurityGroups', SecurityGroupId=record['id']).get('TotalCount') != 0:
        raise RuntimeError('cloud-created security group deletion is not confirmed')
    evidence.write_text(json.dumps({'resource': record, 'compute_and_interfaces_absent': True,
                                   'saved_state': str(states[0]), 'deleted': True}, indent=2) + '\n')
    return True
