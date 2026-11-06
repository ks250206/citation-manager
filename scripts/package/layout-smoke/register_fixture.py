#!/usr/bin/env python3
"""Generate a nonce fixture registration ONLY below an explicitly chosen temp directory.

No installer, real-user registration or preferences changes. Regenerate after a move.
"""
import argparse
import json
from pathlib import Path
import re
import tempfile

ROOT=Path(__file__).resolve().parents[3]

def manifest(bundle,kind,name,extension_id):
    bundle=bundle.resolve(strict=True)
    host=bundle/'Contents/MacOS/cm34-probe-host'
    if host.is_symlink() or not host.is_file(): raise ValueError('missing/unsafe bundled host')
    if not re.fullmatch(r'[a-z0-9_]+(?:\.[a-z0-9_]+)+',name): raise ValueError('invalid host name')
    if kind=='chromium' and not re.fullmatch('[a-p]{32}',extension_id): raise ValueError('invalid Chromium extension ID')
    if kind=='firefox' and (not extension_id or len(extension_id)>128): raise ValueError('invalid Firefox extension ID')
    data=json.loads((ROOT/f'packaging/macos/layout/host-manifests/{kind}.json.in').read_text())
    data['name']=name;data['path']=str(host)
    if kind=='chromium': data['allowed_origins']=['chrome-extension://'+extension_id+'/']
    else: data['allowed_extensions']=[extension_id]
    return data

def write(bundle,kind,name,extension_id,directory):
    directory=directory.resolve(strict=True)
    # Explicit temp area only. Never infer installation/user Application Support.
    roots=[Path('/private/tmp'),Path(tempfile.gettempdir()).resolve()]
    if not any(directory.is_relative_to(r) and directory!=r for r in roots):
        raise ValueError('fixture manifests may only be written to a dedicated temp subdirectory')
    result=manifest(bundle,kind,name,extension_id)
    path=directory/(name+'.json')
    with path.open('x') as f: json.dump(result,f,indent=2,ensure_ascii=False);f.write('\n')
    return path

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--bundle',type=Path,required=True);p.add_argument('--kind',choices=['chromium','firefox'],required=True)
    p.add_argument('--name',required=True);p.add_argument('--extension-id',required=True);p.add_argument('--directory',type=Path,required=True)
    a=p.parse_args();print(write(a.bundle,a.kind,a.name,a.extension_id,a.directory))
