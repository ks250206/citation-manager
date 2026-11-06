#!/usr/bin/env python3
"""Fetch exact macOS universal PDFium, never latest; fail closed on hash/version/license drift."""
import hashlib
import json
from pathlib import Path, PurePosixPath
import sys
import tarfile
import urllib.request

if not __debug__:
    raise RuntimeError('verification requires assertions enabled; do not run Python with -O')

ROOT = Path(__file__).resolve().parents[2]
LOCK = json.loads((Path(__file__).with_name('runtime-lock.json')).read_text())

def sha(data):
    return hashlib.sha256(data).hexdigest()

def fetch(destination):
    destination.mkdir(parents=True, exist_ok=True)
    archive = destination/'pdfium-mac-univ.tgz'
    if not archive.exists():
        with urllib.request.urlopen(LOCK['url'], timeout=60) as response:
            data = response.read(32*1024*1024+1)
        if len(data) > 32*1024*1024 or sha(data) != LOCK['artifact_sha256']:
            raise ValueError('download size/checksum mismatch')
        archive.write_bytes(data)
    if sha(archive.read_bytes()) != LOCK['artifact_sha256']:
        raise ValueError('cached archive checksum mismatch')
    with tarfile.open(archive, 'r:gz') as tar:
        for member in tar.getmembers():
            path = PurePosixPath(member.name)
            if path.is_absolute() or '..' in path.parts or member.issym() or member.islnk() or not (member.isdir() or member.isfile()):
                raise ValueError('unsafe tar member')
        tar.extractall(destination, filter='data')
    library = destination/LOCK['library']
    assert sha(library.read_bytes()) == LOCK['library_sha256'], 'library checksum mismatch'
    assert (destination/'VERSION').read_text() == 'MAJOR=158\nMINOR=0\nBUILD=8125\nPATCH=0\n'
    assert (destination/'LICENSE').is_file() and len(list((destination/'licenses').iterdir())) == 18
    print(library.resolve())

if __name__ == '__main__':
    fetch(Path(sys.argv[1]) if len(sys.argv)>1 else ROOT/'target/pdfium-poc')
