"""Check the image configuration and selected process without installing agents."""
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
RUNTIME = ROOT / "examples/allinone/files/cai-agent"


class AllinoneRuntimeTest(unittest.TestCase):
    @unittest.skipUnless(shutil.which("systemd-analyze"), "requires the native systemd unit verifier")
    def test_management_path_does_not_order_after_a_normal_service(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            unit = root / "cai-ssh.path"
            source = (ROOT / "examples/allinone/files/cai-ssh.path").read_text()
            unit.write_text(source)
            # Normal services start after basic.target; path units start before
            # paths.target, which basic.target waits for. No programs are run.
            for name in ["confidential-agentd.service", "cai-ssh.service"]:
                (root / name).write_text("[Service]\nExecStart=/usr/bin/true\n")
            target = root / "multi-user.target"
            target.write_text("[Unit]\nRequires=basic.target\nAfter=basic.target\nWants=cai-ssh.path confidential-agentd.service\n")
            env = dict(os.environ, SYSTEMD_UNIT_PATH=str(root) + ":/usr/lib/systemd/system:/lib/systemd/system")
            args = ["systemd-analyze", "verify", str(target)]
            verified = subprocess.run(args, env=env, capture_output=True, text=True)
            self.assertEqual(verified.returncode, 0, verified.stderr)
            self.assertNotIn("ordering cycle", verified.stderr.lower())
            # The verifier must actually detect the original ordering defect.
            unit.write_text(source.replace("[Unit]", "[Unit]\nAfter=confidential-agentd.service", 1))
            rejected = subprocess.run(args, env=env, capture_output=True, text=True)
            # systemd 239 reports a broken transaction but still returns zero.
            self.assertIn("ordering cycle", rejected.stderr.lower())
            self.assertIn("deleted to break ordering cycle", rejected.stderr.lower())

    def test_configuration_rejects_unknown_harness_and_process_overrides(self):
        load = runpy.run_path(str(RUNTIME))["load_configuration"]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "runtime.json"
            for value in [{"harness": "other"}, {"harness": "codex", "environment": {"PATH": "/tmp"}},
                          {"harness": "codex", "environment": {"OPENAI_API_KEY": 123}},
                          {"harness": "codex", "command": "other-agent"}]:
                path.write_text(json.dumps(value))
                with self.assertRaises(ValueError):
                    load(path)

    def test_only_the_selected_harness_receives_instance_environment(self):
        namespace = runpy.run_path(str(RUNTIME))
        main = namespace["main"]
        globals_ = main.__globals__
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            globals_["HOME"], globals_["WORKSPACE"] = root / "home", root / "workspace"
            for harness, executable in namespace["COMMANDS"].items():
                globals_["load_configuration"] = lambda: (harness, "cli", None, {"TEST_API_KEY": "private-token"})
                with patch("sys.argv", ["cai-agent", "--version"]), \
                     patch("shutil.which", return_value="/usr/local/bin/agent"), \
                     patch("subprocess.run") as service, \
                     patch("os.chdir"), patch("os.execvpe") as execute:
                    main()
                if harness == "openclaw":
                    service.assert_called_once_with(["systemctl", "start", "cai-pep.service"], check=True)
                else:
                    service.assert_not_called()
                args = execute.call_args.args
                self.assertEqual(args[:2], (executable[0], executable + ["--version"]))
                self.assertEqual(args[2]["TEST_API_KEY"], "private-token")
                self.assertEqual(args[2]["HOME"], str(root / "home"))

    def test_openclaw_does_not_start_when_pep_fails(self):
        main = runpy.run_path(str(RUNTIME))["main"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            main.__globals__.update(HOME=root / "home", WORKSPACE=root / "workspace",
                                   load_configuration=lambda: ("openclaw", "cli", None, {}))
            with patch("sys.argv", ["cai-agent", "agent"]), \
                 patch("shutil.which", return_value="/usr/local/bin/openclaw"), \
                 patch("subprocess.run", side_effect=subprocess.CalledProcessError(1, "systemctl")), \
                 patch("os.execvpe") as execute:
                with self.assertRaises(subprocess.CalledProcessError):
                    main()
                execute.assert_not_called()

    def test_modes_and_listening_ports_are_checked_against_verified_runtime_context(self):
        namespace = runpy.run_path(str(RUNTIME))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config, bootstrap = root / "runtime.json", root / "bootstrap.json"
            config.write_text(json.dumps({"harness": "openclaw", "port": 18790}))
            self.assertEqual(namespace["load_configuration"](config)[:3], ("openclaw", "gateway", 18790))
            bootstrap.write_text(json.dumps({"schema_version": "confidential-agent/application-runtime/v1", "ports": [18789]}))
            with self.assertRaises(ValueError):
                namespace["validate_declared_ports"](18790, bootstrap)
            bootstrap.write_text(json.dumps({"schema_version": "confidential-agent/application-runtime/v1", "ports": [18790]}))
            namespace["validate_declared_ports"](18790, bootstrap)
            bootstrap.write_text(json.dumps({"ports": [18790]}))
            with self.assertRaisesRegex(ValueError, "application runtime context"):
                namespace["validate_declared_ports"](18790, bootstrap)
            for value in [{"harness": "codex", "mode": "gateway"},
                          {"harness": "codex", "mode": "cli", "port": 4500},
                          {"harness": "openclaw", "port": True},
                          {"harness": "openclaw", "port": 65536}]:
                config.write_text(json.dumps(value))
                with self.assertRaises(ValueError):
                    namespace["load_configuration"](config)

    def test_codex_app_server_uses_the_instance_token_and_configured_port(self):
        namespace = runpy.run_path(str(RUNTIME))
        main = namespace["main"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = root / "home"
            token = home / ".codex/app-server-token"
            token.parent.mkdir(parents=True)
            token.write_text("test-capability-token")
            token.chmod(0o600)
            main.__globals__.update(HOME=home, WORKSPACE=root / "workspace",
                                   load_configuration=lambda: ("codex", "app-server", 4501, {}),
                                   validate_declared_ports=lambda port: None)
            with patch("sys.argv", ["cai-agent", "--serve"]), \
                 patch("shutil.which", return_value="/usr/local/bin/codex"), \
                 patch("subprocess.run"), patch("os.chdir"), patch("os.execvpe") as execute:
                main()
            command = execute.call_args.args[1]
            self.assertEqual(command, ["codex", "app-server", "--listen", "ws://0.0.0.0:4501",
                                       "--ws-auth", "capability-token", "--ws-token-file", str(token)])
            token.chmod(0o644)
            with patch("sys.argv", ["cai-agent", "--check"]), \
                 patch("shutil.which", return_value="/usr/local/bin/codex"):
                with self.assertRaises(ValueError):
                    main()

    def test_release_update_controls_preserve_delivered_settings(self):
        namespace = runpy.run_path(str(RUNTIME))
        configure = namespace["configure_fixed_release"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            configure.__globals__["HOME"] = root
            environment = {"OPENCODE_DISABLE_AUTOUPDATE": "false"}
            configure("opencode", environment)
            self.assertEqual(environment["OPENCODE_DISABLE_AUTOUPDATE"], "true")
            configure("qodercli", {})
            path = root / ".qoder/settings.json"
            self.assertFalse(json.loads(path.read_text())["general"]["enableAutoUpdate"])
            path.write_text(json.dumps({"general": {"enableAutoUpdate": True}, "private": "test-only"}))
            before = path.read_bytes()
            with self.assertRaises(ValueError):
                configure("qodercli", {})
            self.assertEqual(path.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
