#!/usr/bin/env python3
"""Fail closed on regressions to Paku's Pi-only public surface and identity."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[1]

class PakuSurface(unittest.TestCase):
    def test_only_pi_and_explicit_test_double_exist_in_wire_protocol(self):
        source = (ROOT / 'crates/proto/src/agent.rs').read_text()
        body = re.search(r'pub enum HarnessId\s*\{(.*?)\n\}', source, re.S).group(1)
        variants = re.findall(r'^\s*(\w+)\s*,\s*$', body, re.M)
        self.assertEqual(variants, ['Pi', 'Mock'])
        for name in ['acp', 'claude', 'codex', 'cursor', 'opencode']:
            self.assertFalse((ROOT / 'crates/harness/src' / name).exists(), name)
        self.assertNotIn('cursor_sdk_version', (ROOT / 'crates/proto/src/entities.rs').read_text())

    def test_paku_namespace_and_executable(self):
        root = (ROOT / 'Cargo.toml').read_text()
        self.assertIn('"apps/paku"', root)
        self.assertNotIn('zeron-', root)
        self.assertNotIn('deser-hjson', root)
        cli = (ROOT / 'apps/paku/src/main.rs').read_text()
        self.assertIn('name = "paku"', cli)
        self.assertIn('HarnessId::Pi', cli)
        desktop = (ROOT / 'dist/paku.desktop').read_text()
        self.assertIn('Name=Paku', desktop)
        self.assertIn('Exec=paku', desktop)
        self.assertIn('x-scheme-handler/paku', desktop)
        self.assertFalse((ROOT / 'apps/zeron').exists())

    def test_fork_credit_and_independent_storage(self):
        readme = (ROOT / 'README.md').read_text()
        self.assertTrue(readme.startswith('# Paku'))
        self.assertIn('https://github.com/zeronsh/zeron', readme)
        self.assertIn('Pi', readme)
        self.assertIn('Copyright', (ROOT / 'LICENSE').read_text())
        paths = (ROOT / 'apps/paku/src/paths.rs').read_text()
        self.assertIn('home.join(".paku")', paths)
        self.assertNotIn('std::fs::rename', paths)

    def test_no_inherited_hosted_service_defaults(self):
        cli = (ROOT / 'apps/paku/src/main.rs').read_text()
        self.assertIn('const DEFAULT_EDGE_URL: &str = "";', cli)
        forbidden = ['edge.zeron.sh', 'https://zeron.sh', 'https://paku.sh',
                     'client_01KWD0EAKZKD50YCQJNYSRE4BY']
        files = list((ROOT / 'crates').glob('*/src/**/*.rs'))
        files += list((ROOT / 'apps/paku/src').glob('*.rs'))
        files += list((ROOT / 'apps/ios/Paku').glob('**/*.swift'))
        for path in files:
            text = path.read_text()
            # Icons retain original authorship, not a runtime service endpoint.
            text = '\n'.join(line for line in text.splitlines()
                             if not line.lstrip().startswith(('//', '*')))
            for value in forbidden:
                self.assertNotIn(value, text, str(path.relative_to(ROOT)))

    def test_windows_install_identity_does_not_collide_with_zeron(self):
        installer = (ROOT / 'dist/windows/paku.iss').read_text()
        updater = (ROOT / 'crates/update/src/windows.rs').read_text()
        app_id = re.search(r'AppId=\{\{([^}]+)\}', installer).group(1)
        self.assertEqual(app_id.upper(), 'D98C3134-EF43-4BDB-94B8-1D892301A381')
        self.assertIn('{' + app_id.upper() + '}_IS1', updater.upper())

if __name__ == '__main__':
    unittest.main(verbosity=2)
