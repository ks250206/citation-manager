#!/usr/bin/env python3
"""Install the Linux desktop build into a private prefix (default ~/.local).

Layout (what `cm_pdf::worker::bundled_library` expects):
  <prefix>/bin/citation-manager
  <prefix>/bin/papersd                            on-demand library daemon (#25), started by the GUI
  <prefix>/bin/papers-native-host                 browser Native Messaging host (#23), sibling of papersd
  <prefix>/lib/citation-manager/libpdfium.so      pinned chromium/8125, checksum-verified
  <prefix>/share/doc/citation-manager/pdfium-licenses/
  <prefix>/share/applications/citation-manager.desktop
  <prefix>/share/icons/hicolor/scalable/apps/citation-manager.svg

The library and UI settings paths are always explicit; nothing connects to a default
library automatically. Without --skip-pdfium the pinned PDFium is downloaded once
(network required) and verified before it is installed.

Browser registration (#23): the installed host writes per-user Chrome/Chromium/Firefox
manifests whose `path` is the absolute installed executable (never the build tree) via
`papers-native-host register`. Allowed extension IDs come from
browser-extension/native-manifests/extension-ids.json (release IDs) or, only with
--development-extensions, the pinned smoke-test extension. Re-running the installer after
moving the prefix re-points the manifests. --pair-native-host also asks the library daemon
for the limited browser-capture grant (this starts papersd and creates the library).
"""
import argparse
import json
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts/pdfium'))
import fetch as pdfium  # noqa: E402


def exec_argument(value):
    """Quote one Exec= argument per the freedesktop Desktop Entry spec."""
    text = str(value).replace('%', '%%')
    if not any(c in text for c in ' \t\n"\'\\><~|&;$*?#()`'):
        return text
    return '"' + ''.join('\\' + c if c in '"`$\\' else c for c in text) + '"'


def desktop_entry(executable, library, settings):
    command = ' '.join(exec_argument(p) for p in [
        executable, '--library', library, '--settings', settings])
    return ('[Desktop Entry]\nType=Application\nName=Citation Manager\n'
            'Comment=Private local citation library\n'
            f'Exec={command}\nIcon=citation-manager\nTerminal=false\n'
            'Categories=Office;Education;\n')


NATIVE_HOST = 'papers-native-host'
EXTENSION_IDS = ROOT / 'browser-extension/native-manifests/extension-ids.json'


def native_host_command(host, library, ids, development=False, pair=False, browsers=()):
    """`papers-native-host register` arguments, or None when no extension ID is available."""
    release = ids.get('release', {})
    chrome, firefox = list(release.get('chrome', [])), list(release.get('firefox', []))
    if not (chrome or firefox or development):
        return None
    command = [str(host), 'register', '--library', str(library)]
    for extension_id in chrome:
        command += ['--chrome-extension-id', extension_id]
    for extension_id in firefox:
        command += ['--firefox-extension-id', extension_id]
    if development:
        command.append('--development-extensions')
    for browser in browsers:
        command += ['--browser', browser]
    if pair:
        command.append('--pair')
    return command


def register_browsers(prefix, library, development=False, pair=False, browsers=(), run=subprocess.run):
    host = prefix / 'bin' / NATIVE_HOST
    if not host.is_file():
        print('papers-native-host was not built; browser capture is not registered')
        return None
    ids = json.loads(EXTENSION_IDS.read_text())
    command = native_host_command(host, library, ids, development, pair, browsers)
    if command is None:
        print('No released browser extension ID yet (#24); browser capture is not registered. '
              'Use --development-extensions for the smoke-test extension.')
        return None
    run(command, check=True)
    return command


