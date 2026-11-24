#!/usr/bin/env python3
"""Install the Linux desktop build into a private prefix (default ~/.local).

Layout (what `cm_pdf::worker::bundled_library` expects):
  <prefix>/bin/citation-manager
  <prefix>/bin/papersd                            on-demand library daemon (#25), started by the GUI
  <prefix>/lib/citation-manager/libpdfium.so      pinned chromium/8125, checksum-verified
  <prefix>/share/doc/citation-manager/pdfium-licenses/
  <prefix>/share/applications/citation-manager.desktop
  <prefix>/share/icons/hicolor/scalable/apps/citation-manager.svg

The library and UI settings paths are always explicit; nothing connects to a default
library automatically. Without --skip-pdfium the pinned PDFium is downloaded once
(network required) and verified before it is installed.
"""
import argparse
from pathlib import Path
import shlex
import shutil
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
    # The GUI starts its sibling papersd by absolute path (#25); never via PATH.
    daemon = executable.with_name('papersd')
    if daemon.is_file():
        shutil.copy2(daemon, bin_dir / 'papersd')
        (bin_dir / 'papersd').chmod(0o755)
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
    args = parser.parse_args()
    if sys.platform != 'linux':
        parser.error('this installer is for Linux; macOS uses the .app layout')
    if not args.executable.with_name('papersd').is_file():
        parser.error('papersd must be built next to the executable: '
                     'cargo build -p cm-daemon --bins --features desktop-bootstrap --release --locked')
    with tempfile.TemporaryDirectory(prefix='cm-pdfium-') as scratch:
        pdfium_dir = None
        if not args.skip_pdfium:
            pdfium_dir = Path(scratch)
            pdfium.fetch(pdfium_dir, 'linux-x64')
        target = install(args.prefix.resolve(), args.executable, args.library, args.settings, pdfium_dir)
    print(f'Installed {target}')
    print(f'Run: {shlex.quote(str(target))} --library {shlex.quote(str(args.library))} '
          f'--settings {shlex.quote(str(args.settings))}')


if __name__ == '__main__':
    main()
