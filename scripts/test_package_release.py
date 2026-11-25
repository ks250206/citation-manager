#!/usr/bin/env python3
"""#31 release scripts in the default lane: notices closure, DMG plan, signing preconditions.

No macOS tools, no network, no secrets: the real .dmg and the (UNVERIFIED) signing status
are produced on macOS by .github/workflows/package-macos.yml.
"""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, ROOT / path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


sys.path.insert(0, str(ROOT / 'scripts/package/macos'))
notices = load('third_party_notices', 'scripts/package/third_party_notices.py')
create_dmg = load('create_dmg', 'scripts/package/macos/create_dmg.py')
release_sign = load('release_sign', 'scripts/package/macos/release_sign.py')


def package(root, name, license, files=(), member=False):
    directory = root / name
    directory.mkdir()
    (directory / 'Cargo.toml').write_text('')
    for file_name, text in files:
        (directory / file_name).write_text(text)
    return {'id': name, 'name': name, 'version': '1.0.0', 'license': license,
            'license_file': None, 'manifest_path': str(directory / 'Cargo.toml'),
            'source': None if member else 'registry+https://github.com/rust-lang/crates.io-index',
            'repository': f'https://example.invalid/{name}'}


def dep(pkg, kind=None):
    return {'pkg': pkg, 'dep_kinds': [{'kind': kind, 'target': None}]}


class Notices(unittest.TestCase):
    def metadata(self, root):
        packages = [
            package(root, 'cm-daemon', None, member=True),
            package(root, 'cm-cli', None, member=True),
            package(root, 'cm-mcp', None, member=True),
            package(root, 'cm-native-host', None, member=True),
            package(root, 'shipped', 'MIT OR Apache-2.0', [('LICENSE-MIT', 'MIT text'), ('LICENSE-APACHE', 'Apache text')]),
            package(root, 'same-text', 'MIT', [('LICENSE', 'MIT text')]),
            package(root, 'no-file', 'Zlib'),
            package(root, 'dev-only', 'GPL-3.0-only'),
            package(root, 'build-only', 'GPL-3.0-only'),
        ]
        nodes = [{'id': 'cm-daemon', 'deps': [dep('shipped'), dep('dev-only', 'dev'), dep('build-only', 'build')]},
                 {'id': 'cm-cli', 'deps': [dep('same-text')]},
                 {'id': 'cm-mcp', 'deps': [dep('no-file')]},
                 {'id': 'cm-native-host', 'deps': []}] + \
            [{'id': p, 'deps': []} for p in ('shipped', 'same-text', 'no-file', 'dev-only', 'build-only')]
        return {'packages': packages, 'workspace_members': ['cm-daemon', 'cm-cli', 'cm-mcp', 'cm-native-host'],
                'resolve': {'nodes': nodes}}

    def test_only_the_normal_closure_of_shipped_crates_is_listed_with_its_texts(self):
        with tempfile.TemporaryDirectory() as directory:
            shipped = notices.shipped_packages(self.metadata(Path(directory)))
            self.assertEqual([p['name'] for p in shipped], ['no-file', 'same-text', 'shipped'])
            document, summary = notices.render(shipped)
        self.assertEqual(summary['without_license_file'], ['no-file 1.0.0'])
        self.assertEqual(summary['copyleft_only'], [])
        self.assertEqual(summary['distinct_texts'], 2, 'identical MIT texts are printed once')
        self.assertIn('### Used by: same-text 1.0.0 (LICENSE), shipped 1.0.0 (LICENSE-MIT)', document)
        self.assertIn('| no-file | 1.0.0 | Zlib | crates.io |', document)
        self.assertIn('licenses/pdfium/', document)
        self.assertNotIn('dev-only', document)

    def test_copyleft_only_or_undeclared_licenses_are_flagged(self):
        self.assertTrue(notices.only_copyleft('GPL-3.0-only'))
        self.assertTrue(notices.only_copyleft('LGPL-2.1-or-later OR GPL-2.0-only'))
        self.assertTrue(notices.only_copyleft(None))
        self.assertFalse(notices.only_copyleft('MIT OR GPL-3.0-only'))
        self.assertFalse(notices.only_copyleft('MPL-2.0'))
        self.assertFalse(notices.only_copyleft('MIT/Apache-2.0'))
        self.assertFalse(notices.only_copyleft('(MIT OR Apache-2.0) AND Unicode-3.0'))