def install(prefix, executable, library, settings, pdfium_dir=None):
    for path in (prefix, library, settings):
        if not path.is_absolute():
            raise ValueError('prefix, library and settings paths must be absolute')
    executable = executable.resolve(strict=True)
    bin_dir = prefix / 'bin'
    lib_dir = prefix / 'lib/citation-manager'
    share = prefix / 'share'
    for directory in (bin_dir, lib_dir):
        directory.mkdir(parents=True, exist_ok=True)
    # Private data directories: owner-only, created once, never replaced.
    for directory in (library.parent, settings.parent):
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    target = bin_dir / 'citation-manager'
    shutil.copy2(executable, target)
    target.chmod(0o755)
    # The GUI and the browser host start their sibling papersd by absolute path (#25/#23).
    for name in ('papersd', NATIVE_HOST):
        sibling = executable.with_name(name)
        if sibling.is_file():
            shutil.copy2(sibling, bin_dir / name)
            (bin_dir / name).chmod(0o755)
    if pdfium_dir is not None:
        library_file = pdfium_dir / pdfium.artifact('linux-x64')['library']
        shutil.copy2(library_file, lib_dir / 'libpdfium.so')
        notices = share / 'doc/citation-manager/pdfium-licenses'
        notices.mkdir(parents=True, exist_ok=True)
        shutil.copy2(pdfium_dir / 'LICENSE', notices / 'LICENSE')
        for notice in (pdfium_dir / 'licenses').iterdir():
            shutil.copy2(notice, notices / notice.name)
    apps = share / 'applications'
    icons = share / 'icons/hicolor/scalable/apps'
    apps.mkdir(parents=True, exist_ok=True)
    icons.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / 'assets/icon.svg', icons / 'citation-manager.svg')
    (apps / 'citation-manager.desktop').write_text(desktop_entry(target, library, settings))
    return target


def main():
    home = Path.home()
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--prefix', type=Path, default=home / '.local')
    parser.add_argument('--executable', type=Path, default=ROOT / 'target/release/citation-manager')
    parser.add_argument('--library', type=Path, default=home / '.local/share/citation-manager/library')
    parser.add_argument('--settings', type=Path, default=home / '.config/citation-manager/ui.json')
    parser.add_argument('--skip-pdfium', action='store_true',
                        help='Install without PDFium; PDF metadata reading then reports the library as unavailable.')
    parser.add_argument('--no-browser-registration', action='store_true',
                        help='Do not write Native Messaging manifests for the browser capture host.')
    parser.add_argument('--development-extensions', action='store_true',
                        help='Also allow the pinned development smoke-test extension IDs.')
    parser.add_argument('--pair-native-host', action='store_true',
                        help='Issue the limited browser-capture grant now (starts papersd, creates the library).')
    parser.add_argument('--browser', action='append', default=[],
                        help='chrome, chromium or firefox[=MANIFEST_DIR]; default: browsers with a profile.')
    args = parser.parse_args()
    if sys.platform != 'linux':
        parser.error('this installer is for Linux; macOS uses the .app layout')
    if not args.executable.with_name('papersd').is_file():
        parser.error('papersd must be built next to the executable: '
                     'cargo build -p cm-daemon --bins --features desktop-bootstrap --release --locked')
    if not args.no_browser_registration and not args.executable.with_name(NATIVE_HOST).is_file():
        parser.error('papers-native-host must be built next to the executable: '
                     'cargo build -p cm-native-host --release --locked (or pass --no-browser-registration)')
    with tempfile.TemporaryDirectory(prefix='cm-pdfium-') as scratch:
        pdfium_dir = None
        if not args.skip_pdfium:
            pdfium_dir = Path(scratch)
            pdfium.fetch(pdfium_dir, 'linux-x64')
        target = install(args.prefix.resolve(), args.executable, args.library, args.settings, pdfium_dir)
    print(f'Installed {target}')
    if not args.no_browser_registration:
        register_browsers(args.prefix.resolve(), args.library, args.development_extensions,
                          args.pair_native_host, args.browser)
    print(f'Run: {shlex.quote(str(target))} --library {shlex.quote(str(args.library))} '
          f'--settings {shlex.quote(str(args.settings))}')


if __name__ == '__main__':
    main()
