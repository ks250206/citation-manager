#!/usr/bin/env python3
"""Default lane: Linux install layout without network, GUI or real PDFium."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('install_linux', ROOT / 'scripts/install_linux.py')
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class InstallLinux(unittest.TestCase):
    def test_layout_matches_the_pdf_worker_bundle_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executable = root / 'build/citation-manager'
            executable.parent.mkdir()
            executable.write_bytes(b'binary')
            (root / 'build/papersd').write_bytes(b'daemon')
            (root / 'build/papers-native-host').write_bytes(b'host')
            pdfium = root / 'pdfium'
            (pdfium / 'lib').mkdir(parents=True)
            (pdfium / 'licenses').mkdir()
            (pdfium / 'lib/libpdfium.so').write_bytes(b'so')
            (pdfium / 'LICENSE').write_text('license')
            (pdfium / 'licenses/pdfium.txt').write_text('notice')
            prefix = root / 'prefix with space'
            library = root / 'data/library'
            settings = root / 'config/ui.json'
            target = installer.install(prefix, executable, library, settings, pdfium)
            self.assertEqual(target, prefix / 'bin/citation-manager')
            self.assertTrue(target.stat().st_mode & 0o111)
            # The GUI starts the sibling daemon by absolute path.
            self.assertEqual((prefix / 'bin/papersd').read_bytes(), b'daemon')
            self.assertTrue((prefix / 'bin/papersd').stat().st_mode & 0o111)
            # The browser host is installed as papersd's sibling (#23).
            self.assertEqual((prefix / 'bin/papers-native-host').read_bytes(), b'host')
            self.assertTrue((prefix / 'bin/papers-native-host').stat().st_mode & 0o111)
            # Must agree with cm_pdf::worker::linux_bundled_library.
            self.assertEqual((prefix / 'lib/citation-manager/libpdfium.so').read_bytes(), b'so')
            self.assertTrue((prefix / 'share/doc/citation-manager/pdfium-licenses/pdfium.txt').is_file())
            self.assertEqual(library.parent.stat().st_mode & 0o777, 0o700)
            self.assertFalse(library.exists())
            entry = (prefix / 'share/applications/citation-manager.desktop').read_text()
            self.assertIn(f'Exec="{target}" --library {library} --settings {settings}', entry)

    def test_browser_registration_uses_the_installed_host_and_explicit_ids(self):
        ids = {'development': {'chrome': ['d' * 32], 'firefox': ['dev@x.invalid']},
               'release': {'chrome': [], 'firefox': []}}
        host = Path('/opt/cm/bin/papers-native-host')
        # No released extension yet and no explicit development opt-in: nothing is registered.
        self.assertIsNone(installer.native_host_command(host, Path('/l'), ids))
        self.assertEqual(
            installer.native_host_command(host, Path('/l'), ids, development=True, pair=True,
                                          browsers=['firefox']),
            [str(host), 'register', '--library', '/l', '--development-extensions',
             '--browser', 'firefox', '--pair'])
        ids['release'] = {'chrome': ['r' * 32], 'firefox': ['cm@example.org']}
        self.assertEqual(
            installer.native_host_command(host, Path('/l'), ids),
            [str(host), 'register', '--library', '/l', '--chrome-extension-id', 'r' * 32,
             '--firefox-extension-id', 'cm@example.org'])
        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory)
            calls = []
            run = lambda command, check: calls.append(command)  # noqa: E731
            self.assertIsNone(installer.register_browsers(prefix, Path('/l'), True, run=run))
            (prefix / 'bin').mkdir()
            (prefix / 'bin/papers-native-host').write_bytes(b'host')
            command = installer.register_browsers(prefix, Path('/l'), True, run=run)
            self.assertEqual(calls, [command])
            self.assertEqual(command[:2], [str(prefix / 'bin/papers-native-host'), 'register'])
            # Without any extension ID the installer still refreshes an existing registration
            # (no-op when unregistered) instead of leaving stale manifests after a move.
            original = installer.EXTENSION_IDS
            empty = prefix / 'ids.json'
            empty.write_text('{"development": {}, "release": {"chrome": [], "firefox": []}}')
            installer.EXTENSION_IDS = empty
            try:
                command = installer.register_browsers(prefix, Path('/l'), False, run=run)
            finally:
                installer.EXTENSION_IDS = original
            self.assertEqual(command, [str(prefix / 'bin/papers-native-host'), 'refresh'])
            self.assertEqual(calls[-1], command)

    def test_desktop_exec_arguments_follow_the_entry_spec(self):
        self.assertEqual(installer.exec_argument('/opt/cm/bin/citation-manager'), '/opt/cm/bin/citation-manager')
        self.assertEqual(installer.exec_argument('/home/a b/50%'), '"/home/a b/50%%"')
        self.assertEqual(installer.exec_argument('/x/$HOME"`\\'), '"/x/\\$HOME\\"\\`\\\\"')

    def test_relative_paths_and_missing_executable_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(ValueError, 'absolute'):
                installer.install(root, root / 'x', Path('library'), root / 'ui.json')
            with self.assertRaises(FileNotFoundError):
                installer.install(root / 'p', root / 'missing', root / 'l', root / 'ui.json')


if __name__ == '__main__':
    unittest.main()
