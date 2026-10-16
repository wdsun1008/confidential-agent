import json
from pathlib import Path
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from inference_case import start_stopped_instance


class ReplicaRecoveryTests(unittest.TestCase):
    def test_start_waits_for_cloud_stop_completion_and_preserves_api_failure(self):
        for fail_start in [False, True]:
            with self.subTest(fail_start=fail_start):
                class Cloud:
                    def __init__(self):
                        self.statuses = iter(["Stopping", "Stopping", "Stopped"])
                        self.events = []
                        self.status = None

                    def api(self, product, action, **parameters):
                        assert product == "ecs"
                        if action == "DescribeInstances":
                            assert json.loads(parameters["InstanceIds"]) == ["i-owned"]
                            self.status = next(self.statuses)
                            self.events.append(self.status)
                            return {"Instances": {"Instance": [{"Status": self.status}]}}
                        assert action == "StartInstance" and parameters == {"InstanceId": "i-owned"}
                        assert self.status == "Stopped", "start was issued before ECS completed stopping"
                        self.events.append("start")
                        if fail_start:
                            raise RuntimeError("original cloud API failure")
                        return {}

                cloud = Cloud()
                with mock.patch("cloud_case.time.sleep"):
                    if fail_start:
                        with self.assertRaisesRegex(RuntimeError, "original cloud API failure"):
                            start_stopped_instance(cloud, "i-owned")
                    else:
                        start_stopped_instance(cloud, "i-owned")
                self.assertEqual(cloud.events, ["Stopping", "Stopping", "Stopped", "start"])


if __name__ == "__main__":
    unittest.main()
