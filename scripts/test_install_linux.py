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
            # Must agree with cm_pdf::worker::linux_bundled_library.
            self.assertEqual((prefix / 'lib/citation-manager/libpdfium.so').read_bytes(), b'so')
            self.assertTrue((prefix / 'share/doc/citation-manager/pdfium-licenses/pdfium.txt').is_file())
            self.assertEqual(library.parent.stat().st_mode & 0o777, 0o700)
            self.assertFalse(library.exists())
            entry = (prefix / 'share/applications/citation-manager.desktop').read_text()
            self.assertIn(f'Exec="{target}" --library {library} --settings {settings}', entry)

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
