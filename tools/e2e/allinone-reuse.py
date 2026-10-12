#!/usr/bin/env python3.11
"""Exercise published-image consumers from private JSON AppSpec fixtures."""
import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import signal
import shlex
import subprocess
import time
import urllib.error
import urllib.request


def run(command, evidence, timeout=1800, success=True):
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        stdout, stderr = process.communicate()
        evidence.with_suffix(".out").write_bytes(stdout)
        evidence.with_suffix(".err").write_bytes(stderr)
        if not success:
            return subprocess.CompletedProcess(command, 124, stdout, stderr)
        raise RuntimeError(f"{evidence.name} timed out; see private evidence") from None
    result = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    evidence.with_suffix(".out").write_bytes(result.stdout)
    evidence.with_suffix(".err").write_bytes(result.stderr)
    if success and result.returncode:
        raise RuntimeError(f"{evidence.name} failed; see private evidence")
    return result


class Consumer:
    def __init__(self, fixture, work, ca, cidr, shared_state=None):
        self.spec_path = fixture / "appspec.json"
        self.spec = json.loads(self.spec_path.read_text())
        self.service = self.spec["service"]["id"]
        self.work = work / self.service
        self.work.mkdir(mode=0o700)
        self.state = shared_state or self.work / "state"
        self.ca = [ca, "--state-dir", str(self.state)]
        self.probe = json.loads((fixture / "probe.json").read_text())
        self.cidr = cidr
        self.attempted = False

    def command(self, label, *arguments, **kwargs):
        return run(self.ca + list(arguments), self.work / label, **kwargs)

    def ssh(self, label, *arguments, **kwargs):
        return self.command(label, "ssh", self.service, "--", "-o", "BatchMode=yes",
                            "-o", "ConnectTimeout=10", *arguments, **kwargs)

    @property
    def directory(self):
        return self.state / "services" / self.service

    def deploy(self):
        self.command("validate", "spec", "validate", "--spec", str(self.spec_path))
        peering = self.command("peering-show", "peering", "show", "reuse-test", success=False)
        if peering.returncode:
            self.command("peering", "peering", "add", "--role", "operator",
                         "--cidr", self.cidr, "--label", "reuse-test")
        else:
            assert f"cidr: {self.cidr}".encode() in peering.stdout
        self.attempted = True
        self.command("deploy", "deploy", "--spec", str(self.spec_path), timeout=3600)
        deadline = time.monotonic() + 600
        while time.monotonic() < deadline:
            if self.ssh("wait-ssh", "true", success=False, timeout=20).returncode == 0:
                break
            time.sleep(5)
        else:
            raise RuntimeError(f"{self.service}: authenticated SSH unavailable")
        self.ssh("runtime", "systemctl is-active cai-allinone.service cai-ssh.service")
        self.ssh("single-harness", "! systemctl is-enabled --quiet "
                 "cai-codex-app-server.service cai-hermes-agent.service sshd.service sshd.socket")
        self.ssh("version", "cai-agent --version")
        self.ssh("disk-protection", "set -eu\n"
                 "dmsetup status --target crypt | grep -w crypt\n"
                 "dmsetup status --target verity | grep -w verity")
        self.ssh("private-resource", "test $(stat -c %a /opt/data/allinone/runtime.json) = 600")
        for index, resource in enumerate(self.spec["resources"].values()):
            digest = hashlib.sha256(Path(resource["source"]).read_bytes()).hexdigest()
            result = self.ssh(f"resource-{index}", f"sha256sum {shlex.quote(resource['target'])}")
            assert result.stdout.split()[0].decode() == digest, "delivered resource differs"
        if self.probe.get("runtime_command"):
            deadline = time.monotonic() + 180
            while self.ssh("native-runtime", self.probe["runtime_command"],
                           success=False, timeout=30).returncode:
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"{self.service}: native runtime unavailable")
                time.sleep(5)
        deadline = time.monotonic() + 180
        while True:
            report = json.loads(self.command("report", "report", "--service", self.service,
                                            "--json").stdout)
            service = next(item for item in report["services"] if item["service_id"] == self.service)
            if service.get("daemon", {}).get("app_ready") and service["daemon"].get("mesh_ready"):
                break
            if time.monotonic() >= deadline:
                raise RuntimeError(f"{self.service}: application readiness did not settle")
            time.sleep(5)
        assert report["schema"] == "confidential-agent/attestation-report/v1"
        assert service["collect_status"] == "ok"
        assert service["tee_info"]["status"] == "ok" and service["tee_info"]["tee"] == "tdx"
        assert service["attestation"]["status"] == "ok" and service["attestation"]["ear_jwt"]
        assert service["daemon"]["app_ready"] and service["daemon"]["mesh_ready"]
        manifest = json.loads((self.directory / "manifest.json").read_text())
        assert manifest["image"]["image_id"]
        assert manifest["ssh"]
        assert not list(self.state.rglob("*.qcow2")), "consumer staged a local image"
        if self.probe.get("connect_http_path"):
            self.verify_connect()
        if self.probe.get("command"):
            result = self.ssh("model", self.probe["command"], timeout=600)
            assert self.probe["expect"].encode() in result.stdout, "model response missing marker"
        else:
            (self.work / "model-skipped.json").write_text(json.dumps({
                "reason": self.probe["skip_reason"]
            }) + "\n")

    def verify_connect(self):
        ready = self.work / "connect-ready.json"
        try:
            self.command("connect-start", "connect", "start", "--service", self.service,
                         "--ready-json", str(ready), "--wait-ready", "180",
                         "--log-file", str(self.work / "connect.log"), timeout=600)
            endpoints = json.loads(ready.read_text())["client_endpoints"]
            assert len(endpoints) == 1
            endpoint = endpoints[0]
            assert endpoint["local_host"] == "127.0.0.1"
            url = endpoint["http_base_url"] + self.probe["connect_http_path"]
            deadline = time.monotonic() + 180
            while True:
                try:
                    with urllib.request.urlopen(url, timeout=15) as response:
                        assert response.status == 200
                        (self.work / "connect-health.json").write_bytes(response.read())
                    break
                except (urllib.error.URLError, TimeoutError, ConnectionError):
                    if time.monotonic() >= deadline:
                        raise RuntimeError(f"{self.service}: connected HTTP health unavailable") from None
                    time.sleep(5)
        except Exception:
            if ready.exists():
                container = json.loads(ready.read_text())["container_name"]
                run(["docker", "logs", "--tail", "200", container],
                    self.work / "connect-diagnostics", success=False, timeout=30)
            raise
        finally:
            if ready.exists():
                self.command("connect-stop", "connect", "stop", "--ready-json", str(ready))

    def destroy(self):
        if not self.attempted:
            return
        for attempt in range(3):
            result = self.command(f"destroy-{attempt}", "destroy", self.service,
                                  success=False, timeout=1800)
            if result.returncode == 0:
                return
        raise RuntimeError(f"{self.service}: cleanup failed; see private state")


