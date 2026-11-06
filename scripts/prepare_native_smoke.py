#!/usr/bin/env python3
"""Prepare an unsigned throwaway .app so macOS/CUA can address the native binary.

No GUI launch, database open, user-library access, signing or packaging claim.
"""
import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import shlex
import shutil
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--executable', type=Path, default=Path(__file__).resolve().parents[1] / 'target/debug/citation-manager')
    parser.add_argument('--deny-network', action='store_true', help='Deny IP traffic in this smoke app process only (macOS sandbox-exec).')
    args = parser.parse_args()
    executable = args.executable.resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix='cm-m0-smoke-')).resolve()
    bundle = root / 'Citation Manager Smoke.app'
    binary_dir = bundle / 'Contents/MacOS'
    binary_dir.mkdir(parents=True)
    shutil.copy2(executable, binary_dir / 'citation-manager')
    if args.deny_network:
        launcher = binary_dir / 'launch-smoke'
        launcher.write_text("#!/bin/sh\n" + "exec /usr/bin/sandbox-exec -p '(version 1)(allow default)(deny network-outbound (remote ip \"*:*\"))(deny network-inbound (local ip \"*:*\"))' \"$(dirname \"$0\")/citation-manager\" \"$@\"\n")
        launcher.chmod(0o700)
    with (bundle / 'Contents/Info.plist').open('wb') as output:
        plistlib.dump({
            'CFBundleExecutable': 'launch-smoke' if args.deny_network else 'citation-manager',
            'CFBundleIdentifier': 'dev.citation-manager.issue10-smoke',
            'CFBundleName': 'Citation Manager Smoke',
            'CFBundlePackageType': 'APPL',
            'NSHighResolutionCapable': True,
        }, output)
    manifest = {
        'bundle': str(bundle), 'library': str(root / 'library'),
        'ip_network_denied': args.deny_network,
        'executable_sha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
    }
    (root / 'smoke.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print(f'Manifest: {root / "smoke.json"}')
    print(shlex.join(['open', '-n', '-W', str(bundle), '--args', '--library', manifest['library']]))


if __name__ == '__main__':
    main()
