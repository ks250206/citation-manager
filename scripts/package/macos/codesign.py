#!/usr/bin/env python3
"""Sign `Citation Manager.app` inside-out (#34 ad-hoc; Developer ID via release_sign.py, #31).

A signed bundle needs a real signature on every nested Mach-O: `codesign` refuses to seal a
bundle whose `Frameworks/libpdfium.dylib` only has the upstream linker signature ("code
object is not signed at all"). Signing rewrites the dylib, and the PDF worker loads only a
dylib whose SHA-256 is pinned at compile time, so the order is:

  1. `codesign.py pdfium SRC DEST` signs a copy of the verified upstream dylib and prints
     its new SHA-256;
  2. the release binaries are built with `CM_PDFIUM_SIGNED_SHA256=<that digest>`
     (cm_pdf::worker::PDFIUM_SIGNED_SHA256) — they then accept the upstream and the signed
     copy, nothing else;
  3. build_app.py --signed-pdfium DEST --signed-pdfium-sha256 <digest> assembles the bundle;
  4. `codesign.py app APP` signs the helper executables, then the bundle (main executable,
     Info.plist and Resources sealed). The already signed dylib is left as is.

The identity is "-" (ad-hoc): no Team ID, never accepted by Gatekeeper for a downloaded app;
it only makes the bundle structurally verifiable. `--hardened-runtime` adds
`--options runtime` to the executables.
"""
import argparse
import hashlib
from pathlib import Path
import shutil
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import build_app  # noqa: E402


def base_command(identity='-', hardened_runtime=False, timestamp=False):
    command = ['codesign', '--force', '--sign', identity]
    command += ['--timestamp'] if timestamp else ['--timestamp=none']
    if hardened_runtime:
        command += ['--options', 'runtime']
    return command


def plan(app, identity='-', hardened_runtime=False, sign_pdfium=False, timestamp=False,
         entitlements=None):
    """The codesign invocations for a bundle, inside-out. Pure: unit-tested without codesign."""
    app = Path(app)
    contents = app / 'Contents'
    base = base_command(identity, hardened_runtime, timestamp)
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


def sign_pdfium_copy(source, destination, identity='-', timestamp=False, keychain=None):
    """Sign a copy of the pinned upstream dylib; return the SHA-256 of the signed copy."""
    source, destination = Path(source), Path(destination)
    pin = build_app.pdfium.artifact('macos')
    if build_app.sha256(source) != pin['library_sha256']:
        raise ValueError(f'{source}: not the pinned upstream PDFium')
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)
    command = base_command(identity, timestamp=timestamp)
    if keychain:
        command += ['--keychain', str(keychain)]
    subprocess.run(command + [str(destination)], check=True)
    subprocess.run(['codesign', '--verify', '--strict', str(destination)], check=True)
    return hashlib.sha256(destination.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command', required=True)
    app = commands.add_parser('app', help='sign the helpers and the bundle (ad-hoc)')
    app.add_argument('app', type=Path)
    app.add_argument('--hardened-runtime', action='store_true')
    app.add_argument('--entitlements', type=Path)
    app.add_argument('--sign-pdfium', action='store_true',
                     help='also (re-)sign the bundled dylib; its digest then no longer matches the build-time pin')
    pdfium = commands.add_parser('pdfium', help='sign a copy of the upstream dylib, print its SHA-256')
    pdfium.add_argument('source', type=Path)
    pdfium.add_argument('destination', type=Path)
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('codesign is only available on macOS')
    if args.command == 'pdfium':
        print(sign_pdfium_copy(args.source, args.destination))
    else:
        sign(args.app, hardened_runtime=args.hardened_runtime, sign_pdfium=args.sign_pdfium,
             entitlements=args.entitlements)


if __name__ == '__main__':
    main()