def reject_invalid_release(consumer):
    spec = consumer.spec
    altered_release = consumer.work / "altered-release.json"
    envelope = json.loads(Path(spec["deploy"]["image"]["release"]).read_text())
    signature = envelope["signatures"][0]["sig"]
    envelope["signatures"][0]["sig"] = ("A" if signature[0] != "A" else "B") + signature[1:]
    altered_release.write_text(json.dumps(envelope))
    wrong_key = consumer.work / "wrong-publisher.key"
    wrong_public = consumer.work / "wrong-publisher.pem"
    run(["openssl", "genpkey", "-algorithm", "ED25519", "-out", str(wrong_key)],
        consumer.work / "wrong-publisher-key")
    run(["openssl", "pkey", "-in", str(wrong_key), "-pubout", "-out", str(wrong_public)],
        consumer.work / "wrong-publisher-public")
    for field, wrong in [("manifest", "sha256:" + "0" * 64), ("account_id", "0"),
                         ("release", str(altered_release)), ("trusted_key", str(wrong_public))]:
        invalid = json.loads(json.dumps(spec))
        invalid["service"]["id"] += "-invalid-" + field.replace("_", "-")
        invalid["deploy"]["image"][field] = wrong
        path = consumer.spec_path.parent / f"invalid-{field}.json"
        path.write_text(json.dumps(invalid))
        result = consumer.command(f"reject-{field}", "deploy", "--spec", str(path),
                                  "--render-only", success=False)
        assert result.returncode != 0, f"accepted invalid {field}"
        assert b"authentication failed" in result.stderr, "failed outside release verification"
        rejected = consumer.state / "services" / invalid["service"]["id"]
        assert not (rejected / "state.json").exists()
        assert not (rejected / "bootstrap.json").exists()


