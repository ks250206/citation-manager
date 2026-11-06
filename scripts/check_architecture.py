#!/usr/bin/env python3
"""Validate the current 12-crate boundary from Cargo metadata, including target/dev deps."""
import json
from pathlib import Path
import subprocess

# Allowed edges, not required imports: feature PRs own additive edges within this boundary.
ALLOWED = {
    'domain': set(), 'core': {'domain'},
    'storage': {'domain', 'core'}, 'search': {'domain', 'core'},
    'pdf': {'domain', 'core'}, 'metadata': {'domain', 'core'},
    'protocol': {'domain', 'core'}, 'desktop': {'protocol'},
    'daemon': {'core', 'protocol', 'storage', 'search', 'pdf', 'metadata'},
    'cli': {'protocol', 'mcp'}, 'mcp': {'protocol'}, 'native-host': {'protocol'},
}
ROOT = Path(__file__).resolve().parents[1]

def validate(metadata):
    packages = {p['id']: p for p in metadata['packages']}
    members = {packages[i]['name']: packages[i] for i in metadata['workspace_members']}
    expected = {'cm-' + name for name in ALLOWED}
    if set(members) != expected:
        raise ValueError(f'workspace must contain exactly the 12 planned crates: {set(members) ^ expected}')
    for name, package in members.items():
        short = name.removeprefix('cm-')
        for dep in package['dependencies']:
            target = dep['name']
            if target.startswith('cm-'):
                if target not in members or target.removeprefix('cm-') not in ALLOWED[short]:
                    raise ValueError(f'forbidden dependency: {name} -> {target}')
            elif short in {'domain', 'core', 'protocol'}:
                # Core/protocol must not inherit an implementation framework via feature/dev deps.
                allowed_external = {'serde', 'serde_json', 'uuid'}
                if target not in allowed_external:
                    raise ValueError(f'implementation dependency in {name}: {target}')
            if target in {'gpui', 'gpui_platform'} and short != 'desktop':
                raise ValueError(f'GPUI must stay in desktop: {name}')
            if target == 'rusqlite' and short != 'storage':
                raise ValueError(f'SQLite driver must stay in storage: {name}')
            if target == 'pdfium-render' and short != 'pdf':
                raise ValueError(f'PDFium must stay in pdf: {name}')
        if short == 'desktop':
            for dep in package['dependencies']:
                if dep['name'] in {'gpui', 'gpui_platform'}:
                    expected_source = 'git+https://github.com/zed-industries/zed?rev=4a6121bb0f10c6e8037b741b7dc0a4cd639ae289'
                    if dep['source'] != expected_source or not dep['optional'] or dep['target'] != 'cfg(target_os = "macos")':
                        raise ValueError('GPUI must use the pinned optional macOS lane')

if __name__ == '__main__':
    result = subprocess.run(['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'],
                            cwd=ROOT, check=True, text=True, capture_output=True)
    validate(json.loads(result.stdout))
    print('Architecture: 12 crates, allowed dependency direction and platform isolation passed')
