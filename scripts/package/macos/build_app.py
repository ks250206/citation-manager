#!/usr/bin/env python3
"""Assemble the unsigned `Citation Manager.app` from release binaries and the pinned PDFium (#34).

Layout (docs/ADR/app-layout.md; what `cm_pdf::worker::bundled_library` and the sibling
`papersd` lookups expect):

  Citation Manager.app/Contents/
    Info.plist, PkgInfo
    MacOS/citation-manager        CFBundleExecutable: GPUI app (cm-daemon, desktop-bootstrap)
    MacOS/papersd                 per-library daemon, started on demand by every client
    MacOS/papers                  CLI (and `papers mcp`)
    MacOS/papers-mcp              MCP stdio server and its grant bootstrap
    MacOS/papers-native-host      browser Native Messaging host
    Frameworks/libpdfium.dylib    chromium/8125 universal, upstream bytes (sha256 pinned)
    Resources/AppIcon.icns        when built on macOS (rendered from assets/icon.svg)
    Resources/licenses/pdfium/    PDFium LICENSE + every upstream third-party notice
    Resources/native-messaging/extension-ids.json
    Resources/pdfium-runtime.json build, archive and library digests of the bundled PDFium

Nothing is signed here (see codesign.py); the dylib keeps its upstream bytes because the
loader enforces the pinned checksum. Binaries come from
  cargo build --release --locked -p cm-daemon --bins --features desktop-bootstrap
  cargo build --release --locked -p cm-cli -p cm-mcp -p cm-native-host
and PDFium from `python3 scripts/pdfium/fetch.py DIR` (checksum-verified).

  python3 scripts/package/macos/build_app.py --bin-dir target/release \\
      --pdfium-dir target/pdfium-poc --out target/package
"""
import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/pdfium'))
import fetch as pdfium  # noqa: E402

APP_NAME = 'Citation Manager'
BUNDLE = f'{APP_NAME}.app'
BUNDLE_ID = 'dev.citation-manager.desktop'
MAIN_EXECUTABLE = 'citation-manager'
# Every helper is a sibling of the main executable: clients start `MacOS/papersd` by
# absolute path derived from their own canonical location, never from PATH or cwd.
EXECUTABLES = (MAIN_EXECUTABLE, 'papersd', 'papers', 'papers-mcp', 'papers-native-host')
PDFIUM = 'Frameworks/libpdfium.dylib'
MINIMUM_MACOS = '13.0'  # the pinned PDFium dylib's LC_BUILD_VERSION minos
EXTENSION_IDS = ROOT / 'browser-extension/native-manifests/extension-ids.json'


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def workspace_version():
    return tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']


def info_plist(version, build, icon=False):
    plist = {
        'CFBundleDevelopmentRegion': 'en',
        'CFBundleDisplayName': APP_NAME,
        'CFBundleExecutable': MAIN_EXECUTABLE,
        'CFBundleIdentifier': BUNDLE_ID,
        'CFBundleInfoDictionaryVersion': '6.0',
        'CFBundleName': APP_NAME,
        'CFBundlePackageType': 'APPL',
        'CFBundleShortVersionString': version,
        'CFBundleVersion': build,
        'LSApplicationCategoryType': 'public.app-category.productivity',
        'LSMinimumSystemVersion': MINIMUM_MACOS,
        'NSHighResolutionCapable': True,
        'NSSupportsAutomaticGraphicsSwitching': True,
    }
    if icon:
        plist['CFBundleIconFile'] = 'AppIcon'
    return plist


def make_icns(svg, destination):
    """Render assets/icon.svg into an .icns with macOS tools (qlmanage, sips, iconutil)."""
    if sys.platform != 'darwin' or not all(shutil.which(t) for t in ('qlmanage', 'sips', 'iconutil')):
        return False
    with tempfile.TemporaryDirectory(prefix='cm-icon-') as scratch:
        scratch = Path(scratch)
        subprocess.run(['qlmanage', '-t', '-s', '1024', '-o', str(scratch), str(svg)],
                       check=True, capture_output=True)
        rendered = scratch / f'{svg.name}.png'
        if not rendered.is_file():
            return False
        iconset = scratch / 'AppIcon.iconset'
        iconset.mkdir()
        for size in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                name = f'icon_{size}x{size}' + ('@2x' if scale == 2 else '') + '.png'
                subprocess.run(['sips', '-z', str(size * scale), str(size * scale), str(rendered),
                                '--out', str(iconset / name)], check=True, capture_output=True)
        subprocess.run(['iconutil', '-c', 'icns', str(iconset), '-o', str(destination)], check=True)
    return destination.is_file()


