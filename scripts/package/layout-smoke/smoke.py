#!/usr/bin/env python3
"""macOS layout smoke for an assembled `Citation Manager.app` (#34; run by package-macos.yml).

The bundle must already sit OUTSIDE the source checkout (the workflow copies it into a
directory with spaces). Everything runs with a throw-away HOME, TMPDIR, library and browser
manifest directory; nothing reads the checkout except the fixture PDF and the pinned lock.

Phases (``--phase``, repeatable; default all):

static  Bundle structure, Info.plist, `lipo -info`, `otool -L` (only system libraries, no
        @rpath/@loader_path references, no link against PDFium), `otool -l` (LC_RPATH,
        LC_ID_DYLIB, minos), PDFium checksum, `codesign -dv`/`--verify`, `spctl --assess`.
cli     The bundled `papers` run through a symlink in a PATH directory: no daemon before,
        lazy start of the bundle's `papersd`, `add file` + `fulltext` (PDFium text through
        the bundled dylib), REST opt-in + `GET /attachments/{id}/pages/0` (PDFium render,
        saved as page.png), then idle exit once nothing is connected.
dmg     For a bundle inside the mounted .dmg (#31): the Applications link, the browser host
        refusing to register from the read-only image, then the cli phase from the image.
gui     Register the browser host, move the bundle to a new path (spaces + non-ASCII), start
        it through LaunchServices without arguments (bundle default library under the temp
        HOME), check the manifest was re-pointed by the app, take a screenshot, quit the
        app and check the daemon idles out.

Writes <out>/summary.json ({check: {ok, detail}}) and <out>/smoke.log. Exit 1 when a
required check failed. Secrets (API token) are never written to the log.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.request
import zlib

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/package/macos'))
import build_app  # noqa: E402

FIXTURE = ROOT / 'fixtures/pdf/corpus.pdf'
FIXTURE_TEXT = json.loads((ROOT / 'fixtures/pdf/manifest.json').read_text())['unicode_line']
SYSTEM_PREFIXES = ('/usr/lib/', '/System/Library/')
IDLE_MS = 8000


# ---------------------------------------------------------------- pure helpers (unit-tested)

def parse_linked_libraries(otool_l_output):
    """Install names from `otool -L FILE` (first line is the file itself)."""
    libraries = []
    for line in otool_l_output.splitlines()[1:]:
        line = line.strip()
        # Universal files repeat the list per architecture under "<file> (architecture x):".
        if line and not line.endswith(':'):
            libraries.append(line.split(' (compatibility version')[0].strip())
    return libraries


def parse_load_commands(otool_output):
    """LC_RPATH paths, LC_ID_DYLIB name and LC_BUILD_VERSION minos values from `otool -l`."""
    result = {'rpaths': [], 'id': None, 'minos': []}
    command = None
    for raw in otool_output.splitlines():
        line = raw.strip()
        if line.startswith('cmd '):
            command = line.split()[1]
            continue
        if command == 'LC_RPATH' and line.startswith('path '):
            result['rpaths'].append(line[5:].split(' (offset')[0])
        elif command == 'LC_ID_DYLIB' and line.startswith('name '):
            result['id'] = line[5:].split(' (offset')[0]
        elif command == 'LC_BUILD_VERSION' and line.startswith('minos '):
            result['minos'].append(line.split()[1])
    return result


def dependency_problems(libraries, own_id=None):
    """Linked libraries an installed bundle cannot rely on (anything outside the OS)."""
    problems = []
    for library in libraries:
        if library == own_id:
            continue
        if library.startswith('@') or 'pdfium' in library.lower():
            problems.append(library)
        elif not library.startswith(SYSTEM_PREFIXES):
            problems.append(library)
    return problems


def png_bytes(width, height, rgba):
    """A PNG (8-bit RGBA, no filter) from raw RGBA rows; stdlib only."""
    if len(rgba) != width * height * 4:
        raise ValueError('RGBA length does not match the dimensions')
    rows = b''.join(b'\x00' + rgba[y * width * 4:(y + 1) * width * 4] for y in range(height))

    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    header = struct.pack('>IIBBBBB', width, height, 8, 6, 0, 0, 0)
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header) + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b'')


def ink_pixels(rgba):
    """Pixels that are visibly not white (the page background)."""
    return sum(1 for i in range(0, len(rgba), 4) if min(rgba[i], rgba[i + 1], rgba[i + 2]) < 200)


def page_text(fulltext_data):
    return '\n'.join(page.get('text') or '' for page in fulltext_data.get('pages', []))


# ---------------------------------------------------------------- evidence

class Evidence:
    def __init__(self, out):
        self.out = out
        out.mkdir(parents=True, exist_ok=True)
        self.checks = {}
        self.secrets = []
        self.log_file = (out / 'smoke.log').open('w', encoding='utf-8')

    def log(self, text):
        for secret in self.secrets:
            text = text.replace(secret, '<redacted>')
        print(text, flush=True)
        self.log_file.write(text + '\n')
        self.log_file.flush()

    def check(self, name, ok, detail=None, required=True):
        self.checks[name] = {'ok': bool(ok), 'required': required, 'detail': detail}
        self.log(f"[{'PASS' if ok else ('FAIL' if required else 'INFO')}] {name}: {json.dumps(detail, ensure_ascii=False)[:2000]}")
        return ok

    def run(self, command, env=None, timeout=120, input=None):
        self.log('$ ' + ' '.join(json.dumps(str(c), ensure_ascii=False) for c in command))
        try:
            result = subprocess.run([str(c) for c in command], env=env, capture_output=True, text=True,
                                    timeout=timeout, input=input)
        except subprocess.TimeoutExpired as error:
            self.log(f'  timed out after {timeout}s')
            return subprocess.CompletedProcess(command, -1, error.stdout or '', error.stderr or '')
        self.log(f'  exit {result.returncode}')
        for name, text in (('stdout', result.stdout), ('stderr', result.stderr)):
            if text.strip():
                self.log(f'  {name}: ' + text.strip()[:6000].replace('\n', '\n    '))
        return result

    def finish(self):
        (self.out / 'summary.json').write_text(json.dumps(self.checks, indent=2, ensure_ascii=False) + '\n')
        failed = [k for k, v in self.checks.items() if v['required'] and not v['ok']]
        self.log(f'{len(self.checks)} checks, {len(failed)} required failed: {failed}')
        return not failed


def wait_for(condition, timeout, step=0.5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = condition()
        if value:
            return value
        time.sleep(step)
    return condition()


def macho_files(app):
    contents = app / 'Contents'
    return [contents / 'MacOS' / name for name in build_app.EXECUTABLES] + [contents / build_app.PDFIUM]


# ---------------------------------------------------------------- phases

def crate_notice_count(contents):
    """Crates listed in the bundled third-party notices summary, or None when absent."""
    summary = contents / 'Resources/licenses/third-party-licenses.json'
    if not summary.is_file():
        return None
    return len(json.loads(summary.read_text())['crates'])


def phase_static(ev, app, expect_signed, require_notices=False):
    contents = app / 'Contents'
    ev.check('app_outside_checkout', ROOT not in app.resolve().parents and ' ' in str(app.resolve()),
             str(app.resolve()))
    plist = plistlib.loads((contents / 'Info.plist').read_bytes())
    ev.check('info_plist', plist.get('CFBundleExecutable') == build_app.MAIN_EXECUTABLE
             and plist.get('CFBundleIdentifier') == build_app.BUNDLE_ID, plist)
    missing = [str(p) for p in macho_files(app) if not p.is_file()]
    ev.check('layout_files_present', not missing, missing or [str(p.relative_to(app)) for p in macho_files(app)])
    notices = sorted(p.name for p in (contents / 'Resources/licenses/pdfium').iterdir())
    ev.check('pdfium_notices_bundled', 'LICENSE' in notices and len(notices) >= 19, notices)
    crates = crate_notice_count(contents)
    ev.check('crate_notices_bundled', bool(crates) and (contents / 'Resources/licenses/THIRD_PARTY_NOTICES.md').is_file(),
             {'crates': crates}, required=require_notices)
    pin = build_app.pdfium.artifact('macos')
    runtime = json.loads((contents / 'Resources/pdfium-runtime.json').read_text())
    digest = hashlib.sha256((contents / build_app.PDFIUM).read_bytes()).hexdigest()
    ev.check('pdfium_pinned_sha256', digest == runtime['bundled_sha256'] and
             (digest == pin['library_sha256'] or runtime['signed_copy']),
             {'bundled': digest, 'upstream': pin['library_sha256'], 'signed_copy': runtime['signed_copy']})
    for path in macho_files(app):
        name = path.name
        lipo = ev.run(['lipo', '-info', path])
        ev.check(f'lipo:{name}', lipo.returncode == 0, lipo.stdout.strip(), required=False)
        linked = parse_linked_libraries(ev.run(['otool', '-L', path]).stdout)
        loads = parse_load_commands(ev.run(['otool', '-l', path]).stdout)
        problems = dependency_problems(linked, loads['id'])
        ev.check(f'otool_dependencies:{name}', not problems,
                 {'linked': linked, 'problems': problems})
        if path.suffix == '.dylib':
            ev.check('pdfium_load_commands', True, loads, required=False)
        else:
            ev.check(f'no_rpath:{name}', not loads['rpaths'], loads)
        sig = ev.run(['codesign', '-dv', '--verbose=4', path])
        ev.check(f'codesign_details:{name}', True,
                 [l for l in sig.stderr.splitlines() if l.split('=')[0] in
                  ('Format', 'CodeDirectory v', 'Signature', 'TeamIdentifier', 'Runtime Version')
                  or l.startswith(('CodeDirectory', 'Signature', 'TeamIdentifier', 'flags'))],
                 required=False)
    verify = ev.run(['codesign', '--verify', '--deep', '--strict', '--verbose=2', app])
    ev.check('codesign_verify_bundle', (verify.returncode == 0) == expect_signed,
             {'expected_signed': expect_signed, 'exit': verify.returncode, 'stderr': verify.stderr.strip()})
    assess = ev.run(['spctl', '--assess', '--type', 'execute', '--verbose=4', app])
    ev.check('gatekeeper_assessment', True,
             {'exit': assess.returncode, 'output': (assess.stdout + assess.stderr).strip(),
              'note': 'ad-hoc/unsigned bundles are expected to be rejected; not a distribution check'},
             required=False)


def isolated_env(work, name):
    home = work / f'{name}-home'
    home.mkdir(parents=True, exist_ok=True)
    # Short private TMPDIR: the daemon socket lives in $TMPDIR/citation-manager-<uid>.
    tmp = Path(tempfile.mkdtemp(prefix=f'cm{name[:3]}', dir='/tmp'))
    env = {k: v for k, v in os.environ.items() if k in ('LANG', 'LC_ALL', 'USER', 'LOGNAME')}
    env.update(HOME=str(home), TMPDIR=str(tmp) + '/', PATH='/usr/bin:/bin:/usr/sbin:/sbin')
    return env


def papers_json(ev, papers, args, env, timeout=120):
    result = ev.run([papers] + args, env=env, timeout=timeout)
    try:
        document = json.loads(result.stdout)
    except ValueError:
        document = {}
    return result.returncode, document


def wait_daemon_gone(ev, pid, timeout):
    """Wait for the daemon process to end without connecting: every IPC connection, even a
    `server status` probe, is activity that restarts the idle timer."""
    began = time.monotonic()
    gone = wait_for(lambda: not daemon_process(pid), timeout, step=1)
    return gone, round(time.monotonic() - began, 1)


def daemon_process(pid):
    out = subprocess.run(['ps', '-o', 'command=', '-p', str(pid)], capture_output=True, text=True).stdout.strip()
    return out or None


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def phase_cli(ev, app, work):
    macos = app / 'Contents/MacOS'
    env = isolated_env(work, 'cli')
    path_dir = work / 'bin on PATH'
    path_dir.mkdir(exist_ok=True)
    link = path_dir / 'papers'
    if link.is_symlink() or link.exists():
        link.unlink()
    link.symlink_to(macos / 'papers')
    env['PATH'] = f'{path_dir}:{env["PATH"]}'
    library = work / 'Library Data/library'
    library.parent.mkdir(parents=True, exist_ok=True)
    library.parent.chmod(0o700)
    env.update(PAPERS_LIBRARY=str(library), PAPERS_DAEMON_IDLE_TIMEOUT_MS=str(IDLE_MS))
    env_path = shutil.which('papers', path=env['PATH'])
    ev.check('papers_resolved_via_path_symlink', env_path == str(link), env_path)
    code, status = papers_json(ev, link, ['server', 'status'], env)
    ev.check('no_daemon_before_first_use', code == 0 and status.get('data', {}).get('running') is False, status.get('data'))
    started = time.monotonic()
    code, added = papers_json(ev, link, ['add', 'file', str(FIXTURE), '--yes'], env)
    result = added.get('data', {}).get('result', {})
    paper, attachment = result.get('paper_id'), result.get('attachment_id')
    ev.check('lazy_start_and_add_file', code == 0 and paper and attachment,
             {'paper': paper, 'attachment': attachment, 'seconds': round(time.monotonic() - started, 2),
              'title': added.get('data', {}).get('title')})
    code, status = papers_json(ev, link, ['server', 'status'], env)
    data = status.get('data', {})
    command = daemon_process(data.get('pid')) if data.get('pid') else None
    ev.check('daemon_is_the_bundled_papersd', data.get('running') is True and command
             and command.startswith(str(macos / 'papersd')), {'status': data, 'process': command})
    if not paper:
        return
    texts = {}

    def extracted():
        code, document = papers_json(ev, link, ['fulltext', paper], env)
        state = document.get('data', {}).get('extraction', {}).get('state')
        texts['last'] = document.get('data', {})
        return state == 'complete'
    done = wait_for(extracted, 90, step=2)
    text = page_text(texts.get('last', {}))
    ev.check('pdfium_text_extraction_in_bundle', done and FIXTURE_TEXT in text,
             {'state': texts.get('last', {}).get('extraction'), 'expected': FIXTURE_TEXT,
              'first_chars': text[:300]})
    # REST opt-in and a PDFium page render through the bundled papersd.
    papersd = macos / 'papersd'
    port = free_port()
    enabled = ev.run([papersd, 'api', 'set', '--library', library, '--enable', '--port', port], env=env)
    issued = ev.run([papersd, 'token', 'issue', '--library', library, '--label', 'layout smoke',
                     '--kind', 'api', '--scope', 'papers:read', '--scope', 'files:read',
                     '--expires-days', '1', '--allow-non-tty'], env=env)
    token = issued.stdout.strip()
    if token:
        ev.secrets.append(token)
    ev.check('rest_opt_in_and_token', enabled.returncode == 0 and issued.returncode == 0 and token,
             {'api_set': enabled.returncode, 'token_issue': issued.returncode,
              'stored_in': [l for l in issued.stderr.splitlines() if 'Stored in' in l]})
    rendered = None
    if token:
        def get(path):
            request = urllib.request.Request(f'http://127.0.0.1:{port}/api/v1{path}',
                                             headers={'Authorization': f'Bearer {token}'})
            with urllib.request.urlopen(request, timeout=30) as response:
                return json.loads(response.read())
        def healthy():
            try:
                return get('/health').get('status') == 'ok'
            except OSError:
                return False
        try:
            ev.check('rest_listener_up', wait_for(healthy, 30), {'port': port})
            rendered = get(f'/attachments/{attachment}/pages/0?zoom=1')
        except Exception as error:  # noqa: BLE001 - recorded as evidence
            ev.log(f'render request failed: {error!r}')
    if rendered:
        rgba = base64.b64decode(rendered['data'])
        (ev.out / 'page.png').write_bytes(png_bytes(rendered['width'], rendered['height'], rgba))
        ink = ink_pixels(rgba)
        ev.check('pdfium_render_in_bundle', ink > 100,
                 {k: rendered.get(k) for k in ('page', 'page_count', 'width', 'height', 'format', 'rotation', 'has_text')}
                 | {'ink_pixels': ink, 'png': 'page.png'})
    else:
        ev.check('pdfium_render_in_bundle', False, 'no rendered page')
    disabled = ev.run([papersd, 'api', 'set', '--library', library, '--disable'], env=env)
    ev.check('rest_disabled_again', disabled.returncode == 0, disabled.returncode)
    pid = data.get('pid')
    gone, seconds = wait_daemon_gone(ev, pid, IDLE_MS / 1000 + 60) if pid else (False, None)
    code, status = papers_json(ev, link, ['--no-start', 'server', 'status'], env)
    ev.check('daemon_idle_exit', gone and status.get('data', {}).get('running') is False,
             {'idle_timeout_ms': IDLE_MS, 'pid': pid, 'seconds_after_last_client': seconds})
    shutil.rmtree(env['TMPDIR'], ignore_errors=True)


def pids_of(path):
    out = subprocess.run(['pgrep', '-f', str(path)], capture_output=True, text=True).stdout
    return [int(p) for p in out.split()]


def phase_gui(ev, app, work, moved_parent):
    env = isolated_env(work, 'gui')
    home = Path(env['HOME'])
    host = app / 'Contents/MacOS/papers-native-host'
    nm_dir = home / 'Library/Application Support/Chromium/NativeMessagingHosts'
    nm_dir.parent.mkdir(parents=True)
    library = home / 'Library/Application Support/Citation Manager/library'
    reg = ev.run([host, 'register', '--library', library, '--browser', f'chromium={nm_dir}',
                  '--development-extensions'], env=env)
    manifest = nm_dir / 'dev.citation_manager.native_host.json'
    before = json.loads(manifest.read_text())['path'] if manifest.is_file() else None
    ev.check('host_registered_with_bundle_path', reg.returncode == 0 and before == str(host.resolve()),
             {'manifest_path': before})
    moved_parent.mkdir(parents=True, exist_ok=True)
    moved = moved_parent / app.name
    shutil.move(str(app), str(moved))
    ev.log(f'moved bundle to {moved}')
    exe = moved / 'Contents/MacOS/citation-manager'
    launched = ev.run(['open', '-n', '--env', f'HOME={env["HOME"]}', '--env', f'TMPDIR={env["TMPDIR"]}', moved], timeout=60)
    how = 'open (LaunchServices, no arguments)'
    process = None
    if launched.returncode != 0:
        how = 'direct exec of Contents/MacOS/citation-manager (no arguments)'
        process = subprocess.Popen([str(exe)], env=env, stdout=(ev.out / 'gui.stdout').open('wb'),
                                   stderr=(ev.out / 'gui.stderr').open('wb'), start_new_session=True)
    gui = wait_for(lambda: pids_of(exe), 30)
    ev.check('gui_process_started_from_moved_bundle', bool(gui), {'how': how, 'pids': gui})
    new_host = moved / 'Contents/MacOS/papers-native-host'
    repointed = wait_for(lambda: manifest.is_file() and json.loads(manifest.read_text())['path'] == str(new_host.resolve()), 30)
    ev.check('app_launch_repoints_browser_manifest', repointed,
             {'before': before, 'after': json.loads(manifest.read_text())['path'] if manifest.is_file() else None})
    status_env = dict(env, PAPERS_LIBRARY=str(library))
    papers = moved / 'Contents/MacOS/papers'
    status = wait_for(lambda: (lambda d: d if d.get('running') else None)(
        papers_json(ev, papers, ['--no-start', 'server', 'status'], status_env)[1].get('data', {})), 45, step=3)
    daemon_pid = (status or {}).get('pid')
    ev.check('gui_started_daemon_for_bundle_default_library', bool(daemon_pid) and library.is_dir(),
             {'library': str(library), 'exists': library.is_dir(),
              'process': daemon_process(daemon_pid) if daemon_pid else None})
    time.sleep(8)
    shot = ev.run(['screencapture', '-x', ev.out / 'gui.png'], timeout=60)
    ev.check('gui_screenshot', shot.returncode == 0 and (ev.out / 'gui.png').is_file(), 'gui.png', required=False)
    windows = ev.run(['osascript', '-e',
                      'tell application "System Events" to get name of every window of (every process whose unix id is '
                      + (str(gui[0]) if gui else '0') + ')'], timeout=30)
    ev.check('gui_window_listed', True, (windows.stdout + windows.stderr).strip(), required=False)
    for pid in pids_of(exe):
        os.kill(pid, signal.SIGTERM)
    if process:
        process.wait(timeout=30)
    ev.check('gui_quit', wait_for(lambda: not pids_of(exe), 30), None)
    gone, seconds = wait_daemon_gone(ev, daemon_pid, 120) if daemon_pid else (False, None)
    ev.check('daemon_idle_exit_after_gui_quit', gone,
             {'pid': daemon_pid, 'seconds_after_gui_quit': seconds, 'default_idle_timeout_s': 30})
    ev.run([new_host, 'unregister', '--keep-grant'], env=env)
    shutil.rmtree(env['TMPDIR'], ignore_errors=True)
    return moved


def phase_dmg(ev, app, work):
    link = app.parent / 'Applications'
    ev.check('dmg_applications_link', link.is_symlink() and os.readlink(link) == '/Applications', str(link))
    env = isolated_env(work, 'dmg')
    host = app / 'Contents/MacOS/papers-native-host'
    refused = ev.run([host, 'register', '--library', work / 'dmg library', '--browser',
                      f'chromium={Path(env["HOME"]) / "nm"}', '--development-extensions'], env=env)
    ev.check('host_refuses_read_only_image', refused.returncode != 0 and 'read-only volume' in refused.stderr,
             {'exit': refused.returncode, 'stderr': refused.stderr.strip()[-400:]})
    shutil.rmtree(env['TMPDIR'], ignore_errors=True)
    phase_cli(ev, app, work)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--app', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True, help='evidence directory')
    parser.add_argument('--work', type=Path, required=True, help='scratch directory (HOME, libraries)')
    parser.add_argument('--phase', action='append', choices=['static', 'cli', 'gui', 'dmg'])
    parser.add_argument('--require-notices', action='store_true', help='the crate notices must be bundled')
    parser.add_argument('--expect-signed', action='store_true', help='codesign --verify must pass')
    parser.add_argument('--moved-parent', type=Path, help='where the gui phase moves the bundle')
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('macOS only')
    phases = args.phase or ['static', 'cli', 'gui']
    ev = Evidence(args.out)
    app = args.app.resolve()
    args.work.mkdir(parents=True, exist_ok=True)
    ev.log(f'app {app}; phases {phases}; macOS {subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip()}')
    try:
        if 'static' in phases:
            phase_static(ev, app, args.expect_signed, args.require_notices)
        if 'cli' in phases:
            phase_cli(ev, app, args.work)
        if 'dmg' in phases:
            phase_dmg(ev, app, args.work)
        if 'gui' in phases:
            moved = phase_gui(ev, app, args.work, args.moved_parent or args.work / 'Moved Apps 移動先')
            (args.out / 'moved-app-path.txt').write_text(str(moved) + '\n')
    except Exception as error:  # noqa: BLE001
        ev.check('smoke_completed', False, repr(error))
        raise
    finally:
        ok = ev.finish()
    sys.exit(0 if ok else 1)


if __name__ == '__main__':
    main()
