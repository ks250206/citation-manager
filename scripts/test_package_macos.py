#!/usr/bin/env python3
"""#34 macOS packaging scripts in the default lane (no macOS tools, no real PDFium).

The real bundle is built, signed ad hoc and exercised on macOS by
.github/workflows/package-macos.yml; these tests pin the layout contract, the signing
order and the parsers the smoke relies on.
"""
import hashlib
import importlib.util
import json
from pathlib import Path
import plistlib
import sys
import tempfile
import unittest
from unittest import mock
import zlib

ROOT = Path(__file__).resolve().parents[1]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, ROOT / path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


build_app = load('build_app', 'scripts/package/macos/build_app.py')
codesign = load('codesign', 'scripts/package/macos/codesign.py')
smoke = load('layout_smoke', 'scripts/package/layout-smoke/smoke.py')
browser_smoke = load('browser_smoke', 'scripts/native-host/browser_smoke.py')


def fake_inputs(root, dylib=b'pdfium'):
    bin_dir = root / 'target/release'
    bin_dir.mkdir(parents=True)
    for name in build_app.EXECUTABLES:
        (bin_dir / name).write_bytes(name.encode())
    pdfium = root / 'pdfium'
    (pdfium / 'lib').mkdir(parents=True)
    (pdfium / 'licenses').mkdir()
    (pdfium / 'lib/libpdfium.dylib').write_bytes(dylib)
    (pdfium / 'LICENSE').write_text('BSD')
    for name in ('pdfium.txt', 'freetype.txt'):
        (pdfium / 'licenses' / name).write_text(name)
    return bin_dir, pdfium