class BundledNotices(unittest.TestCase):
    def test_generated_notices_are_bundled_with_the_pdfium_ones(self):
        build_app = sys.modules['build_app'] if 'build_app' in sys.modules else load(
            'build_app', 'scripts/package/macos/build_app.py')
        import hashlib
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bin_dir = root / 'bin'
            bin_dir.mkdir()
            for name in build_app.EXECUTABLES:
                (bin_dir / name).write_bytes(b'x')
            pdfium = root / 'pdfium'
            (pdfium / 'lib').mkdir(parents=True)
            (pdfium / 'licenses').mkdir()
            (pdfium / 'lib/libpdfium.dylib').write_bytes(b'pdfium')
            (pdfium / 'LICENSE').write_text('BSD')
            notices_dir = root / 'notices'
            notices_dir.mkdir()
            pin = dict(build_app.pdfium.artifact('macos'), library_sha256=hashlib.sha256(b'pdfium').hexdigest())
            with mock.patch.object(build_app.pdfium, 'artifact', return_value=pin), \
                    mock.patch.object(build_app.pdfium, 'verify_notices'):
                with self.assertRaisesRegex(ValueError, 'third_party_notices.py'):
                    build_app.build(bin_dir, pdfium, root / 'out', icon=False, notices_dir=notices_dir)
                (notices_dir / 'THIRD_PARTY_NOTICES.md').write_text('# Third-party notices')
                (notices_dir / 'third-party-licenses.json').write_text('{}')
                app = build_app.build(bin_dir, pdfium, root / 'out', icon=False, notices_dir=notices_dir)
            licenses = app / 'Contents/Resources/licenses'
            self.assertEqual((licenses / 'THIRD_PARTY_NOTICES.md').read_text(), '# Third-party notices')
            self.assertTrue((licenses / 'third-party-licenses.json').is_file())
            self.assertTrue((licenses / 'pdfium/LICENSE').is_file())


class Dmg(unittest.TestCase):
    def test_image_holds_the_app_and_an_applications_link(self):
        steps = create_dmg.plan('/b/Citation Manager.app', '/s', '/o/x.dmg')
        self.assertEqual(steps[0], ['ditto', '/b/Citation Manager.app', '/s/Citation Manager.app'])
        self.assertEqual(steps[1], ['ln', '-s', '/Applications', '/s/Applications'])
        create = steps[2]
        self.assertEqual(create[:2], ['hdiutil', 'create'])
        self.assertEqual(create[create.index('-format') + 1], 'UDZO')
        self.assertEqual(create[create.index('-volname') + 1], 'Citation Manager')
        self.assertEqual(steps[3], ['hdiutil', 'verify', '/o/x.dmg'])


class ReleaseSign(unittest.TestCase):
    def test_missing_identity_is_reported_unverified(self):
        none = mock.Mock(stdout='     0 valid identities found\n')
        with mock.patch.object(release_sign.subprocess, 'run', return_value=none) as run:
            status, identity = release_sign.preflight({'CM_NOTARY_KEYCHAIN_PROFILE': 'p'})
        self.assertIsNone(identity)
        run.assert_called_once()  # only the identity listing, nothing is signed
        for key in ('developer_id_identity', 'pdfium_signature', 'developer_id_signature'):
            self.assertEqual(status[key], 'UNVERIFIED: no Developer ID')
        self.assertTrue(status['notarization_dmg'].startswith('UNVERIFIED'))
        # An identity in the environment that the Keychain does not hold is still unverified.
        with mock.patch.object(release_sign.subprocess, 'run', return_value=none):
            _, identity = release_sign.preflight({'CM_SIGN_IDENTITY': 'Developer ID Application: A (TEAM123456)'})
        self.assertIsNone(identity)

    def test_team_id_comes_from_the_identity_name(self):
        self.assertEqual(release_sign.team_id('Developer ID Application: A B (TEAM123456)'), 'TEAM123456')
        self.assertIsNone(release_sign.team_id('-'))

    def test_identity_must_be_a_listed_developer_id(self):
        listed = '  1) ABC "Developer ID Application: A B (TEAM123456)"\n     1 valid identities found\n'
        self.assertTrue(release_sign.identity_available('Developer ID Application: A B (TEAM123456)', listed))
        self.assertFalse(release_sign.identity_available('Developer ID Application: C (X)', listed))
        self.assertFalse(release_sign.identity_available('Apple Development: A B (TEAM123456)',
                                                         listed.replace('Developer ID Application', 'Apple Development')))
        self.assertFalse(release_sign.identity_available('', listed))

    def test_notary_credentials_come_only_from_named_environment_entries(self):
        self.assertIsNone(release_sign.notary_credentials({}))
        self.assertIsNone(release_sign.notary_credentials({'CM_NOTARY_KEY_ID': 'k'}))
        self.assertEqual(release_sign.notary_credentials({'CM_NOTARY_KEYCHAIN_PROFILE': 'cm'}),
                         ['--keychain-profile', 'cm'])
        self.assertEqual(release_sign.notary_credentials(
            {'CM_NOTARY_KEY_PATH': '/k.p8', 'CM_NOTARY_KEY_ID': 'K', 'CM_NOTARY_ISSUER': 'I'}),
            ['--key', '/k.p8', '--key-id', 'K', '--issuer', 'I'])

    def test_workflow_never_prints_or_traces_secrets(self):
        workflow = (ROOT / '.github/workflows/package-macos.yml').read_text()
        self.assertNotRegex(workflow, r'-----BEGIN|MII[A-Za-z0-9+/]{40}')
        self.assertNotIn('set -x', workflow)
        for secret in ('CM_SIGN_P12_BASE64', 'CM_SIGN_P12_PASSWORD', 'CM_NOTARY_KEY_P8', 'CM_SIGN_IDENTITY'):
            self.assertIn(f'{secret}: ${{{{ secrets.{secret} }}}}', workflow)
            for line in workflow.splitlines():
                if secret in line:
                    self.assertNotRegex(line, r'\becho\b', line)
        # Unverified is reported, never success, when the secrets are absent.
        self.assertIn('UNVERIFIED: no Developer ID', workflow)

if __name__ == '__main__':
    unittest.main()
