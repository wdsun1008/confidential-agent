"""Check the image configuration and selected process without installing agents."""
import json
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
RUNTIME = ROOT / "examples/allinone/files/cai-agent"


class AllinoneRuntimeTest(unittest.TestCase):
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
                     patch("os.chdir"), patch("os.execvpe") as execute:
                    main()
                args = execute.call_args.args
                self.assertEqual(args[:2], (executable[0], executable + ["--version"]))
                self.assertEqual(args[2]["TEST_API_KEY"], "private-token")
                self.assertEqual(args[2]["HOME"], str(root / "home"))

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
