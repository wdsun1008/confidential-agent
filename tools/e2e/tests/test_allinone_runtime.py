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
                globals_["load_configuration"] = lambda: (harness, {"TEST_API_KEY": "private-token"})
                with patch("sys.argv", ["cai-agent", "--version"]), \
                     patch("shutil.which", return_value="/usr/local/bin/agent"), \
                     patch("os.chdir"), patch("os.execvpe") as execute:
                    main()
                args = execute.call_args.args
                self.assertEqual(args[:2], (executable[0], executable + ["--version"]))
                self.assertEqual(args[2]["TEST_API_KEY"], "private-token")
                self.assertEqual(args[2]["HOME"], str(root / "home"))


if __name__ == "__main__":
    unittest.main()