def copy_pdfium(pdfium_dir, contents):
    """Copy the verified dylib and its notices; fail closed on any checksum drift."""
    pin = pdfium.artifact('macos')
    source = pdfium_dir / pin['library']
    if not source.is_file() or sha256(source) != pin['library_sha256']:
        raise ValueError(f'{source}: not the pinned PDFium library (run scripts/pdfium/fetch.py)')
    pdfium.verify_notices(pdfium_dir, pin['notice_overrides_sha256'])
    target = contents / PDFIUM
    target.parent.mkdir(parents=True)
    shutil.copy2(source, target)
    target.chmod(0o755)
    licenses = contents / 'Resources/licenses/pdfium'
    licenses.mkdir(parents=True)
    shutil.copy2(pdfium_dir / 'LICENSE', licenses / 'LICENSE')
    for notice in sorted((pdfium_dir / 'licenses').iterdir()):
        shutil.copy2(notice, licenses / notice.name)
    runtime = {key: pin[key] for key in ('build', 'binding', 'features', 'release', 'url',
                                         'artifact_sha256', 'library_sha256', 'platform')}
    runtime['bundle_path'] = PDFIUM
    (contents / 'Resources/pdfium-runtime.json').write_text(json.dumps(runtime, indent=2) + '\n')
    return target


def build(bin_dir, pdfium_dir, out, version=None, build_number='1', icon=True):
    """Create `<out>/Citation Manager.app` (replacing an earlier build) and return its path."""
    bin_dir, pdfium_dir, out = Path(bin_dir), Path(pdfium_dir), Path(out)
    missing = [name for name in EXECUTABLES if not (bin_dir / name).is_file()]
    if missing:
        raise ValueError(f'missing release binaries in {bin_dir}: {", ".join(missing)}')
    out.mkdir(parents=True, exist_ok=True)
    app = out / BUNDLE
    if app.exists():
        shutil.rmtree(app)
    contents = app / 'Contents'
    (contents / 'MacOS').mkdir(parents=True)
    (contents / 'Resources').mkdir()
    for name in EXECUTABLES:
        shutil.copy2(bin_dir / name, contents / 'MacOS' / name)
        (contents / 'MacOS' / name).chmod(0o755)
    copy_pdfium(pdfium_dir, contents)
    messaging = contents / 'Resources/native-messaging'
    messaging.mkdir()
    shutil.copy2(EXTENSION_IDS, messaging / 'extension-ids.json')
    has_icon = icon and make_icns(ROOT / 'assets/icon.svg', contents / 'Resources/AppIcon.icns')
    plist = info_plist(version or workspace_version(), build_number, has_icon)
    with (contents / 'Info.plist').open('wb') as handle:
        plistlib.dump(plist, handle, sort_keys=True)
    (contents / 'PkgInfo').write_text('APPL????')
    # A bundle is never group/other-writable: the native host refuses to register a
    # writable executable or directory (#23).
    for path in [app, *app.rglob('*')]:
        if not path.is_symlink():
            path.chmod(path.stat().st_mode & ~0o022)
    return app


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--bin-dir', type=Path, required=True, help='cargo release output directory')
    parser.add_argument('--pdfium-dir', type=Path, required=True, help='scripts/pdfium/fetch.py output')
    parser.add_argument('--out', type=Path, required=True, help='directory to place the .app in')
    parser.add_argument('--version', help='CFBundleShortVersionString (default: workspace version)')
    parser.add_argument('--build-number', default='1', help='CFBundleVersion')
    parser.add_argument('--no-icon', action='store_true')
    args = parser.parse_args()
    app = build(args.bin_dir, args.pdfium_dir, args.out, args.version, args.build_number, not args.no_icon)
    print(app)


if __name__ == '__main__':
    main()
