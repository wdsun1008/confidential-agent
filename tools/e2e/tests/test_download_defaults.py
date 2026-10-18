import importlib.util
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch


PATH = Path(__file__).resolve().parents[1] / "render_case.py"
sys.path.insert(0, str(PATH.parent))
SPEC = importlib.util.spec_from_file_location("render_download_defaults", PATH)
RENDER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RENDER)


class DownloadDefaultsTest(unittest.TestCase):
    def context(self, environment):
        environment = {"COSIGN_KEY": "/unused-test-cosign.key", **environment}
        with patch.dict(os.environ, environment, clear=True):
            return RENDER.context_from_env(Path("/unused-e2e-work"))

    def test_cli_and_other_agents_share_mainland_default(self):
        context = self.context({})
        self.assertEqual(context["NPM_REGISTRY"], "https://registry.npmmirror.com/")
        self.assertEqual(context["CLI_AGENT_NPM_REGISTRY"], context["NPM_REGISTRY"])

    def test_shared_registry_override_reaches_cli_agents(self):
        context = self.context({"E2E_NPM_REGISTRY": "https://shared.example/"})
        self.assertEqual(context["CLI_AGENT_NPM_REGISTRY"], "https://shared.example/")

    def test_cli_override_takes_precedence(self):
        context = self.context({"E2E_NPM_REGISTRY": "https://shared.example/",
                                "E2E_CLI_AGENT_NPM_REGISTRY": "https://cli.example/"})
        self.assertEqual(context["NPM_REGISTRY"], "https://shared.example/")
        self.assertEqual(context["CLI_AGENT_NPM_REGISTRY"], "https://cli.example/")
