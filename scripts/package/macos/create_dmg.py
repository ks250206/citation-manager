#!/usr/bin/env python3
"""Create the distribution disk image for `Citation Manager.app` (#31).

The image holds the app and an `Applications` symlink for drag-and-drop installation.
It is a compressed, read-only UDZO image; `hdiutil verify` checks its checksum. Signing and
notarizing the image is release_sign.py's job; this script never signs.

  python3 scripts/package/macos/create_dmg.py --app "<dir>/Citation Manager.app" \\
      --out "<dir>/Citation-Manager-0.1.0-arm64.dmg"
"""
import argparse
from pathlib import Path
import subprocess
import sys
import tempfile

VOLUME_NAME = 'Citation Manager'


def plan(app, staging, dmg, volume=VOLUME_NAME):
    """hdiutil/ditto invocations (pure; unit-tested)."""
    app, staging, dmg = Path(app), Path(staging), Path(dmg)
    return [
        ['ditto', str(app), str(staging / app.name)],
        ['ln', '-s', '/Applications', str(staging / 'Applications')],
        ['hdiutil', 'create', '-volname', volume, '-srcfolder', str(staging), '-fs', 'HFS+',
         '-format', 'UDZO', '-imagekey', 'zlib-level=9', '-ov', str(dmg)],
        ['hdiutil', 'verify', str(dmg)],
    ]


def build(app, dmg, volume=VOLUME_NAME):
    dmg = Path(dmg)
    dmg.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='cm-dmg-') as staging:
        for step in plan(app, Path(staging) / 'root', dmg, volume):
            if step[0] == 'ditto':
                (Path(staging) / 'root').mkdir()
            subprocess.run(step, check=True)
    return dmg


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--app', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('hdiutil is only available on macOS')
    print(build(args.app, args.out))


if __name__ == '__main__':
    main()
