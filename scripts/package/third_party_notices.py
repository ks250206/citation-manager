#!/usr/bin/env python3
"""Generate THIRD_PARTY_NOTICES.md for the shipped binaries from Cargo metadata (#31).

The dependency set is what the release binaries link: the normal (not dev/build)
dependency closure of cm-daemon (with desktop-bootstrap, i.e. GPUI + PDFium bindings),
cm-cli, cm-mcp and cm-native-host, resolved for the macOS target. For every crate the
SPDX expression, source and every license/notice file shipped in the crate package are
included verbatim (identical texts are printed once). Crates that ship no license file are
listed explicitly; they are not silently covered. PDFium and its 18 upstream notices are
bundled separately in Resources/licenses/pdfium/ and referenced from the output.

  python3 scripts/package/third_party_notices.py --out DIR [--target aarch64-apple-darwin]

writes DIR/THIRD_PARTY_NOTICES.md and DIR/third-party-licenses.json, and exits 1 when a
crate's license expression offers only copyleft (GPL/LGPL/AGPL) alternatives or none.
Needs the crate sources locally (run after `cargo build`/`cargo fetch`).
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
SHIPPED = ('cm-daemon', 'cm-cli', 'cm-mcp', 'cm-native-host')
FEATURES = ('cm-daemon/desktop-bootstrap',)
LICENSE_FILE = re.compile(r'^(LICEN[CS]E|COPYING|NOTICE|UNLICENSE|COPYRIGHT|AUTHORS)([-_.].*)?$', re.I)
DENIED = re.compile(r'\b(A?GPL|LGPL)-')


def cargo_metadata(target):
    command = ['cargo', 'metadata', '--format-version', '1', '--locked', '--filter-platform', target]
    for feature in FEATURES:
        command += ['--features', feature]
    return json.loads(subprocess.run(command, cwd=ROOT, check=True, capture_output=True, text=True).stdout)


def shipped_packages(metadata, roots=SHIPPED):
    """Third-party packages in the normal-dependency closure of the shipped workspace crates."""
    packages = {p['id']: p for p in metadata['packages']}
    nodes = {n['id']: n for n in metadata['resolve']['nodes']}
    members = set(metadata['workspace_members'])
    stack = [i for i in members if packages[i]['name'] in roots]
    missing = set(roots) - {packages[i]['name'] for i in stack}
    if missing:
        raise ValueError(f'shipped crates not in the workspace: {sorted(missing)}')
    seen = set()
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        for dep in nodes[current]['deps']:
            if any(kind['kind'] is None for kind in dep['dep_kinds']):
                stack.append(dep['pkg'])
    third = [packages[i] for i in seen - members]
    return sorted(third, key=lambda p: (p['name'], p['version']))


def license_files(package):
    directory = Path(package['manifest_path']).parent
    files = []
    if package.get('license_file'):
        files.append(directory / package['license_file'])
    if directory.is_dir():
        for entry in sorted(os.listdir(directory)):
            if LICENSE_FILE.match(entry):
                files.append(directory / entry)
    unique, seen = [], set()
    for path in files:
        resolved = path.resolve()
        if resolved.is_file() and resolved not in seen:
            seen.add(resolved)
            unique.append(path)
    return unique


def only_copyleft(expression):
    """True when no OR-alternative of the SPDX expression avoids GPL/LGPL/AGPL (or none given)."""
    if not expression:
        return True
    alternatives = [a.strip(' ()') for a in re.split(r'\s+OR\s+|/', expression.replace('(', ' ').replace(')', ' '))]
    return all(DENIED.search(a) for a in alternatives if a)


def source_of(package):
    source = package.get('source') or ''
    if source.startswith('registry+'):
        return 'crates.io'
    if source.startswith('git+'):
        return source[4:]
    return package.get('repository') or source or 'local'


def render(packages):
    texts = {}   # sha256 -> (text, [crate labels])
    rows, without_file, violations = [], [], []
    for package in packages:
        label = f"{package['name']} {package['version']}"
        files = license_files(package)
        if not files:
            without_file.append(package)
        if only_copyleft(package.get('license')):
            violations.append(label)
        names = []
        for path in files:
            text = path.read_text(encoding='utf-8', errors='replace').strip() + '\n'
            digest = hashlib.sha256(text.encode()).hexdigest()
            texts.setdefault(digest, (text, []))[1].append(f'{label} ({path.name})')
            names.append(path.name)
        rows.append({'name': package['name'], 'version': package['version'],
                     'license': package.get('license'), 'source': source_of(package),
                     'repository': package.get('repository'), 'license_files': names})
    lines = ['# Third-party notices', '',
             'Citation Manager bundles the Rust crates listed below (the normal dependency closure of',
             '`citation-manager`, `papersd`, `papers`, `papers-mcp` and `papers-native-host`, generated',
             'from `Cargo.lock` by `scripts/package/third_party_notices.py`) and PDFium.', '',
             '**PDFium** (chromium/8125, BSD-3-Clause) and its third-party components (FreeType, ICU,',
             'libjpeg-turbo, OpenJPEG, libpng, zlib, lcms, HarfBuzz, abseil, ...): the full license and',
             'every upstream notice are in `licenses/pdfium/` next to this file.', '',
             f'## Crates ({len(rows)})', '', '| Crate | Version | License | Source |', '|---|---|---|---|']
    for row in rows:
        lines.append(f"| {row['name']} | {row['version']} | {row['license'] or '(none declared)'} | {row['source']} |")
    if without_file:
        lines += ['', f'## Crates without a license file in their package ({len(without_file)})', '',
                  'These crates declare the SPDX license above but ship no license text. The standard',
                  'text of that license applies; it is reproduced below where another crate ships it.', '']
        lines += [f"- {p['name']} {p['version']}: {p.get('license') or '(none declared)'}" for p in without_file]
    lines += ['', '## License texts', '']
    for digest, (text, users) in sorted(texts.items(), key=lambda item: item[1][1][0]):
        lines += [f'### Used by: {", ".join(users)}', '', '```text', text.rstrip('\n').replace('```', "'''"), '```', '']
    summary = {'crates': rows, 'without_license_file': [f"{p['name']} {p['version']}" for p in without_file],
               'copyleft_only': violations, 'distinct_texts': len(texts)}
    return '\n'.join(lines) + '\n', summary


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--target', default='aarch64-apple-darwin')
    parser.add_argument('--metadata', type=Path, help='use a saved `cargo metadata` JSON instead')
    args = parser.parse_args()
    metadata = json.loads(args.metadata.read_text()) if args.metadata else cargo_metadata(args.target)
    notices, summary = render(shipped_packages(metadata))
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / 'THIRD_PARTY_NOTICES.md').write_text(notices)
    (args.out / 'third-party-licenses.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(f"{len(summary['crates'])} crates, {summary['distinct_texts']} distinct license texts, "
          f"{len(summary['without_license_file'])} without a license file")
    if summary['copyleft_only']:
        print('copyleft-only or undeclared licenses: ' + ', '.join(summary['copyleft_only']), file=sys.stderr)
        sys.exit(1)


if __name__ == '__main__':
    main()
