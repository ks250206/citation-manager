#!/usr/bin/env python3
"""Sign `Citation Manager.app` inside-out (#34 ad-hoc; Developer ID is #31).

Order: every helper executable in Contents/MacOS, then the bundle (which signs the main
executable and seals Info.plist/Resources). `Frameworks/libpdfium.dylib` is NOT re-signed by
default: it keeps the upstream bytes (and upstream ad-hoc signature) because the PDF worker
only loads a dylib whose SHA-256 equals the pinned original (docs/ADR/pdfium-runtime.md).
`--sign-pdfium` re-signs it anyway, which is required for notarization and is expected to
make the bundled PDFium fail the pinned-hash gate until #31 changes the trust policy.

  python3 scripts/package/macos/codesign.py APP                    # ad-hoc
  python3 scripts/package/macos/codesign.py APP --hardened-runtime # ad-hoc + hardened runtime

The identity is "-" (ad-hoc). Ad-hoc signatures carry no Team ID and are never accepted by
Gatekeeper for a downloaded app; they only make the bundle structurally verifiable.
"""
import argparse
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import build_app  # noqa: E402


def plan(app, identity='-', hardened_runtime=False, sign_pdfium=False, timestamp=False,
         entitlements=None):
    """The codesign invocations, inside-out. Pure: unit-tested without codesign."""
    app = Path(app)
    contents = app / 'Contents'
    base = ['codesign', '--force', '--sign', identity]
    base += ['--timestamp'] if timestamp else ['--timestamp=none']
    if hardened_runtime:
        base += ['--options', 'runtime']
    steps = []
    if sign_pdfium:
        steps.append(base + [str(contents / build_app.PDFIUM)])
    executable_base = base + (['--entitlements', str(entitlements)] if entitlements else [])
    for name in build_app.EXECUTABLES:
        if name != build_app.MAIN_EXECUTABLE:
            steps.append(executable_base + [str(contents / 'MacOS' / name)])
    steps.append(executable_base + [str(app)])
    return steps


def verify_command(app):
    return ['codesign', '--verify', '--deep', '--strict', '--verbose=2', str(app)]


def sign(app, **options):
    for step in plan(app, **options):
        subprocess.run(step, check=True)
    subprocess.run(verify_command(app), check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('app', type=Path)
    parser.add_argument('--hardened-runtime', action='store_true')
    parser.add_argument('--sign-pdfium', action='store_true',
                        help='also re-sign the PDFium dylib (changes its hash; see above)')
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('codesign is only available on macOS')
    sign(args.app, hardened_runtime=args.hardened_runtime, sign_pdfium=args.sign_pdfium)


if __name__ == '__main__':
    main()