class BuildApp(unittest.TestCase):
    def test_layout_matches_the_bundle_contract_of_the_binaries(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bin_dir, pdfium = fake_inputs(root)
            pin = dict(build_app.pdfium.artifact('macos'),
                       library_sha256=hashlib.sha256(b'pdfium').hexdigest())
            with mock.patch.object(build_app.pdfium, 'artifact', return_value=pin), \
                    mock.patch.object(build_app.pdfium, 'verify_notices'):
                app = build_app.build(bin_dir, pdfium, root / 'out dir', version='9.8.7', icon=False)
            self.assertEqual(app.name, 'Citation Manager.app')
            contents = app / 'Contents'
            # Siblings: every client starts MacOS/papersd by absolute path.
            for name in build_app.EXECUTABLES:
                executable = contents / 'MacOS' / name
                self.assertEqual(executable.read_bytes(), name.encode())
                self.assertEqual(executable.stat().st_mode & 0o777, 0o755)
            # Must agree with cm_pdf::worker::macos_bundled_library.
            self.assertEqual((contents / 'Frameworks/libpdfium.dylib').read_bytes(), b'pdfium')
            self.assertEqual(build_app.PDFIUM, 'Frameworks/libpdfium.dylib')
            plist = plistlib.loads((contents / 'Info.plist').read_bytes())
            self.assertEqual(plist['CFBundleExecutable'], 'citation-manager')
            self.assertEqual(plist['CFBundleIdentifier'], build_app.BUNDLE_ID)
            self.assertEqual(plist['CFBundleShortVersionString'], '9.8.7')
            self.assertEqual(plist['LSMinimumSystemVersion'], '13.0')
            self.assertNotIn('CFBundleIconFile', plist)
            self.assertEqual((contents / 'PkgInfo').read_text(), 'APPL????')
            licenses = contents / 'Resources/licenses/pdfium'
            self.assertEqual(sorted(p.name for p in licenses.iterdir()), ['LICENSE', 'freetype.txt', 'pdfium.txt'])
            runtime = json.loads((contents / 'Resources/pdfium-runtime.json').read_text())
            self.assertEqual(runtime['bundle_path'], 'Frameworks/libpdfium.dylib')
            self.assertEqual(runtime['library_sha256'], pin['library_sha256'])
            ids = json.loads((contents / 'Resources/native-messaging/extension-ids.json').read_text())
            self.assertIn('release', ids)
            # The native host refuses group/other-writable executables or directories.
            for path in [app, *app.rglob('*')]:
                self.assertEqual(path.stat().st_mode & 0o022, 0, path)
            # Rebuilding replaces the bundle instead of merging into it.
            (contents / 'stale').write_text('x')
            with mock.patch.object(build_app.pdfium, 'artifact', return_value=pin), \
                    mock.patch.object(build_app.pdfium, 'verify_notices'):
                build_app.build(bin_dir, pdfium, root / 'out dir', icon=False)
            self.assertFalse((contents / 'stale').exists())

    def test_an_unpinned_dylib_or_missing_binary_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bin_dir, pdfium = fake_inputs(root, dylib=b'not the pinned build')
            with self.assertRaisesRegex(ValueError, 'pinned PDFium'):
                build_app.build(bin_dir, pdfium, root / 'out', icon=False)
            (bin_dir / 'papersd').unlink()
            with self.assertRaisesRegex(ValueError, 'papersd'):
                build_app.build(bin_dir, pdfium, root / 'out', icon=False)


class Codesign(unittest.TestCase):
    def test_inside_out_order_keeps_the_pinned_dylib_unless_asked(self):
        app = Path('/x/Citation Manager.app')
        steps = codesign.plan(app)
        targets = [step[-1] for step in steps]
        self.assertEqual(targets[-1], str(app), 'the bundle is signed last')
        self.assertNotIn(str(app / 'Contents/Frameworks/libpdfium.dylib'), targets)
        self.assertEqual(set(targets[:-1]), {str(app / 'Contents/MacOS' / n) for n in
                                             ('papersd', 'papers', 'papers-mcp', 'papers-native-host')})
        for step in steps:
            self.assertEqual(step[:4], ['codesign', '--force', '--sign', '-'])
            self.assertIn('--timestamp=none', step)
            self.assertNotIn('runtime', step)
        hardened = codesign.plan(app, hardened_runtime=True, sign_pdfium=True)
        self.assertEqual(hardened[0][-1], str(app / 'Contents/Frameworks/libpdfium.dylib'))
        self.assertTrue(all(step[step.index('--options') + 1] == 'runtime' for step in hardened))


class SmokeParsers(unittest.TestCase):
    def test_otool_output_parsing(self):
        linked = smoke.parse_linked_libraries(
            '/A.app/Contents/MacOS/papers:\n'
            '\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1351.0.0)\n'
            '\t/System/Library/Frameworks/Security.framework/Versions/A/Security (compatibility version 1.0.0, current version 1.0.0)\n')
        self.assertEqual(linked, ['/usr/lib/libSystem.B.dylib',
                                  '/System/Library/Frameworks/Security.framework/Versions/A/Security'])
        self.assertEqual(smoke.dependency_problems(linked), [])
        self.assertEqual(smoke.dependency_problems(['@rpath/libpdfium.dylib', '/opt/homebrew/lib/libz.dylib',
                                                    './libpdfium.dylib']),
                         ['@rpath/libpdfium.dylib', '/opt/homebrew/lib/libz.dylib', './libpdfium.dylib'])
        # The dylib's own install name is not a dependency.
        self.assertEqual(smoke.dependency_problems(['./libpdfium.dylib', '/usr/lib/libc++.1.dylib'],
                                                   own_id='./libpdfium.dylib'), [])
        loads = smoke.parse_load_commands(
            'Load command 9\n      cmd LC_ID_DYLIB\n  cmdsize 48\n         name ./libpdfium.dylib (offset 24)\n'
            'Load command 10\n      cmd LC_BUILD_VERSION\n  cmdsize 32\n platform 1\n    minos 13.0\n      sdk 26.0\n'
            'Load command 11\n          cmd LC_RPATH\n      cmdsize 32\n         path @loader_path/../Frameworks (offset 12)\n')
        self.assertEqual(loads, {'rpaths': ['@loader_path/../Frameworks'], 'id': './libpdfium.dylib',
                                 'minos': ['13.0']})

    def test_rendered_page_becomes_a_valid_png(self):
        rgba = bytes([255, 255, 255, 255, 0, 0, 0, 255]) * 2  # 2x2: white, black per row
        png = smoke.png_bytes(2, 2, rgba)
        self.assertTrue(png.startswith(b'\x89PNG\r\n\x1a\n'))
        idat = png[png.index(b'IDAT') + 4:png.index(b'IEND') - 8]
        self.assertEqual(zlib.decompress(idat), b'\x00' + rgba[:8] + b'\x00' + rgba[8:])
        self.assertEqual(smoke.ink_pixels(rgba), 2)
        with self.assertRaises(ValueError):
            smoke.png_bytes(3, 2, rgba)

    def test_page_text_joins_returned_pages(self):
        self.assertEqual(smoke.page_text({'pages': [{'text': 'a'}, {'text': None}, {'text': 'b'}]}), 'a\n\nb')


class BrowserSmokeMacos(unittest.TestCase):
    def test_ps_lines_match_the_exact_executable_with_spaces(self):
        exe = '/tmp/Install Root/Citation Manager.app/Contents/MacOS/papersd'
        self.assertTrue(browser_smoke.command_runs(exe + ' --library /l', exe))
        self.assertTrue(browser_smoke.command_runs(exe, exe))
        self.assertFalse(browser_smoke.command_runs(exe + '-other --x', exe))
        self.assertFalse(browser_smoke.command_runs('/tmp/Install', exe))

    def test_branded_chrome_gets_the_load_extension_escape_hatch_on_macos_only(self):
        mac = browser_smoke.chromium_arguments(Path('/p'), Path('/e'), True)
        linux = browser_smoke.chromium_arguments(Path('/p'), Path('/e'), False)
        self.assertIn('--disable-features=DisableLoadExtensionCommandLineSwitch', mac)
        self.assertIn('--use-mock-keychain', mac)
        self.assertNotIn('--use-mock-keychain', linux)
        for arguments in (mac, linux):
            self.assertIn('--load-extension=/e', arguments)
            self.assertEqual(arguments[-1], 'about:blank')


if __name__ == '__main__':
    unittest.main()
