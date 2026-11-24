#!/usr/bin/env python3
"""Real browser -> Native Messaging -> papers-native-host -> IPC -> papersd smoke (#23).

Uses a throw-away prefix, HOME, library and runtime directory; never the user's library,
browser profile or the development tree (binaries are copied into the temp prefix first).

  1. Install papersd + papers-native-host into <tmp>/prefix/bin, `register --pair` with the
     pinned development extension IDs and manifests in the temp browser profile.
  2. Wait until the papersd started for pairing idles out, so the browser finds it stopped.
  3. Launch the browser with the dev extension (browser-extension/native-manifests/
     dev-extension). It runs hello + capture.preview (one-shot) and destinations +
     capture.commit (port) against an offline HTML fixture, then reports here.
  4. Check every host process exited, quit the browser, and confirm (GUI never started)
     that papersd persisted the paper and drove the PDF job to a terminal state.

Example (Linux, Xvfb on :99):
  cargo build -p cm-daemon --bin papersd && cargo build -p cm-native-host --bins --examples
  python3 scripts/native-host/browser_smoke.py --bin-dir target/debug \\
      --browser chromium --executable /opt/pw-browsers/chromium-1243/chrome-linux/chrome
  python3 scripts/native-host/browser_smoke.py --bin-dir target/debug \\
      --browser firefox --executable /path/to/firefox/firefox
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import zipfile

ROOT = Path(__file__).resolve().parents[2]
EXTENSION = ROOT / 'browser-extension/native-manifests/dev-extension'
IDS = json.loads((ROOT / 'browser-extension/native-manifests/extension-ids.json').read_text())
FIREFOX_ID = IDS['development']['firefox'][0]
PAGE_URL = 'https://journal.example.org/articles/nh23-smoke'


def log(message):
    print(f'[smoke] {message}', flush=True)


def processes_running(executable):
    """PIDs whose /proc/<pid>/exe is `executable` (exact path)."""
    found = []
    for entry in Path('/proc').iterdir():
        if entry.name.isdigit():
            try:
                if os.readlink(entry / 'exe') == str(executable):
                    found.append(int(entry.name))
            except OSError:
                pass
    return found


def wait_for(condition, timeout, step=0.2):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if condition():
            return True
        time.sleep(step)
    return condition()


class Driver(http.server.BaseHTTPRequestHandler):
    reports = []
    event = threading.Event()
    fixture = b''

    def do_GET(self):
        if self.path != '/fixture.html':
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(self.fixture)))
        self.end_headers()
        self.wfile.write(self.fixture)

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
        Driver.reports.append(json.loads(body))
        Driver.event.set()
        self.send_response(204)
        self.end_headers()

    def log_message(self, *_):
        pass


def install(tmp, bin_dir):
    prefix_bin = tmp / 'prefix/bin'
    prefix_bin.mkdir(parents=True)
    for name in ('papersd', 'papers-native-host'):
        shutil.copy2(bin_dir / name, prefix_bin / name)
        (prefix_bin / name).chmod(0o755)
    return prefix_bin


def prepare_extension(tmp, browser, port):
    ext = tmp / 'extension'
    ext.mkdir()
    shutil.copy2(EXTENSION / 'background.js', ext / 'background.js')
    manifest = 'manifest.firefox.json' if browser == 'firefox' else 'manifest.chrome.json'
    shutil.copy2(EXTENSION / manifest, ext / 'manifest.json')
    config = {'port': port, 'page_url': PAGE_URL, 'title': 'Native Messaging Smoke'}
    (ext / 'smoke-config.json').write_text(json.dumps(config))
    return ext


def launch_browser(tmp, browser, executable, ext, env):
    if browser == 'firefox':
        profile = tmp / 'firefox-profile'
        (profile / 'extensions').mkdir(parents=True)
        with zipfile.ZipFile(profile / f'extensions/{FIREFOX_ID}.xpi', 'w') as xpi:
            for item in ext.iterdir():
                xpi.write(item, item.name)
        prefs = {
            'xpinstall.signatures.required': False,
            'extensions.autoDisableScopes': 0,
            'extensions.enabledScopes': 15,
            'browser.shell.checkDefaultBrowser': False,
            'datareporting.policy.dataSubmissionEnabled': False,
            'toolkit.telemetry.reportingpolicy.firstRun': False,
            'browser.startup.homepage_override.mstone': 'ignore',
            'app.update.disabledForTesting': True,
        }
        (profile / 'user.js').write_text(
            ''.join(f'user_pref({json.dumps(k)}, {json.dumps(v)});\n' for k, v in prefs.items()))
        command = [str(executable), '--headless', '--no-remote', '--profile', str(profile), 'about:blank']
    else:
        user_data = tmp / 'chromium-profile'
        command = [str(executable), f'--user-data-dir={user_data}', '--no-first-run',
                   '--no-default-browser-check', '--no-sandbox', '--disable-gpu',
                   f'--disable-extensions-except={ext}', f'--load-extension={ext}', 'about:blank']
    return subprocess.Popen(command, env=env, stdout=subprocess.DEVNULL, stderr=open(tmp / 'browser.log', 'wb'),
                            start_new_session=True)


def quit_browser(process):
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=20)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()
    # Remaining helper processes of the browser's session.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--bin-dir', type=Path, required=True,
                        help='cargo output with papersd, papers-native-host and examples/inspect_capture')
    parser.add_argument('--browser', choices=['chromium', 'firefox'], required=True)
    parser.add_argument('--executable', type=Path, required=True)
    parser.add_argument('--keep', action='store_true', help='keep the temp directory')
    args = parser.parse_args()
    bin_dir = args.bin_dir.resolve()
    tmp = Path(tempfile.mkdtemp(prefix=f'cm-nh-smoke-{args.browser}-'))
    checks = {}
    try:
        prefix_bin = install(tmp, bin_dir)
        host = prefix_bin / 'papers-native-host'
        papersd = prefix_bin / 'papersd'
        library, runtime = tmp / 'library', tmp / 'run'
        runtime.mkdir(mode=0o700)
        env = {k: v for k, v in os.environ.items() if k in ('PATH', 'DISPLAY', 'LANG', 'XAUTHORITY')}
        env.update(HOME=str(tmp / 'home'), XDG_CONFIG_HOME=str(tmp / 'config'),
                   XDG_RUNTIME_DIR=str(runtime), TMPDIR=str(tmp))
        (tmp / 'home').mkdir()
        if args.browser == 'firefox':
            manifest_spec = 'firefox'  # ~/.mozilla/native-messaging-hosts under the temp HOME
        else:
            manifest_spec = f'chromium={tmp / "chromium-profile/NativeMessagingHosts"}'
        register = subprocess.run(
            [str(host), 'register', '--library', str(library), '--runtime-dir', str(runtime),
             '--browser', manifest_spec, '--development-extensions', '--pair'],
            env=env, capture_output=True, text=True, timeout=60)
        log(f'register --pair: exit {register.returncode}\n{register.stdout}{register.stderr}')
        checks['register_and_pair'] = register.returncode == 0
        status = subprocess.run([str(host), 'status'], env=env, capture_output=True, text=True)
        report = json.loads(status.stdout)
        checks['manifest_points_to_installed_host'] = report['healthy'] and report['paired']
        manifest_path = Path(report['manifests'][0]['path'])
        manifest = json.loads(manifest_path.read_text())
        log(f'manifest {manifest_path}: {json.dumps(manifest)}')
        checks['manifest_path_absolute_in_prefix'] = manifest['path'] == str(host)

        log('waiting for the pairing papersd to idle out (default 30 s)')
        waited = time.monotonic()
        checks['papersd_stopped_before_browser'] = wait_for(lambda: not processes_running(papersd), 60)
        log(f'papersd gone after {time.monotonic() - waited:.1f}s: {not processes_running(papersd)}')

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Driver)
        Driver.fixture = (ROOT / 'scripts/native-host/fixture-page.html').read_bytes()
        threading.Thread(target=server.serve_forever, daemon=True).start()
        ext = prepare_extension(tmp, args.browser, server.server_address[1])
        started = time.monotonic()
        browser = launch_browser(tmp, args.browser, args.executable.resolve(), ext, env)
        log(f'launched {args.browser} pid {browser.pid}')
        got = Driver.event.wait(120)
        checks['extension_reported'] = got
        if not got:
            log('no report; browser log tail:\n' + (tmp / 'browser.log').read_text(errors='replace')[-4000:])
            raise SystemExit(1)
        result = Driver.reports[0]
        log(f'report after {time.monotonic() - started:.1f}s: ' + json.dumps(result)[:3000])
        steps = result.get('steps', {})
        checks['extension_flow_ok'] = result.get('ok') is True
        checks['hello_paired'] = steps.get('hello', {}).get('result', {}).get('paired') is True
        commit = steps.get('commit', {})
        checks['commit_ok_via_port'] = commit.get('ok') is True and commit['result'].get('paper') is not None
        checks['unsupported_version_rejected'] = steps.get('unsupported_version', {}).get('error', {}).get('code') == 'unsupported_version'
        checks['not_a_generic_proxy'] = steps.get('not_a_proxy', {}).get('error', {}).get('code') == 'unknown_type'
        checks['merge_denied_by_daemon_grant'] = steps.get('merge_denied', {}).get('error', {}).get('code') == 'denied'
        paper = commit.get('result', {}).get('paper')
        job = (commit.get('result', {}).get('pdf_job') or {}).get('id')
        log(f'paper {paper}, pdf job {job} ({(commit.get("result", {}).get("pdf_job") or {}).get("state")})')

        checks['host_exited_after_requests'] = wait_for(lambda: not processes_running(host), 10)
        quit_browser(browser)
        checks['browser_quit'] = browser.poll() is not None
        browser_log = (tmp / 'browser.log').read_text(errors='replace')
        diagnostics = [line for line in browser_log.splitlines() if line.startswith('papers-native-host:')]
        log('host stderr as captured by the browser:\n' + '\n'.join(diagnostics))
        checks['host_stderr_has_no_page_content'] = not any(
            marker in browser_log for marker in ('journal.example.org', 'Native Messaging Smoke', '10.5555/nh23'))
        daemon_pids = processes_running(papersd)
        log(f'after browser quit: papersd pids {daemon_pids}, host pids {processes_running(host)}')
        checks['papersd_outlived_host_and_browser'] = bool(daemon_pids) or job is None

        inspect = subprocess.run(
            [str(bin_dir / 'examples/inspect_capture'), '--library', str(library), '--runtime-dir', str(runtime),
             '--papersd', str(papersd), '--paper', str(paper)] + (['--job', job] if job else []) +
            ['--wait-secs', '120'], env=env, capture_output=True, text=True, timeout=180)
        log(f'inspect: {inspect.stdout}{inspect.stderr}')
        state = json.loads(inspect.stdout)
        checks['paper_persisted'] = state['paper']['found'] and not state['paper']['deleted']
        checks['pdf_job_terminal'] = job is None or state['job']['terminal']
    finally:
        for pid in processes_running(tmp / 'prefix/bin/papersd'):
            os.kill(pid, signal.SIGTERM)
        if not args.keep:
            shutil.rmtree(tmp, ignore_errors=True)
        else:
            log(f'kept {tmp}')
        log('checks: ' + json.dumps(checks, indent=2))
    sys.exit(0 if checks and all(checks.values()) else 1)


if __name__ == '__main__':
    main()
