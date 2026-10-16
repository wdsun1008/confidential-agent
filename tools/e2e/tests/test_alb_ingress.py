import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from inference_case import validate_lb_ingress


class LoadBalancerIngressTests(unittest.TestCase):
    def test_cloud_policy_requires_owned_attachment_and_effective_default_deny(self):
        for kind in ["alb", "nlb"]:
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory:
                self.check_policy(Path(directory), kind)

    def check_policy(self, root, lb_kind):
        resources = [{"mode": "managed", "type": kind, "name": name,
                      "instances": [{"attributes": {"id": identity}}]}
                     for kind, name, identity in [("alicloud_security_group", lb_kind + "_ingress", "sg-owned"),
                                                  ("alicloud_" + lb_kind + "_load_balancer", "shelter", lb_kind + "-owned")]]
        (root / "shelter.tfstate").write_text(json.dumps({"resources": resources}))

        class Fixture:
            work = root
            operator_cidr = "192.0.2.1/32"

            def __init__(self):
                self.checks = {}
                self.attached = ["sg-owned"] if lb_kind == "nlb" else ["sg-system", "sg-owned"]
                self.rules = [
                    {"Policy": "Drop", "Priority": "100", "IpProtocol": "ALL", "PortRange": "-1/-1", "SourceCidrIp": "0.0.0.0/0"},
                    {"Policy": "Accept", "Priority": "1", "IpProtocol": "TCP", "PortRange": "8080/8080", "SourceCidrIp": self.operator_cidr}]

            def local_state(self, service):
                return {"deploy": {"terraform_dir": str(root)}}

            def api(self, product, action, **parameters):
                if product == lb_kind:
                    return {"SecurityGroupIds": self.attached}
                return {"Permissions": {"Permission": copy.deepcopy(self.rules)}}

        case = Fixture()
        validate_lb_ingress(case, "inference", lb_kind)
        self.assertTrue(case.checks[lb_kind + "_owned_ingress_default_deny"])
        defects = ["system-only", "allow-only", "public-allow", "ineffective-allow-priority"]
        if lb_kind == "nlb":
            defects.append("extra-attached-group")
        for defect in defects:
            case = Fixture()
            if defect == "system-only":
                case.attached = ["sg-system"]
            elif defect == "allow-only":
                case.rules.pop(0)
            elif defect == "public-allow":
                case.rules[1]["SourceCidrIp"] = "0.0.0.0/0"
            elif defect == "ineffective-allow-priority":
                case.rules[1]["Priority"] = "100"
            else:
                case.attached.append("sg-unexpected")
            with self.subTest(defect=defect), self.assertRaises(AssertionError):
                validate_lb_ingress(case, "inference", lb_kind)


if __name__ == "__main__":
    unittest.main()
