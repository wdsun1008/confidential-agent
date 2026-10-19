"""Exercise the build-time adapter without installing Hermes or changing its pins."""
from collections import namedtuple
from pathlib import Path
import re
import runpy
import subprocess
import tempfile
from types import ModuleType
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
# The upstream adapter's public contract; its downloader is exercised separately
# with the real pinned source and hash-verified binaries on the deployment host.
ADAPTER = '''import re
from pm.downloader import Source

def mirror_url(sha256):
    if not re.fullmatch(r"[a-f0-9]{64}", sha256):
        raise ValueError("invalid pinned hash")
    return "https://hermes-assets.nousresearch.com/upstream/sha256/" + sha256

def pinned_source(url, dest, sha256):
    archive = mirror_url(sha256)
    return Source(url, dest, sha256, fallbacks=() if url == archive else (archive,))
'''


class HermesDownloads(unittest.TestCase):
    def prepare(self, root, *, proxy='https://gh-proxy.org/', source=ADAPTER):
        repo, payload = root / 'repo', root / 'payload'
        module = repo / 'pm/artifact_mirror.py'
        module.parent.mkdir(parents=True, exist_ok=True)
        if not module.exists():
            module.write_text(source)
        lock = repo / 'pm/lock.json'
        if not lock.exists():
            lock.write_text('{"packages":{"python":{"version":"pinned"}}}\n')
        installer = (ROOT / 'examples/hermes-agent/files/install-hermes-agent-runtime.sh').read_text()
        function = re.search(r'(?ms)^prepare_hermes_binary_downloads\(\) \{\n.*?^PY\n\}', installer)
        self.assertIsNotNone(function)
        shell = 'HERMES_INSTALL_DIR="$1"\nHERMES_PAYLOAD_DIR="$2"\nCA_GITHUB_PROXY_URL="$3"\n' + function[0] + '\nprepare_hermes_binary_downloads\n'
        result = subprocess.run(['bash', '-eu', '-c', shell, 'hermes-download-test', str(repo), str(payload), proxy],
                                capture_output=True, text=True, timeout=20)
        return result, module, lock

    def load(self, module):
        downloader = ModuleType('pm.downloader')
        downloader.Source = namedtuple('Source', ['url', 'dest', 'sha256', 'fallbacks'])
        with patch.dict('sys.modules', {'pm.downloader': downloader}):
            return runpy.run_path(str(module))['pinned_source']

    def test_proxy_routes_keep_both_fallbacks_hashes_and_pins(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            result, module, lock = self.prepare(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            source = self.load(module)
            digest = 'a' * 64
            destination = root / 'archive'
            for name in ['python-build-standalone', 'FFmpeg-Builds', 'ripgrep', 'uv']:
                url = f'https://github.com/vendor/{name}/releases/download/pin/archive.tar.gz'
                selected = source(url, destination, digest)
                self.assertEqual(selected.url, 'https://gh-proxy.org/' + url)
                self.assertEqual(selected.fallbacks, (url, 'https://hermes-assets.nousresearch.com/upstream/sha256/' + digest))
                self.assertEqual((selected.dest, selected.sha256), (destination, digest))
            for url in ['https://example.org/other.tar.gz',
                        'https://hermes-assets.nousresearch.com/upstream/sha256/' + digest]:
                selected = source(url, destination, digest)
                self.assertEqual(selected.url, url)
                self.assertEqual(selected.fallbacks, () if url.endswith(digest) else ('https://hermes-assets.nousresearch.com/upstream/sha256/' + digest,))
            with self.assertRaises(ValueError):
                source('https://github.com/vendor/archive', destination, 'unverified')
            self.assertEqual(lock.read_text(), '{"packages":{"python":{"version":"pinned"}}}\n')

    def test_custom_prefix_and_repeated_preparation_are_stable(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            result, module, _ = self.prepare(root, proxy='https://mirror.example/prefix')
            self.assertEqual(result.returncode, 0, result.stderr)
            before = module.read_bytes()
            receipt = (root / 'payload/binary-downloads.json').read_bytes()
            result, _, _ = self.prepare(root, proxy='https://mirror.example/prefix')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(module.read_bytes(), before)
            self.assertEqual((root / 'payload/binary-downloads.json').read_bytes(), receipt)
            url = 'https://github.com/vendor/archive'
            self.assertEqual(self.load(module)(url, root / 'archive', 'b' * 64).url,
                             'https://mirror.example/prefix/' + url)

    def test_invalid_proxy_or_changed_upstream_fails_before_modifying_source(self):
        for proxy in ['http://mirror.example/', 'https://user:password@mirror.example/',
                      'https://mirror.example/?token=x', 'https://mirror.example/\npath']:
            with self.subTest(proxy=proxy), tempfile.TemporaryDirectory() as tmp:
                result, module, _ = self.prepare(Path(tmp), proxy=proxy)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(module.read_text(), ADAPTER)
        with tempfile.TemporaryDirectory() as tmp:
            changed = ADAPTER.replace('    return Source(', '    return make_source(')
            result, module, _ = self.prepare(Path(tmp), source=changed)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('unsupported Hermes binary download adapter', result.stderr)
            self.assertEqual(module.read_text(), changed)


if __name__ == '__main__':
    unittest.main()