def assert_independent(consumers):
    for name in ["ssh_client", "ssh_host", "disk_passphrase"]:
        values = [(item.directory / "secrets" / name).read_bytes() for item in consumers]
        assert len(set(values)) == len(values), f"consumers share {name}"
    identities = [json.loads((item.directory / "state.json").read_text())
                  ["gateway_identity"]["public_key"] for item in consumers]
    assert len(set(identities)) == len(identities), "consumers share a gateway identity"
    first, second = consumers[:2]
    state = json.loads((second.directory / "state.json").read_text())
    host = state["deploy"]["public_ip"]
    manifest = json.loads((second.directory / "manifest.json").read_text())
    identity = manifest["ssh"]
    wrong_client = run([
        "ssh", "-i", str(first.directory / "secrets/ssh_client"),
        "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
        "-o", "StrictHostKeyChecking=yes", "-o", f"HostKeyAlias={identity['host_alias']}",
        "-o", f"UserKnownHostsFile={identity['known_hosts']}", f"root@{host}", "true"
    ], second.work / "reject-other-client", success=False, timeout=30)
    assert wrong_client.returncode != 0 and b"Permission denied" in wrong_client.stderr
    wrong_hosts = second.work / "wrong-known-hosts"
    wrong_hosts.write_text(identity["host_alias"] + " " +
                           (first.directory / "secrets/ssh_host.pub").read_text())
    wrong_host = run([
        "ssh", "-i", identity["private_key"], "-o", "IdentitiesOnly=yes",
        "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "StrictHostKeyChecking=yes",
        "-o", f"HostKeyAlias={identity['host_alias']}",
        "-o", f"UserKnownHostsFile={wrong_hosts}", f"root@{host}", "true"
    ], second.work / "reject-other-host", success=False, timeout=30)
    assert wrong_host.returncode != 0 and b"Host key verification failed" in wrong_host.stderr


def reboot(consumer, aliyun):
    before = consumer.ssh("boot-before", "cat /proc/sys/kernel/random/boot_id").stdout.strip()
    consumer.ssh("persistent-write", "printf REUSE_PERSISTENT > /opt/data/allinone/reboot-marker; sync")
    state = json.loads((consumer.directory / "state.json").read_text())
    cloud = state["deploy"]
    region = consumer.spec["deploy"]["region"]
    run([aliyun, "--region", region, "ecs", "RebootInstance", "--RegionId", region,
         "--InstanceId", cloud["instance_id"], "--ForceStop", "true"],
        consumer.work / "force-reboot")
    time.sleep(30)
    deadline = time.monotonic() + 600
    while time.monotonic() < deadline:
        if consumer.spec["attestation"]["mode"] == "challenge":
            consumer.command("reinject", "inject", "--spec", str(consumer.spec_path),
                             "--target-ip", cloud["public_ip"], success=False, timeout=60)
        result = consumer.ssh("boot-after", "cat /proc/sys/kernel/random/boot_id",
                              success=False, timeout=20)
        if result.returncode == 0 and result.stdout.strip() != before:
            consumer.ssh("persistent-after", "grep -Fx REUSE_PERSISTENT /opt/data/allinone/reboot-marker")
            consumer.ssh("runtime-after", "systemctl is-active cai-allinone.service cai-ssh.service")
            return
        time.sleep(10)
    raise RuntimeError(f"{consumer.service}: reboot recovery failed")


