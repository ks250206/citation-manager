#!/usr/bin/env python3
"""Build an unsigned test .app outside the checkout. No install or user registration."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import secrets
import shutil
import tempfile

ROOT = Path(__file__).resolve().parents[3]
LAYOUT = ROOT / 'packaging/macos/layout'
LOCK = json.loads((ROOT / 'scripts/pdfium/runtime-lock.json').read_text())

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def refresh_inventory(bundle):
    contents = bundle / 'Contents'
    entries = {}
    for base in [contents/'Resources', contents/'MacOS']:
        for path in sorted(base.rglob('*')):
            if path.is_symlink():
                raise ValueError('symlinks are not bundle resources')
            if path.is_file() and path.name not in {'layout-integrity.json', 'citation-manager'}:
                entries[str(path.relative_to(contents))] = digest(path)
    (contents/'Resources/layout-integrity.json').write_text(json.dumps(entries, indent=2)+'\n')
    return entries

def build(destination, runtime, binary, probe):
    destination = destination.parent.resolve() / destination.name
    if destination.exists() or destination.is_relative_to(ROOT) or not destination.parent.is_dir():
        raise ValueError('destination must be new, absolute and outside checkout')
    if digest(runtime/LOCK['library']) != LOCK['library_sha256']:
        raise ValueError('pinned original PDFium checksum mismatch')
    spec = importlib.util.spec_from_file_location('pdfium_fetch', ROOT/'scripts/pdfium/fetch.py')
    fetch = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fetch)
    fetch.verify_notices(runtime)
    destination.mkdir(mode=0o700)
    private = Path(tempfile.mkdtemp(prefix='cm34-', dir='/private/tmp')).resolve()
    nonce = secrets.token_hex(12)
    (private/'nonce').write_text(nonce)
    os.chmod(private/'nonce', 0o600)
    bundle = destination/'Citation Layout 日本語.app'
    contents = bundle/'Contents'
    macos = contents/'MacOS'; macos.mkdir(parents=True)
    resources = contents/'Resources'; resources.mkdir()
    frameworks = contents/'Frameworks'; frameworks.mkdir()
    shutil.copy2(binary, macos/'citation-manager')
    for helper in ['cm34-probe-host', 'cm34-probe-daemon']:
        shutil.copy2(probe, macos/helper)
    shutil.copy2(runtime/LOCK['library'], frameworks/'libpdfium.dylib')
    shutil.copy2(ROOT/'fixtures/pdf/corpus.pdf', resources/'layout-fixture.pdf')
    shutil.copytree(LAYOUT/'licenses', resources/'licenses')
    shutil.copytree(LAYOUT/'host-manifests', resources/'host-manifests')
    pdfium = resources/'licenses/pdfium'; pdfium.mkdir()
    shutil.copy2(runtime/'LICENSE', pdfium/'LICENSE')
    shutil.copytree(runtime/'licenses', pdfium/'third-party')
    for name in ['CC0-1.0.txt','OFL-NotoSansJP.txt']:
        shutil.copy2(ROOT/'fixtures/pdf'/name, resources/'licenses'/name)
    shutil.copy2(ROOT/'docs/licenses/GPUI-APACHE-2.0.txt', resources/'licenses/GPUI-APACHE-2.0.txt')
    (resources/'lifecycle-config.txt').write_text(nonce+'\n'+str(private)+'\n')
    with (contents/'Info.plist').open('wb') as f:
        plistlib.dump({'CFBundleName':'Citation Layout Probe', 'CFBundleDisplayName':'Citation Layout Probe',
          'CFBundleIdentifier':'test.citationmanager.layout.'+nonce,
          'CFBundleExecutable':'citation-manager','CFBundlePackageType':'APPL',
          'CFBundleVersion':'34','NSHighResolutionCapable':True,'LSMinimumSystemVersion':'13.0'}, f)
    refresh_inventory(bundle)
    result = {'bundle':str(bundle),'private':str(private),'nonce':nonce,
      'executable_sha256':digest(macos/'citation-manager'),'helper_sha256':digest(macos/'cm34-probe-host'),
      'pdfium_sha256':digest(frameworks/'libpdfium.dylib')}
    (destination/'bundle.json').write_text(json.dumps(result, indent=2)+'\n')
    return result

if __name__ == '__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--out',type=Path,required=True)
    p.add_argument('--runtime',type=Path,required=True)
    p.add_argument('--binary',type=Path,default=ROOT/'target/debug/citation-manager')
    p.add_argument('--probe',type=Path,required=True)
    a=p.parse_args()
    print(json.dumps(build(a.out,a.runtime,a.binary,a.probe),indent=2))
