"""Safety checks for fixture packaging/registration; not browser or native runtime proof."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]

def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT/f'scripts/package/layout-smoke/{name}.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

bundle = load('bundle')
register = load('register_fixture')

class LayoutSafety(unittest.TestCase):
    def test_absolute_unicode_manifest_and_move_requires_new_registration(self):
        with tempfile.TemporaryDirectory(dir='/private/tmp') as tmp:
            root = Path(tmp)
            app = root/'空白 App.app'
            host = app/'Contents/MacOS/cm34-probe-host'
            host.parent.mkdir(parents=True)
            host.write_bytes(b'test-only placeholder')
            directory = root/'registration';directory.mkdir()
            old = register.write(app, 'chromium', 'test.cm34', 'a'*32, directory)
            data = json.loads(old.read_text())
            self.assertEqual(data['path'], str(host))
            self.assertEqual(data['allowed_origins'], ['chrome-extension://'+'a'*32+'/'])
            with self.assertRaises(FileExistsError):
                register.write(app, 'chromium', 'test.cm34', 'a'*32, directory)
            moved = root/'移設 App.app';app.rename(moved)
            self.assertFalse(Path(data['path']).exists())
            fresh = register.manifest(moved, 'firefox', 'test.cm34', 'fixture@invalid')
            self.assertEqual(fresh['path'], str(moved/'Contents/MacOS/cm34-probe-host'))
            self.assertEqual(fresh['allowed_extensions'], ['fixture@invalid'])
            with self.assertRaises(ValueError):
                register.write(moved, 'firefox', 'test.cm34', 'fixture@invalid', Path.home())
            for invalid in ['../escape', 'a..b', '.leading', 'Uppercase']:
                with self.assertRaises(ValueError):
                    register.manifest(moved, 'firefox', invalid, 'fixture@invalid')

    def test_inventory_refuses_linked_resources_and_fetch_rejects_drift(self):
        with tempfile.TemporaryDirectory(dir='/private/tmp') as tmp:
            root = Path(tmp);app=root/'Test.app';res=app/'Contents/Resources'
            res.mkdir(parents=True);(app/'Contents/MacOS').mkdir()
            outside=root/'outside';outside.write_bytes(b'not a resource')
            (res/'linked').symlink_to(outside)
            with self.assertRaises(ValueError): bundle.refresh_inventory(app)
            runtime=root/'runtime/lib';runtime.mkdir(parents=True)
            (runtime/'libpdfium.dylib').write_bytes(b'not pinned PDFium')
            with self.assertRaisesRegex(ValueError, 'PDFium checksum'):
                bundle.build(root/'new', runtime.parent, outside, outside)
            self.assertFalse((root/'new').exists())

if __name__ == '__main__': unittest.main()
