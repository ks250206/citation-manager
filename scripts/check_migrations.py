#!/usr/bin/env python3
"""Validate immutable migration identities/checksums. Does not execute SQL (#8)."""
import argparse
from datetime import datetime
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]
DIRECTORY = Path('crates/storage/migrations')
MANIFEST = DIRECTORY / 'manifest.json'
NAME = re.compile(r'(\d{8}T\d{6}Z)_issue-([1-9]\d*)_(\d{3})_([a-z][a-z0-9-]*)\.sql\Z')

def load_manifest(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f'duplicate manifest key: {key}')
            result[key] = value
        return result
    result = json.loads(raw, object_pairs_hook=unique)
    if not isinstance(result, dict) or any(not isinstance(v, str) or not re.fullmatch('[0-9a-f]{64}', v) for v in result.values()):
        raise ValueError('manifest must map migration filenames to lowercase SHA256 strings')
    return result

def validate(directory, entries, previous=None):
    paths = list(directory.iterdir())
    if any(p.is_symlink() or not p.is_file() for p in paths):
        raise ValueError('migration directory must contain only regular files, no symlink or subdirectory')
    files = {p.name: p for p in paths if p.name != 'manifest.json'}
    if set(files) != set(entries):
        raise ValueError('migration files and manifest differ')
    identities = set()
    for name, checksum in entries.items():
        match = NAME.fullmatch(name)
        if not match:
            raise ValueError(f'invalid migration filename: {name}')
        timestamp, issue, sequence, _ = match.groups()
        datetime.strptime(timestamp, '%Y%m%dT%H%M%SZ')
        if sequence == '000':
            raise ValueError('migration sequence starts at 001')
        identity = (timestamp, issue, sequence)
        if identity in identities:
            raise ValueError(f'duplicate migration ID: {identity}')
        identities.add(identity)
        path = files[name]
        if path.is_symlink() or hashlib.sha256(path.read_bytes()).hexdigest() != checksum:
            raise ValueError(f'migration checksum mismatch or symlink: {name}')
    for name, checksum in (previous or {}).items():
        if entries.get(name) != checksum:
            raise ValueError(f'existing migration changed or deleted: {name}')

def baseline_manifest(root, base):
    # Fail closed if the base itself is unavailable. An initial base may lack a registry.
    subprocess.run(['git', 'rev-parse', '--verify', f'{base}^{{commit}}'], cwd=root, check=True, capture_output=True)
    listing = subprocess.run(['git', 'ls-tree', '--name-only', base, '--', str(MANIFEST)],
                             cwd=root, check=True, text=True, capture_output=True)
    if not listing.stdout.strip():
        return {}
    result = subprocess.run(['git', 'show', f'{base}:{MANIFEST}'], cwd=root, check=True, text=True, capture_output=True)
    return load_manifest(result.stdout)

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', help='immutable comparison base, e.g. origin/main or PR base SHA')
    args = parser.parse_args()
    if (ROOT / MANIFEST).is_symlink():
        raise ValueError('manifest must not be a symlink')
    entries = load_manifest((ROOT / MANIFEST).read_text())
    previous = baseline_manifest(ROOT, args.base) if args.base else None
    validate(ROOT / DIRECTORY, entries, previous)
    print(f'Migrations: {len(entries)} checksums/IDs valid' + ('; baseline preserved' if args.base else ''))
