import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import generate_library_fixtures as generator

class LibraryFixtures(unittest.TestCase):
    def test_original_pdf_fixture_hashes_and_reproduction(self):
        source = Path(__file__).resolve().parents[1] / 'fixtures/library'
        manifest = json.loads((source / 'manifest.json').read_text())
        for name, digest in manifest['files'].items():
            self.assertEqual(hashlib.sha256((source / name).read_bytes()).hexdigest(), digest)
        with tempfile.TemporaryDirectory() as root:
            destination = Path(root) / 'library'
            with patch.object(generator, 'OUT', destination):
                generator.main()
            for name in [*manifest['files'], 'manifest.json']:
                self.assertEqual((source / name).read_bytes(), (destination / name).read_bytes())
