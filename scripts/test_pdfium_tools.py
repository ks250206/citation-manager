#!/usr/bin/env python3
"""Default lane: no native/Python PDF dependencies. Pins, fixture/notice integrity, unsafe fetches."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('pdfium_fetch', ROOT/'scripts/pdfium/fetch.py')
fetch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fetch)

class PdfiumPins(unittest.TestCase):
    def test_committed_corpus_and_license_integrity(self):
        manifest=json.loads((ROOT/'fixtures/pdf/manifest.json').read_text())
        for name,digest in manifest['files'].items():
            self.assertEqual(hashlib.sha256((ROOT/'fixtures/pdf'/name).read_bytes()).hexdigest(),digest)
        notices=ROOT/'docs/ADR/pdfium-licenses'
        manifest=json.loads((notices/'manifest.json').read_text())
        self.assertEqual(set(manifest['files']),{p.name for p in notices.iterdir()}-{'manifest.json'})
        for name,digest in manifest['files'].items():
            self.assertEqual(hashlib.sha256((notices/name).read_bytes()).hexdigest(),digest)
        lock=fetch.LOCK
        rust=(ROOT/'crates/pdf/src/worker.rs').read_text()
        self.assertIn(lock['library_sha256'],rust)
        self.assertIn(lock['build'],rust)
        self.assertEqual(manifest['artifact_sha256'],lock['artifact_sha256'])

    def test_cached_tampered_artifact_fails_before_extract(self):
        with tempfile.TemporaryDirectory() as directory:
            p=Path(directory)
            (p/'pdfium-mac-univ.tgz').write_bytes(b'not the pinned archive')
            with self.assertRaisesRegex(ValueError,'checksum'):
                fetch.fetch(p)
            self.assertFalse((p/'lib').exists())

    def test_fetched_upstream_notices_require_exact_contents_and_file_set(self):
        with tempfile.TemporaryDirectory() as directory:
            p=Path(directory)
            (p/'licenses').mkdir()
            (p/'LICENSE').write_bytes(b'license')
            (p/'licenses/notice.txt').write_bytes(b'notice')
            expected={'LICENSE':hashlib.sha256(b'license').hexdigest(),
                      'notice.txt':hashlib.sha256(b'notice').hexdigest()}
            with patch.dict(fetch.NOTICE_MANIFEST,upstream_files_sha256=expected):
                fetch.verify_notices(p)
                (p/'licenses/notice.txt').write_bytes(b'modified')
                with self.assertRaisesRegex(ValueError,'checksum'):
                    fetch.verify_notices(p)
                (p/'licenses/notice.txt').write_bytes(b'notice')
                (p/'licenses/extra.txt').write_bytes(b'extra')
                with self.assertRaisesRegex(ValueError,'file set'):
                    fetch.verify_notices(p)
                (p/'licenses/extra.txt').unlink()
                (p/'LICENSE').unlink()
                with self.assertRaisesRegex(ValueError,'checksum'):
                    fetch.verify_notices(p)

    def test_tar_traversal_symlink_and_devices_fail_closed(self):
        for name,kind in [('../outside',tarfile.REGTYPE),('/absolute',tarfile.REGTYPE),('alias',tarfile.SYMTYPE),('device',tarfile.CHRTYPE)]:
            data=io.BytesIO()
            with tarfile.open(fileobj=data,mode='w:gz') as archive:
                info=tarfile.TarInfo(name);info.type=kind;info.linkname='/tmp/outside'
                archive.addfile(info)
            binary=data.getvalue()
            with tempfile.TemporaryDirectory() as directory, patch.dict(fetch.LOCK,artifact_sha256=hashlib.sha256(binary).hexdigest()), patch.object(fetch.urllib.request,'urlopen',return_value=io.BytesIO(binary)):
                with self.assertRaisesRegex(ValueError,'unsafe tar'):
                    fetch.fetch(Path(directory))

if __name__ == '__main__':
    unittest.main()