def trustee_outage(consumer, aliyun, unit):
    """A disposable local Trustee must gate cold-boot key release."""
    before = consumer.ssh("outage-boot-before", "cat /proc/sys/kernel/random/boot_id").stdout.strip()
    cloud = json.loads((consumer.directory / "state.json").read_text())["deploy"]
    region = consumer.spec["deploy"]["region"]
    run(["systemctl", "stop", unit], consumer.work / "trustee-stop")
    try:
        run([aliyun, "--region", region, "ecs", "RebootInstance", "--RegionId", region,
             "--InstanceId", cloud["instance_id"], "--ForceStop", "true"],
            consumer.work / "outage-reboot")
        time.sleep(30)
        for attempt in range(8):
            assert consumer.ssh(f"outage-ssh-{attempt}", "true", success=False,
                                timeout=20).returncode != 0, "boot bypassed unavailable Trustee"
            time.sleep(15)
        output = run([aliyun, "--region", region, "ecs", "GetInstanceConsoleOutput", "--RegionId", region,
                      "--InstanceId", cloud["instance_id"]], consumer.work / "outage-console")
        console = base64.b64decode(json.loads(output.stdout)["ConsoleOutput"]).decode(errors="replace")
        (consumer.work / "outage-console.txt").write_text(console)
        assert "trustee" in console.lower() and "failed" in console.lower(), "missing key-release failure evidence"
        instances = json.loads(run([
            aliyun, "--region", region, "ecs", "DescribeInstances", "--RegionId", region,
            "--InstanceIds", json.dumps([cloud["instance_id"]])
        ], consumer.work / "outage-before-stop").stdout)["Instances"]["Instance"]
        assert len(instances) == 1
        if instances[0]["Status"] == "Running":
            run([aliyun, "--region", region, "ecs", "StopInstance", "--RegionId", region,
                 "--InstanceId", cloud["instance_id"], "--ForceStop", "true",
                 "--StoppedMode", "KeepCharging"], consumer.work / "outage-force-stop")
    finally:
        run(["systemctl", "start", unit], consumer.work / "trustee-start")
    deadline = time.monotonic() + 600
    attempt = 0
    while time.monotonic() < deadline:
        # The guest powers off when key release fails. ECS can finish that
        # transition after Trustee returns, so observe it throughout recovery.
        instances = json.loads(run([
            aliyun, "--region", region, "ecs", "DescribeInstances", "--RegionId", region,
            "--InstanceIds", json.dumps([cloud["instance_id"]])
        ], consumer.work / f"outage-instance-status-{attempt}").stdout)["Instances"]["Instance"]
        assert len(instances) == 1
        if instances[0]["Status"] == "Stopped":
            run([aliyun, "--region", region, "ecs", "StartInstance", "--RegionId", region,
                 "--InstanceId", cloud["instance_id"]],
                consumer.work / f"outage-start-instance-{attempt}")
        attempt += 1
        result = consumer.ssh("outage-boot-after", "cat /proc/sys/kernel/random/boot_id",
                              success=False, timeout=20)
        if result.returncode == 0 and result.stdout.strip() != before:
            consumer.ssh("outage-runtime-after", "systemctl is-active cai-allinone.service cai-ssh.service")
            return
        time.sleep(10)
    raise RuntimeError(f"{consumer.service}: Trustee recovery failed without reinjection")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", type=Path, action="append", required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--operator-cidr", required=True)
    parser.add_argument("--ca", default="confidential-agent")
    parser.add_argument("--aliyun", default="aliyun")
    parser.add_argument("--parallel", type=int, choices=range(1, 7), default=3)
    parser.add_argument("--reboot", action="store_true",
                        help="Force-reboot one consumer and verify encrypted data survives")
    parser.add_argument("--trustee-url", help="Guest-visible URL for a disposable Trustee")
    parser.add_argument("--trustee-management-url")
    parser.add_argument("--trustee-admin-key", type=Path)
    parser.add_argument("--trustee-outage-service",
                        help="Stop this disposable local Trustee unit during a cold-boot test")
    args = parser.parse_args()
    if bool(args.trustee_url) != bool(args.trustee_admin_key):
        parser.error("--trustee-url and --trustee-admin-key must be supplied together")
    if args.trustee_outage_service and not args.trustee_url:
        parser.error("--trustee-outage-service requires Trustee fixtures")
    os.umask(0o077)
    args.work_dir = args.work_dir.resolve()
    args.work_dir.mkdir(mode=0o700)
    shared_state = args.work_dir / "state" if args.trustee_url else None
    consumers = [Consumer(path.resolve(), args.work_dir, args.ca, args.operator_cidr, shared_state)
                 for path in args.fixture]
    assert len(consumers) >= 2, "reuse requires at least two consumers"
    assert len({item.service for item in consumers}) == len(consumers)
    pins = [item.spec["deploy"]["image"] for item in consumers]
    assert all(pin == pins[0] for pin in pins), "fixtures must select the same release"
    assert all("build" not in item.spec and item.spec["service"]["ssh"] for item in consumers)
    if args.trustee_url:
        assert all(item.spec["attestation"]["mode"] == "trustee" for item in consumers)
        configure = ["trustee", "configure", "--url", args.trustee_url,
                     "--admin-key", str(args.trustee_admin_key.resolve())]
        if args.trustee_management_url:
            configure += ["--management-url", args.trustee_management_url]
        first = consumers[0]
        first.command("trustee-configure", *configure)
        doctor = json.loads(first.command("trustee-doctor", "trustee", "doctor", "--json").stdout)
        first.command("trustee-adopt", "trustee", "adopt", "--attestation-policy-sha256",
                      doctor["attestation_policy_sha256"], "--resource-policy-sha256",
                      doctor["resource_policy_sha256"])
        # One CA state owns the shared Trustee policies; serialize its updates.
        args.parallel = 1
    else:
        assert all(item.spec["attestation"]["mode"] == "challenge" for item in consumers)
    try:
        reject_invalid_release(consumers[0])
        with ThreadPoolExecutor(max_workers=args.parallel) as pool:
            list(pool.map(lambda item: item.deploy(), consumers))
        assert_independent(consumers)
        if args.reboot:
            reboot(consumers[0], args.aliyun)
            for consumer in consumers[1:]:
                consumer.ssh("independent-data", "test ! -e /opt/data/allinone/reboot-marker")
        if args.trustee_outage_service:
            trustee_outage(consumers[0], args.aliyun, args.trustee_outage_service)
        first, *remaining = consumers
        first.destroy()
        first.attempted = False
        for item in remaining:
            item.ssh("survives-other-destroy", "cai-agent --version")
    finally:
        cleanup_errors = []
        for item in consumers:
            try:
                item.destroy()
            except Exception as error:
                cleanup_errors.append(str(error))
        if cleanup_errors:
            raise RuntimeError("; ".join(cleanup_errors))
    image = json.loads((consumers[0].directory / "manifest.json").read_text())["image"]
    region = consumers[0].spec["deploy"]["region"]
    result = run([args.aliyun, "--region", region, "ecs", "DescribeImages", "--RegionId",
                  region, "--ImageId", image["image_id"]],
                 args.work_dir / "shared-image-preserved")
    assert len(json.loads(result.stdout)["Images"]["Image"]) == 1
    (args.work_dir / "result.json").write_text(json.dumps({
        "passed": True, "consumers": [item.service for item in consumers],
        "shared_image_preserved": True,
        "trustee_outage": bool(args.trustee_outage_service),
        "model_calls": {item.service: "passed" if item.probe.get("command")
                        else {"skipped": item.probe["skip_reason"]} for item in consumers}
    }) + "\n")
    print("Shared-image consumer tests passed; private evidence:", args.work_dir)


if __name__ == "__main__":
    main()
