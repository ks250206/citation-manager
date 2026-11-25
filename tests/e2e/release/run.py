#!/usr/bin/env python3
"""Release E2E (#32): one fresh install driven through the whole MVP flow.

    new install -> save from a browser (GUI and daemon not running) -> GUI start/quit/restart
    -> search -> annotate -> annotated PDF export -> MCP / CLI / REST
    + offline operation, data portability (BibTeX/RIS, backup/restore), privacy defaults

Everything runs in a throw-away HOME / config / library / runtime directory with a clean
environment (no proxy variables, nothing inherited from the caller except PATH/LANG/DISPLAY);
the caller's own library, settings and browser profiles are never touched. Each step records
named checks; the run fails if any check fails. Steps whose prerequisites are missing (no
browser, no X display, not Linux) are reported as `skipped` with the reason, never as passed.

Linux (installs with scripts/install_linux.py into <work>/prefix):

    python3 tests/e2e/release/run.py --bin-dir target/release --pdfium-dir DIR \\
        [--browser chromium[=EXECUTABLE]] [--gui] [--offline] --out DIR

macOS (the built bundle, copied like a drag-install into <work>/Applications):

    python3 tests/e2e/release/run.py --app "/path/Citation Manager.app" \\
        --owner-client target/release/examples/release_e2e_owner --out DIR

--bin-dir holds citation-manager, papersd, papers, papers-mcp and papers-native-host
(`cargo build --release --locked -p cm-daemon --bins --features desktop-bootstrap` and
`cargo build --release --locked -p cm-cli -p cm-mcp -p cm-native-host`), plus
examples/release_e2e_owner (`-p cm-native-host --example release_e2e_owner`), the owner's IPC
session that stands in for the GUI's "Confirm" and "Save PDF…" clicks.

--offline (Linux, root or user namespaces) re-runs this script inside a new network
namespace that only has loopback, so every step after the start proves it needs no network;
the namespace's `Ip:OutNoRoutes` counters show any attempt to reach another host (a deliberate
DOI lookup is the positive control). --browser needs DISPLAY, Node 22, Playwright and
`npm run build` in browser-extension/. --gui needs DISPLAY and xdotool (Linux).
See README.md next to this file.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import secrets
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[3]
FIXTURES = ROOT / 'fixtures'
CORPUS = FIXTURES / 'pdf/corpus.pdf'
CORPUS_TITLE = 'Release E2E corpus'
# Words on page index 0 of corpus.pdf (rotation 0, MediaBox 400x500): "coordinate proof".
HIGHLIGHT_RECT = dict(left=126.0, bottom=380.0, right=237.0, top=395.0)
BROWSER_TITLE = 'Capturing Papers With the Desktop App Closed'
BROWSER_DOI = '10.9999/e2e24.capture'
PROVISIONAL_TITLE = 'Lab Notes on Provisional Capture (corrected)'
HOST_TITLE = 'Host-level Capture Without a Browser'
EXECUTABLES = ('citation-manager', 'papersd', 'papers', 'papers-mcp', 'papers-native-host')
# Heuristic only (absence proves little): well-known telemetry/crash-reporting crates.
TELEMETRY_CRATE = re.compile(r'^(sentry|opentelemetry|posthog|segment|rudderanalytics|mixpanel|amplitude|'
                             r'datadog|bugsnag|rollbar|newrelic|honeycomb|analytics)([-_].*)?$|telemetry')


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as handle:
        for block in iter(lambda: handle.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def loopback_up():
    """Brings `lo` up inside a fresh network namespace (SIOCSIFFLAGS: UP|LOOPBACK|RUNNING)."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        fcntl.ioctl(s, 0x8914, struct.pack('16sH14s', b'lo', 0x1 | 0x8 | 0x40, b''))


def net_counters():
    """Packets the kernel could not route (any attempt to reach a non-loopback host in a
    loopback-only namespace, DNS included). Linux only."""
    counters = {}
    try:
        lines = Path('/proc/net/snmp').read_text().splitlines()
        for header, values in zip(lines[::2], lines[1::2]):
            for name, value in zip(header.split()[1:], values.split()[1:]):
                counters[header.split()[0] + name] = int(value)
    except OSError:
        return None
    try:  # absent when IPv6 is disabled
        for line in Path('/proc/net/snmp6').read_text().splitlines():
            name, value = line.split()
            counters[name] = int(value)
    except OSError:
        pass
    return dict(ipv4_no_route=counters.get('Ip:OutNoRoutes', 0),
                ipv6_no_route=counters.get('Ip6OutNoRoutes', 0),
                tcp_active_opens=counters.get('Tcp:ActiveOpens', 0))


def interfaces():
    try:
        return sorted(line.split(':')[0].strip() for line in Path('/proc/net/dev').read_text().splitlines()[2:])
    except OSError:
        return None


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def processes(executable):
    """PIDs whose executable is `executable` (Linux /proc; macOS pgrep -f)."""
    executable = str(executable)
    if Path('/proc/self/exe').exists():
        found = []
        for entry in os.listdir('/proc'):
            if entry.isdigit():
                try:
                    if os.readlink(f'/proc/{entry}/exe') == executable:
                        found.append(int(entry))
                except OSError:
                    pass
        return found
    out = subprocess.run(['ps', '-axo', 'pid=,command='], capture_output=True, text=True)
    found = []
    for line in out.stdout.splitlines():
        pid, _, command = line.strip().partition(' ')
        if command == executable or command.startswith(executable + ' '):
            found.append(int(pid))
    return found


def tcp_listeners(pid):
    """TCP sockets in LISTEN state held by `pid`."""
    if platform.system() == 'Darwin':
        out = subprocess.run(['lsof', '-nP', '-a', '-p', str(pid), '-iTCP', '-sTCP:LISTEN'],
                             capture_output=True, text=True)
        return [line for line in out.stdout.splitlines()[1:] if line.strip()]
    inodes = set()
    try:
        for fd in os.listdir(f'/proc/{pid}/fd'):
            target = os.readlink(f'/proc/{pid}/fd/{fd}')
            if target.startswith('socket:['):
                inodes.add(target[8:-1])
    except OSError:
        return []
    found = []
    for table in ('/proc/net/tcp', '/proc/net/tcp6'):
        try:
            for line in Path(table).read_text().splitlines()[1:]:
                fields = line.split()
                if fields[3] == '0A' and fields[9] in inodes:
                    found.append(f'{table}:{fields[1]}')
        except OSError:
            pass
    return found


def wait_for(condition, timeout, step=0.25):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = condition()
        if value:
            return value
        time.sleep(step)
    return condition()


class Mcp:
    """Minimal MCP stdio client: newline-delimited JSON-RPC."""

    def __init__(self, argv, env, log):
        self.proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=log, env=env, text=True, encoding='utf-8')
        self.next = 1
        self.raw = []

    def request(self, method, params=None, timeout=60):
        ident = self.next
        self.next += 1
        self.proc.stdin.write(json.dumps(dict(jsonrpc='2.0', id=ident, method=method, params=params or {})) + '\n')
        self.proc.stdin.flush()
        box = {}

        def read():
            while True:
                line = self.proc.stdout.readline()
                if not line:
                    return
                self.raw.append(line)
                message = json.loads(line)
                if message.get('id') == ident:
                    box['message'] = message
                    return

        reader = threading.Thread(target=read, daemon=True)
        reader.start()
        reader.join(timeout)
        if 'message' not in box:
            raise RuntimeError(f'MCP {method}: no response within {timeout}s')
        return box['message']

    def notify(self, method):
        self.proc.stdin.write(json.dumps(dict(jsonrpc='2.0', method=method)) + '\n')
        self.proc.stdin.flush()

    def initialize(self):
        reply = self.request('initialize', dict(protocolVersion='2025-06-18', capabilities={},
                                                clientInfo=dict(name='release-e2e', version='1')))
        self.notify('notifications/initialized')
        return reply

    def call(self, name, arguments):
        reply = self.request('tools/call', dict(name=name, arguments=arguments))
        result = reply.get('result', {})
        return result.get('isError', False), result.get('structuredContent'), reply

    def close(self):
        try:
            self.proc.stdin.close()
            return self.proc.wait(30)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            return None


class Run:
    def __init__(self, args):
        self.args = args
        self.out = args.out.resolve()
        self.out.mkdir(parents=True, exist_ok=True)
        self.logs = self.out / 'logs'
        self.logs.mkdir(exist_ok=True)
        self.work = Path(args.work or tempfile.mkdtemp(prefix='cm-release-e2e-')).resolve()
        self.work.mkdir(parents=True, exist_ok=True)
        # Unix socket paths are short (104/108 bytes): keep the runtime base short.
        self.runtime_base = Path(tempfile.mkdtemp(prefix='cmr-', dir='/tmp')).resolve()
        os.chmod(self.runtime_base, 0o700)
        self.runtime = self.runtime_base / 'citation-manager'
        self.home = self.work / 'home'
        self.config_home = self.home / '.config'
        for directory in (self.home, self.config_home):
            directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.library = self.home / '.local/share/citation-manager/library'
        self.settings = self.config_home / 'citation-manager/ui.json'
        self.chromium_profile = self.work / 'chromium-profile'
        self.checks = {}
        self.results = {}
        self.steps = []
        self.ids = {}
        self.browser_query = BROWSER_TITLE.split()[0]
        self.browser_title = BROWSER_TITLE
        self.env = dict(PATH=os.environ.get('PATH', '/usr/bin:/bin'), LANG='C.UTF-8', HOME=str(self.home),
                        XDG_CONFIG_HOME=str(self.config_home), XDG_DATA_HOME=str(self.home / '.local/share'),
                        XDG_RUNTIME_DIR=str(self.runtime_base), TMPDIR=str(self.work),
                        PAPERS_LIBRARY=str(self.library), PAPERS_DAEMON_IDLE_TIMEOUT_MS='4000')
        for key in ('DISPLAY', 'XAUTHORITY'):
            if os.environ.get(key):
                self.env[key] = os.environ[key]
        self.bin = None
        self.http = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    # ---- recording ------------------------------------------------------------------

    def check(self, name, ok, detail=None):
        self.checks[name] = bool(ok)
        if detail is not None:
            self.results.setdefault('check_details', {})[name] = detail
        print(f'[release-e2e] {"PASS" if ok else "FAIL"} {name}' + (f': {detail}' if detail is not None and not ok else ''),
              flush=True)
        return bool(ok)

    def step(self, name, function, *, skip=None):
        record = dict(name=name)
        if skip:
            record.update(status='skipped', reason=skip)
            self.steps.append(record)
            print(f'[release-e2e] SKIP {name}: {skip}', flush=True)
            return
        before_checks = set(self.checks)
        before_net = net_counters()
        started = time.monotonic()
        try:
            function()
            record['status'] = 'completed'
        except Exception as error:  # recorded, the run continues with the next step
            record['status'] = 'error'
            record['error'] = ''.join(traceback.format_exception(error))[-4000:]
            self.check(f'{name}.completed', False, str(error))
        record['seconds'] = round(time.monotonic() - started, 1)
        after_net = net_counters()
        if before_net and after_net:
            record['network'] = {k: after_net[k] - before_net[k] for k in after_net}
        new = [c for c in self.checks if c not in before_checks]
        record['checks'] = {c: self.checks[c] for c in new}
        if record['status'] == 'completed' and new and not all(self.checks[c] for c in new):
            record['status'] = 'failed'
        self.steps.append(record)

    def no_external_traffic(self, name):
        """In the loopback-only namespace: this step did not try to reach any other host."""
        if not self.args.in_netns:
            return
        record = self.steps[-1] if self.steps and self.steps[-1]['name'] == name else None
        delta = (record or {}).get('network')
        self.check(f'{name}.no_external_network_attempt',
                   delta is not None and delta['ipv4_no_route'] == 0 and delta['ipv6_no_route'] == 0, delta)

    # ---- process helpers ---------------------------------------------------------------

    def run(self, argv, *, name, input=None, timeout=180, env=None, check_exit=None):
        started = time.monotonic()
        proc = subprocess.run([str(a) for a in argv], input=input, capture_output=True, text=True,
                              timeout=timeout, env=env or self.env, encoding='utf-8', errors='replace')
        log = self.logs / f'{name}.log'
        log.write_text(f'$ {" ".join(str(a) for a in argv)}\nexit {proc.returncode} '
                       f'({time.monotonic() - started:.1f}s)\n--- stdout\n{proc.stdout}\n--- stderr\n{proc.stderr}')
        if check_exit is not None and proc.returncode != check_exit:
            raise RuntimeError(f'{name}: exit {proc.returncode} (expected {check_exit}); see {log}')
        return proc

    def papers(self, *argv, name, check_exit=0, timeout=180, input=None):
        proc = self.run([self.bin / 'papers', *argv], name=name, check_exit=check_exit, timeout=timeout, input=input)
        try:
            return proc.returncode, json.loads(proc.stdout) if proc.stdout.strip() else None
        except json.JSONDecodeError:
            return proc.returncode, proc.stdout

    def daemons(self):
        return processes(self.bin / 'papersd')

    def wait_daemon_exit(self, timeout=120):
        return wait_for(lambda: not self.daemons(), timeout, 0.5)

    def rest(self, method, path, *, token=None, body=None, host=None):
        url = f'http://127.0.0.1:{self.port}/api/v1{path}'
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(url, data=data, method=method)
        if token:
            request.add_header('Authorization', f'Bearer {token}')
        if data is not None:
            request.add_header('Content-Type', 'application/json')
        if host:
            request.add_header('Host', host)
        try:
            with self.http.open(request, timeout=30) as response:
                payload = response.read()
                return response.status, json.loads(payload) if payload else None
        except urllib.error.HTTPError as error:
            payload = error.read()
            try:
                return error.code, json.loads(payload)
            except json.JSONDecodeError:
                return error.code, payload.decode(errors='replace')

    def mcp(self, *extra, read_only_wrapper=False):
        log = (self.logs / f'mcp-{len(list(self.logs.glob("mcp-*")))}.stderr.log').open('w')
        if read_only_wrapper:
            argv = [str(self.bin / 'papers'), 'mcp']
        else:
            argv = [str(self.bin / 'papers-mcp'), 'serve', '--library', str(self.library), *extra]
        client = Mcp(argv, self.env, log)
        client.initialize()
        return client

    # ---- steps ---------------------------------------------------------------------------

    def install(self):
        if self.args.app:
            # macOS: a drag-install is a copy of the bundle into an Applications folder.
            target = self.work / 'Applications' / self.args.app.name
            target.parent.mkdir(parents=True, exist_ok=True)
            self.run(['ditto', self.args.app, target], name='install-ditto', check_exit=0)
            self.bin = target / 'Contents/MacOS'
            self.app = target
            pdfium = target / 'Contents/Frameworks/libpdfium.dylib'
            self.check('install.pdfium_bundled', pdfium.is_file())
            self.check('install.licenses_bundled', (target / 'Contents/Resources/licenses').is_dir())
        else:
            prefix = self.work / 'prefix'
            argv = [sys.executable, ROOT / 'scripts/install_linux.py', '--prefix', prefix,
                    '--executable', self.args.bin_dir / 'citation-manager', '--library', self.library,
                    '--settings', self.settings]
            if self.args.pdfium_dir:
                argv += ['--pdfium-dir', self.args.pdfium_dir]
            if self.args.browser:
                argv += ['--development-extensions', '--pair-native-host',
                         '--browser', f'chromium={self.chromium_profile / "NativeMessagingHosts"}']
            else:
                argv += ['--no-browser-registration']
            self.run(argv, name='install', check_exit=0, timeout=600)
            self.bin = prefix / 'bin'
            lock = json.loads((ROOT / 'scripts/pdfium/runtime-lock.json').read_text())['linux_x64']
            pdfium = prefix / 'lib/citation-manager/libpdfium.so'
            self.check('install.pdfium_pinned_checksum', pdfium.is_file() and sha256(pdfium) == lock['library_sha256'])
            self.check('install.pdfium_notices', (prefix / 'share/doc/citation-manager/pdfium-licenses/LICENSE').is_file())
            entry = prefix / 'share/applications/citation-manager.desktop'
            self.check('install.desktop_entry_explicit_library',
                       entry.is_file() and f'--library {self.library}' in entry.read_text())
            if self.args.browser:
                manifest = self.chromium_profile / 'NativeMessagingHosts/dev.citation_manager.native_host.json'
                doc = json.loads(manifest.read_text()) if manifest.is_file() else {}
                self.check('install.browser_manifest_points_at_installed_host',
                           doc.get('path') == str(self.bin / 'papers-native-host'), doc)
        for name in EXECUTABLES:
            path = self.bin / name
            self.check(f'install.{name}_installed', path.is_file() and os.access(path, os.X_OK))
        version = self.run([self.bin / 'papers', '--version'], name='papers-version')
        self.results['papers_version'] = version.stdout.strip()
        self.results['install_root'] = str(self.bin)

    def privacy_defaults(self):
        # A fresh library through the normal lazy start (as the first CLI call does).
        code, status = self.papers('server', 'start', name='privacy-server-start')
        self.results['daemon_status'] = status
        daemons = self.daemons()
        self.check('privacy.daemon_started_lazily_from_install', len(daemons) == 1, daemons)
        mode = lambda p: p.stat().st_mode & 0o777  # noqa: E731
        self.check('privacy.library_owner_only', mode(self.library) == 0o700, oct(mode(self.library)))
        self.check('privacy.database_owner_only', mode(self.library / 'library.sqlite') == 0o600)
        credential = self.library / 'daemon/desktop.credential'
        self.check('privacy.owner_credential_0600', credential.is_file() and mode(credential) == 0o600)
        self.check('privacy.initial_backup_created', (self.library / 'backups/initial.sqlite').is_file())
        api = self.run([self.bin / 'papersd', 'api', 'show', '--library', self.library], name='privacy-api-show')
        self.results['api_show_default'] = api.stdout.strip()
        self.check('privacy.http_api_disabled_by_default', 'enabled=false' in api.stdout, api.stdout.strip())
        listeners = [entry for pid in self.daemons() for entry in tcp_listeners(pid)]
        self.check('privacy.daemon_has_no_tcp_listener', not listeners, listeners)
        # MCP: the default grant is read-only; a write tool is refused by papersd.
        grant = self.run([self.bin / 'papers-mcp', 'grant', '--library', self.library], name='privacy-mcp-grant')
        self.results['mcp_default_grant'] = grant.stdout.strip() + grant.stderr.strip()
        self.check('privacy.mcp_default_grant_issued', grant.returncode == 0)
        client = self.mcp()
        try:
            tools = client.request('tools/list')['result']['tools']
            self.results['mcp_tools'] = sorted(t['name'] for t in tools)
            read_only = {t['name'] for t in tools if (t.get('annotations') or {}).get('readOnlyHint')}
            self.check('privacy.mcp_lists_12_tools_4_read_only', len(tools) == 12 and read_only == {
                'search_papers', 'get_paper', 'get_paper_fulltext', 'get_paper_annotations'}, sorted(read_only))
            error, content, _ = client.call('create_collection', dict(name='should be refused'))
            self.results['mcp_default_write'] = content
            self.check('privacy.mcp_default_refuses_writes', error, content)
        finally:
            client.close()
        lockfile = (ROOT / 'Cargo.lock').read_text()
        names = set(re.findall(r'^name = "([^"]+)"', lockfile, re.M))
        found = sorted(n for n in names if TELEMETRY_CRATE.search(n))
        self.check('privacy.no_telemetry_crates_in_lockfile', not found, found)

    def seed_with_cli(self):
        code, added = self.papers('add', 'file', CORPUS, '--title', CORPUS_TITLE, '--yes', name='cli-add-file')
        result = added['data']['result']
        self.ids['corpus'] = result['paper_id']
        self.ids['attachment'] = result['attachment_id']
        self.check('cli.add_file_saved', added['ok'] and added['data']['saved'])
        stored = sorted(self.library.glob('attachments/**/*.pdf'))
        self.check('cli.attachment_stored_unchanged', any(sha256(p) == sha256(CORPUS) for p in stored))
        code, imported = self.papers('import', FIXTURES / 'bibtex/external-public.bib', '--yes',
                                     '--on-duplicate', 'skip', name='cli-import-bibtex')
        self.check('cli.import_bibtex', imported['ok'] and imported['data']['records'] >= 1, imported['data'].get('plan'))
        code, imported = self.papers('import', FIXTURES / 'ris/recovery.ris', '--yes',
                                     '--on-duplicate', 'skip', name='cli-import-ris')
        self.check('cli.import_ris', imported['ok'], imported['data'].get('plan'))

        def extracted():
            code, text = self.papers('fulltext', self.ids['corpus'], '--page', '1', name='cli-fulltext', check_exit=None)
            return code == 0 and 'coordinate proof' in json.dumps(text, ensure_ascii=False)
        self.check('cli.fulltext_extracted', wait_for(extracted, 120, 1))
        code, listing = self.papers('list', '--limit', '500', name='cli-list-after-seed')
        self.results['papers_after_seed'] = len(listing['data']['papers'])

    def browser_save(self):
        """GUI not running and no papersd: the first save from the browser starts everything."""
        gui = processes(self.bin / 'citation-manager')
        self.check('browser.gui_not_running', not gui, gui)
        self.check('browser.daemon_stopped_before_save', self.wait_daemon_exit(), self.daemons())
        browser, _, executable = self.args.browser.partition('=')
        report = self.out / f'browser-{browser}.json'
        argv = ['node', ROOT / 'browser-extension/tests/e2e/run.mjs', '--browser', browser, '--bin-dir', self.bin,
                '--installed', '--no-inspect', '--library', self.library, '--runtime-dir', self.runtime_base,
                '--home', self.home, '--config-home', self.config_home, '--chromium-profile', self.chromium_profile,
                '--report', report]
        if executable:
            argv += ['--executable', executable]
        # Chromium's singleton socket lives in TMPDIR: keep it short (socket path limit).
        short_tmp = tempfile.mkdtemp(prefix='cmb-', dir='/tmp')
        env = dict(self.env, TMPDIR=short_tmp)
        try:
            proc = self.run(argv, name=f'browser-{browser}', timeout=900, env=env)
        finally:
            shutil.rmtree(short_tmp, ignore_errors=True)
        data = json.loads(report.read_text()) if report.is_file() else {}
        failed = [k for k, v in data.get('checks', {}).items() if not v]
        self.results['browser'] = dict(exit=proc.returncode, checks=len(data.get('checks', {})), failed=failed,
                                       popup_mode=data.get('results', {}).get('popup_mode'))
        self.check('browser.extension_e2e_passed', proc.returncode == 0 and data.get('checks') and not failed, failed)
        self.ids['browser'] = data.get('results', {}).get('article_paper')
        self.ids['provisional'] = data.get('results', {}).get('provisional_paper')
        code, shown = self.papers('show', self.ids['browser'], name='browser-show-article')
        self.check('browser.article_persisted_with_doi', code == 0 and BROWSER_DOI in json.dumps(shown), code)
        code, shown = self.papers('show', self.ids['provisional'], name='browser-show-provisional')
        self.check('browser.provisional_persisted_with_correction', code == 0 and PROVISIONAL_TITLE in json.dumps(shown))

    def host_save(self):
        """No browser available: the same Native Messaging requests the extension sends,
        written to the installed host the way a browser starts it."""
        self.check('host.daemon_stopped_before_save', self.wait_daemon_exit(), self.daemons())
        ids = json.loads((ROOT / 'browser-extension/native-manifests/extension-ids.json').read_text())
        manifest_dir = self.work / 'host-manifests'
        self.run([self.bin / 'papers-native-host', 'register', '--library', self.library, '--development-extensions',
                  '--browser', f'chromium={manifest_dir}', '--pair'], name='host-register', check_exit=0)
        self.check('host.daemon_stopped_after_pairing', self.wait_daemon_exit(), self.daemons())
        origin = f'chrome-extension://{ids["development"]["chrome"][0]}/'
        proc = subprocess.Popen([str(self.bin / 'papers-native-host'), origin], stdin=subprocess.PIPE,
                                stdout=subprocess.PIPE, stderr=(self.logs / 'host-capture.stderr.log').open('w'),
                                env=self.env)

        def exchange(message):
            body = json.dumps(message).encode()
            proc.stdin.write(struct.pack('=I', len(body)) + body)
            proc.stdin.flush()
            length = struct.unpack('=I', proc.stdout.read(4))[0]
            return json.loads(proc.stdout.read(length))
        hello = exchange(dict(v=1, id='h', type='hello'))
        self.check('host.hello_paired', hello.get('ok') and hello['result'].get('paired'), hello)
        page = 'http://127.0.0.1:9/release-e2e/host.html'
        preview = exchange(dict(v=1, id='p', type='capture.preview', capture=dict(
            page_url=page, title=HOST_TITLE, offline=True,
            html=f'<html><head><title>{HOST_TITLE}</title></head><body></body></html>',
            meta=[dict(name='citation_title', content=HOST_TITLE), dict(name='citation_author', content='Hopper, Grace'),
                  dict(name='citation_publication_date', content='2026')], json_ld=[],
            destination=dict(collection=None, tags=[]))))
        self.check('host.preview', preview.get('ok'), preview.get('error'))
        commit = exchange(dict(v=1, id='c', type='capture.commit', preview_id=preview['result']['preview']['id'],
                               decisions=[dict(candidate_index=0, action='Create')]))
        self.check('host.commit', commit.get('ok'), commit.get('error'))
        proc.stdin.close()
        self.check('host.exits_when_port_closes', proc.wait(30) == 0)
        paper = commit['result']['paper'] if commit.get('ok') else None
        self.ids['browser'] = paper.get('id') if isinstance(paper, dict) else paper
        self.browser_query = HOST_TITLE.split()[0]
        self.browser_title = HOST_TITLE
        code, shown = self.papers('show', self.ids['browser'], name='host-show')
        self.check('host.paper_persisted', code == 0 and HOST_TITLE in json.dumps(shown))
        self.run([self.bin / 'papers-native-host', 'unregister'], name='host-unregister')

    def gui_restart(self):
        """Linux/X: start the installed GUI, search in it, quit with Ctrl+Q, start it again."""
        screenshots = self.out / 'screenshots'
        screenshots.mkdir(exist_ok=True)

        def shot(name):
            if shutil.which('import'):
                subprocess.run(['import', '-window', 'root', str(screenshots / f'{name}.png')], env=self.env,
                               check=False, timeout=60)

        def type_search(window, text):
            subprocess.run(['xdotool', 'windowactivate', '--sync', window], env=self.env, check=False, timeout=30)
            subprocess.run(['xdotool', 'key', '--clearmodifiers', 'ctrl+f'], env=self.env, check=False, timeout=30)
            time.sleep(1)
            subprocess.run(['xdotool', 'type', '--delay', '40', text], env=self.env, check=False, timeout=60)
            subprocess.run(['xdotool', 'key', 'Return'], env=self.env, check=False, timeout=30)
            time.sleep(4)

        for round_ in (1, 2):
            # The browser step's retried PDF job may still keep a daemon alive.
            running_before = self.daemons()
            self.results[f'gui_{round_}_daemon_before_start'] = running_before
            log = (self.logs / f'gui-{round_}.log').open('w')
            started = time.monotonic()
            gui = subprocess.Popen([str(self.bin / 'citation-manager'), '--library', str(self.library),
                                    '--settings', str(self.settings)], stdin=subprocess.DEVNULL,
                                   stdout=log, stderr=log, env=self.env)
            try:
                found = subprocess.run(['timeout', '90', 'xdotool', 'search', '--sync', '--onlyvisible', '--pid',
                                        str(gui.pid)], capture_output=True, text=True, env=self.env)
                windows = found.stdout.split()
                self.check(f'gui.{round_}.window_shown', bool(windows), found.stderr.strip())
                self.results[f'gui_{round_}_window_seconds'] = round(time.monotonic() - started, 1)
                if not windows:
                    continue
                window = windows[0]
                time.sleep(6)
                if running_before:
                    self.check(f'gui.{round_}.daemon_running', bool(wait_for(self.daemons, 30)))
                else:
                    self.check(f'gui.{round_}.daemon_started_by_gui', bool(wait_for(self.daemons, 30)))
                code, status = self.papers('--no-start', 'server', 'status', name=f'gui-{round_}-status', check_exit=None)
                self.results[f'gui_{round_}_daemon_status'] = status
                self.check(f'gui.{round_}.gui_connected_to_daemon',
                           code == 0 and status['data']['connections'] >= 2, status)
                shot(f'gui-{round_}-library')
                query = self.browser_query if round_ == 1 else CORPUS_TITLE
                type_search(window, query)
                shot(f'gui-{round_}-search')
                # Machine-checkable result: Esc back to the table, select the first hit and run
                # "Copy citation" from the Command Palette; the clipboard names that paper.
                key = lambda *keys: subprocess.run(['xdotool', 'key', '--clearmodifiers', *keys],  # noqa: E731
                                                   env=self.env, check=False, timeout=30)
                subprocess.run(['xdotool', 'windowactivate', '--sync', window], env=self.env, check=False, timeout=30)
                key('Escape')
                time.sleep(0.5)
                key('Down')
                time.sleep(1)
                key('ctrl+k')
                time.sleep(1)
                subprocess.run(['xdotool', 'type', '--delay', '40', 'Copy citation'], env=self.env, check=False,
                               timeout=60)
                time.sleep(1)
                key('Return')
                time.sleep(1.5)
                shot(f'gui-{round_}-copied')
                clip = subprocess.run(['xclip', '-o', '-selection', 'clipboard'], capture_output=True, text=True,
                                      env=self.env, timeout=10) if shutil.which('xclip') else None
                copied = clip.stdout if clip else ''
                self.results[f'gui_{round_}_clipboard'] = copied
                expected = (self.browser_title if round_ == 1 else CORPUS_TITLE)
                self.check(f'gui.{round_}.search_finds_and_copies_paper', expected in copied, copied or None)
                if round_ == 2:
                    # Visual evidence only: the selected paper's PDF tab (fixed 1400x900 Xvfb layout).
                    subprocess.run(['xdotool', 'mousemove', '1134', '93', 'click', '1'], env=self.env, check=False,
                                   timeout=30)
                    time.sleep(5)
                    shot('gui-2-pdf-tab')
                subprocess.run(['xdotool', 'windowactivate', '--sync', window], env=self.env, check=False, timeout=30)
                key('Escape')
                key('ctrl+q')
                try:
                    gui.wait(30)
                    quit_ok = True
                except subprocess.TimeoutExpired:
                    quit_ok = False
                self.check(f'gui.{round_}.quit_with_ctrl_q', quit_ok and gui.returncode == 0, gui.returncode)
            finally:
                if gui.poll() is None:
                    gui.terminate()
                    try:
                        gui.wait(20)
                    except subprocess.TimeoutExpired:
                        gui.kill()
                log.close()
        self.check('gui.daemon_idles_out_after_quit', self.wait_daemon_exit(), self.daemons())
        # The palette history ("Copy citation") is saved through papersd. Known defect: the GUI's
        # --settings only applies when the GUI itself started papersd; otherwise the daemon uses
        # <library>/ui-settings.json. Record which file was written; require one, owner-only.
        files = [f for f in (self.settings, self.library / 'ui-settings.json') if f.is_file()]
        self.results['gui_settings_files'] = [str(f) for f in files]
        self.check('gui.palette_history_persisted_owner_only',
                   any('citation.copy' in f.read_text() and f.stat().st_mode & 0o077 == 0 for f in files),
                   self.results['gui_settings_files'])

    def search_everywhere(self):
        """The same papers through CLI, REST and MCP (DESIGN §21.2)."""
        self.port = free_port()
        self.run([self.bin / 'papersd', 'api', 'set', '--library', self.library, '--enable', '--port', self.port],
                 name='rest-enable', check_exit=0)
        token = self.run([self.bin / 'papersd', 'token', 'issue', '--library', self.library, '--label', 'release e2e',
                          '--kind', 'api', '--allow-non-tty', '--scope', 'papers:read', '--scope', 'files:read',
                          '--scope', 'annotations:read', '--scope', 'annotations:write', '--scope', 'papers:write'],
                         name='rest-token-issue', check_exit=0)
        self.token = token.stdout.strip().splitlines()[-1]
        read = self.run([self.bin / 'papersd', 'token', 'issue', '--library', self.library, '--label', 'read only',
                         '--kind', 'api', '--allow-non-tty', '--scope', 'papers:read'],
                        name='rest-token-read', check_exit=0)
        self.read_token = read.stdout.strip().splitlines()[-1]
        self.check('rest.listening', wait_for(lambda: self.rest('GET', '/health')[0] == 200, 30))
        listeners = [entry for pid in self.daemons() for entry in tcp_listeners(pid)]
        self.results['rest_listeners'] = listeners
        self.check('rest.loopback_listener_only_when_enabled', bool(listeners))
        status, body = self.rest('GET', '/papers')
        self.check('rest.unauthenticated_refused', status == 401, status)
        status, body = self.rest('GET', '/papers', token=self.token, host='evil.example')
        self.check('rest.foreign_host_header_refused', status in (400, 403, 421), status)

        queries = dict(fulltext='coordinate proof', browser=self.browser_query, japanese='日本語')
        mcp = self.mcp('--read-only')
        try:
            for key, query in queries.items():
                code, cli = self.papers('search', query, name=f'search-cli-{key}')
                cli_ids = {h['paper_id'] for h in cli['data']['hits']}
                status, rest = self.rest('GET', '/search?' + urllib.parse.urlencode(dict(q=query)), token=self.token)
                rest_ids = {h['paper_id'] for h in (rest or {}).get('results', [])} if status == 200 else set()
                error, content, _ = mcp.call('search_papers', dict(query=query))
                mcp_ids = {(item.get('source') or {}).get('paper_id') for item in (content or {}).get('results', [])}
                self.results[f'search_{key}'] = dict(cli=sorted(cli_ids), rest=sorted(rest_ids), mcp=sorted(mcp_ids),
                                                     rest_status=status, mcp_error=error)
                expected = self.ids['corpus'] if key != 'browser' else self.ids.get('browser')
                self.check(f'search.{key}.cli_rest_mcp_agree',
                           expected in cli_ids and cli_ids == rest_ids == mcp_ids, self.results[f'search_{key}'])
            code, cli = self.papers('search', 'coordinate proof', name='search-cli-page')
            page_hits = [h for h in cli['data']['hits'] if h['source'] == 'page']
            self.check('search.fulltext_hit_has_page_jump_target',
                       any(h['page_number'] == 1 and h['uri'].endswith('?page=1') for h in page_hits), page_hits[:1])
            error, content, _ = mcp.call('get_paper_fulltext', dict(paper_id=self.ids['corpus'], from_page_index=0, page_count=1))
            self.check('mcp.fulltext_with_source', not error and 'coordinate proof' in json.dumps(content, ensure_ascii=False)
                       and self.ids['corpus'] in json.dumps(content), content if error else None)
            status, rest = self.rest('GET', f'/papers/{self.ids["corpus"]}/fulltext?from_page=0&max_pages=1', token=self.token)
            self.check('rest.fulltext', status == 200 and 'coordinate proof' in json.dumps(rest, ensure_ascii=False), status)
        finally:
            mcp.close()
        client = self.mcp(read_only_wrapper=True)
        try:
            tools = client.request('tools/list')['result']['tools']
            self.check('cli.papers_mcp_wrapper_serves_tools', len(tools) == 12)
        finally:
            client.close()

    def annotate_and_export(self):
        # REST: a write needs the scope; the read-only token is refused.
        note = dict(kind='note', quote='', comment='REST note from the release E2E',
                    anchor=dict(state='resolved', page_index=0, page_label=None, quads=[]))
        status, body = self.rest('POST', f'/attachments/{self.ids["attachment"]}/annotations', token=self.read_token, body=note)
        self.check('rest.write_without_scope_refused', status == 403, status)
        status, body = self.rest('POST', f'/attachments/{self.ids["attachment"]}/annotations', token=self.token, body=note)
        self.check('rest.annotation_created', status == 201, body)
        status, body = self.rest('POST', '/papers', token=self.token, body=dict(title='Added over REST'))
        self.check('rest.paper_created', status == 201, body)
        rest_paper = (body or {}).get('paper', body or {}).get('id') if isinstance(body, dict) else None
        self.ids['rest_paper'] = rest_paper
        # MCP: read-only grant refuses add_annotation; the owner approves writes; then it works.
        client = self.mcp()
        try:
            arguments = dict(paper_id=self.ids['corpus'], kind='highlight', page_index=0, rects=[HIGHLIGHT_RECT],
                             quote='coordinate proof', comment='MCP highlight from the release E2E', color='yellow')
            error, content, _ = client.call('add_annotation', arguments)
            self.check('mcp.read_only_grant_refuses_annotation', error, content)
            grant = self.run([self.bin / 'papers-mcp', 'grant', '--library', self.library, '--write', '--ttl', '1h'],
                             name='mcp-grant-write')
            self.check('mcp.owner_grants_write', grant.returncode == 0, grant.stderr.strip())
            error, content, _ = client.call('add_annotation', arguments)
            self.check('mcp.annotation_after_write_grant', not error, content)
            error, content, _ = client.call('get_paper_annotations', dict(paper_id=self.ids['corpus']))
            text = json.dumps(content, ensure_ascii=False)
            self.check('mcp.annotations_read_back_with_source',
                       not error and 'MCP highlight' in text and 'REST note' in text, content if error else None)
        finally:
            client.close()
        self.run([self.bin / 'papers-mcp', 'grant', '--library', self.library], name='mcp-grant-back-to-read-only',
                 check_exit=0)
        code, listed = self.papers('annotations', self.ids['corpus'], name='cli-annotations')
        self.check('cli.annotations_listed', code == 0 and len(listed['data']['annotations']) >= 2)
        # The owner (GUI path) confirms the AI annotations and saves the annotated copy.
        owner = [self.args.owner_client, '--library', self.library, '--runtime-dir', self.runtime,
                 '--papersd', self.bin / 'papersd']
        proc = self.run([*owner, 'confirm-ai', '--paper', self.ids['corpus']], name='owner-confirm-ai')
        confirmed = json.loads(proc.stdout or '{}')
        self.check('owner.ai_annotations_confirmed', proc.returncode == 0 and len(confirmed.get('confirmed', [])) >= 2,
                   confirmed)
        exported = self.work / 'Exports' / 'corpus (annotated).pdf'
        exported.parent.mkdir(exist_ok=True)
        proc = self.run([*owner, 'export-pdf', '--attachment', self.ids['attachment'], '--out', exported],
                        name='owner-export-pdf')
        report = json.loads(proc.stdout or '{}')
        self.results['annotated_export'] = report
        details = report.get('report', {})
        self.check('export.annotated_pdf_written_and_verified',
                   proc.returncode == 0 and report.get('complete') and exported.is_file(), report)
        self.check('export.highlight_and_note_in_copy', details.get('highlights', 0) >= 1 and details.get('notes', 0) >= 1)
        self.check('export.original_unchanged', details.get('original_sha256') == sha256(CORPUS)
                   and any(sha256(p) == sha256(CORPUS) for p in self.library.glob('attachments/**/*.pdf')))
        self.check('export.position_error_below_half_point', details.get('max_position_error', 9) < 0.5,
                   details.get('max_position_error'))
        try:
            import pypdf  # optional independent re-read (not PDFium)
            reader = pypdf.PdfReader(str(exported))
            subtypes = [str(a.get_object().get('/Subtype')) for a in (reader.pages[0].get('/Annots') or [])]
            self.results['pypdf_page0_annotations'] = subtypes
            self.check('export.pypdf_sees_highlight_and_note', '/Highlight' in subtypes and '/Text' in subtypes, subtypes)
        except ImportError:
            self.results['pypdf_page0_annotations'] = 'skipped: pypdf not installed'
        # The CLI writes the bibliography; MCP needs the export grant for it.
        code, exports = self.papers('export', '--format', 'bibtex', '-o', self.work / 'Exports/library.bib',
                                    name='cli-export-bibtex')
        self.check('cli.export_bibtex', code == 0 and (self.work / 'Exports/library.bib').is_file())
        code, exports = self.papers('export', '--format', 'ris', '-o', self.work / 'Exports/library.ris',
                                    name='cli-export-ris')
        self.check('cli.export_ris', code == 0 and (self.work / 'Exports/library.ris').is_file())
        # Disable REST again: the listener goes away.
        self.run([self.bin / 'papersd', 'api', 'set', '--library', self.library, '--disable'], name='rest-disable',
                 check_exit=0)
        self.check('rest.listener_closed_after_disable', wait_for(lambda: not any(
            tcp_listeners(pid) for pid in self.daemons()), 30))

    def portability(self):
        """Exports re-import into fresh libraries; a stopped library copied and restored elsewhere
        serves the same papers, PDFs, full text and annotations (INSTALL §6)."""
        code, listing = self.papers('list', '--limit', '500', name='portability-list-source')
        source = {p['id']: p['title'] for p in listing['data']['papers']}
        self.results['library_papers'] = len(source)
        for fmt, path in (('bibtex', 'library.bib'), ('ris', 'library.ris')):
            other = self.work / f'reimport-{fmt}'
            code, imported = self.run_papers_on(other, 'import', self.work / 'Exports' / path, '--yes',
                                                '--on-duplicate', 'skip', name=f'portability-reimport-{fmt}')
            code, listing = self.run_papers_on(other, 'list', '--limit', '500', name=f'portability-list-{fmt}')
            titles = sorted(p['title'] for p in listing['data']['papers'])
            self.results[f'reimport_{fmt}'] = len(titles)
            self.check(f'portability.{fmt}_round_trip_titles', titles == sorted(source.values()),
                       dict(source=len(source), reimported=len(titles)))
        # Backup = a copy of the stopped library; restore = put the copy at a new location.
        self.run([self.bin / 'papers', 'server', 'stop'], name='portability-stop')
        self.check('portability.daemon_stopped_for_backup', self.wait_daemon_exit())
        backup = self.work / 'Backups/library-copy'
        backup.parent.mkdir(exist_ok=True)
        copy = ['ditto', self.library, backup] if platform.system() == 'Darwin' else ['cp', '-a', self.library, backup]
        self.run(copy, name='portability-backup', check_exit=0)
        restored = self.work / 'Restored/library'
        restored.parent.mkdir(exist_ok=True)
        self.run(['ditto', backup, restored] if platform.system() == 'Darwin' else ['cp', '-a', backup, restored],
                 name='portability-restore', check_exit=0)
        code, listing = self.run_papers_on(restored, 'list', '--limit', '500', name='portability-list-restored')
        self.check('portability.restored_same_papers',
                   {p['id']: p['title'] for p in listing['data']['papers']} == source)
        code, text = self.run_papers_on(restored, 'fulltext', self.ids['corpus'], '--page', '1',
                                        name='portability-fulltext-restored')
        self.check('portability.restored_fulltext', 'coordinate proof' in json.dumps(text, ensure_ascii=False))
        code, notes = self.run_papers_on(restored, 'annotations', self.ids['corpus'], name='portability-annotations-restored')
        self.check('portability.restored_annotations', code == 0 and len(notes['data']['annotations']) >= 2)
        code, shown = self.run_papers_on(restored, 'show', self.ids['corpus'], name='portability-show-restored')
        self.check('portability.restored_attachment', sha256(CORPUS) in json.dumps(shown))
        self.run_papers_on(restored, 'server', 'stop', name='portability-stop-restored', check_exit=None)

    def run_papers_on(self, library, *argv, name, check_exit=0):
        proc = self.run([self.bin / 'papers', '--library', library, *argv], name=name, check_exit=check_exit)
        return proc.returncode, json.loads(proc.stdout) if proc.stdout.strip() else None

    def offline_doi(self):
        """Positive control for the namespace counters: a DOI lookup needs the network; it
        fails explicitly and leaves the library unchanged."""
        code, before = self.papers('list', '--limit', '500', name='offline-list-before')
        code, result = self.papers('add', 'doi', '10.1000/release-e2e-offline', '--yes', name='offline-add-doi',
                                   check_exit=None)
        self.results['offline_doi'] = dict(exit=code, error=(result or {}).get('error'))
        self.check('offline.doi_lookup_fails_explicitly', code != 0 and not (result or {}).get('ok'), code)
        code, after = self.papers('list', '--limit', '500', name='offline-list-after')
        self.check('offline.library_unchanged_by_failed_lookup',
                   len(after['data']['papers']) == len(before['data']['papers']))

    def teardown(self):
        self.run([self.bin / 'papers-mcp', 'revoke', '--library', self.library], name='teardown-mcp-revoke')
        if (self.bin / 'papers-native-host').is_file():
            self.run([self.bin / 'papers-native-host', 'unregister'], name='teardown-host-unregister')
        self.run([self.bin / 'papers', 'server', 'stop', '--force'], name='teardown-stop')
        stopped = self.wait_daemon_exit(60)
        for pid in self.daemons():
            os.kill(pid, signal.SIGTERM)
        self.check('teardown.no_daemon_left', stopped)
        self.check('teardown.no_host_left', not processes(self.bin / 'papers-native-host'))

    # ---- driver ------------------------------------------------------------------------------

    def main(self):
        args = self.args
        self.results['platform'] = dict(system=platform.system(), machine=platform.machine(),
                                        release=platform.release(), python=platform.python_version())
        self.results['network_namespace'] = dict(loopback_only=args.in_netns, interfaces=interfaces())
        if args.in_netns:
            try:
                socket.create_connection(('1.1.1.1', 443), timeout=3).close()
                reachable = True
            except OSError:
                reachable = False
            self.check('offline.no_route_to_other_hosts', not reachable and interfaces() == ['lo'], interfaces())
        linux = platform.system() == 'Linux'
        display = bool(os.environ.get('DISPLAY'))
        self.step('install', self.install)
        if self.bin is None:
            return self.finish()
        self.step('privacy_defaults', self.privacy_defaults)
        self.no_external_traffic('privacy_defaults')
        self.step('seed_with_cli', self.seed_with_cli)
        self.no_external_traffic('seed_with_cli')
        if args.browser:
            self.step('browser_save', self.browser_save,
                      skip=None if display and shutil.which('node') else 'needs DISPLAY and node')
        else:
            self.step('host_save', self.host_save)
            self.no_external_traffic('host_save')
        self.step('gui_restart', self.gui_restart,
                  skip=None if args.gui and linux and display and shutil.which('xdotool') else
                  'GUI step needs --gui, Linux, DISPLAY and xdotool (macOS: covered by the package smoke)')
        self.no_external_traffic('gui_restart')
        self.step('search_everywhere', self.search_everywhere)
        self.no_external_traffic('search_everywhere')
        self.step('annotate_and_export', self.annotate_and_export,
                  skip=None if args.owner_client else 'no --owner-client (release_e2e_owner) given')
        self.no_external_traffic('annotate_and_export')
        self.step('portability', self.portability)
        self.no_external_traffic('portability')
        self.step('offline_doi', self.offline_doi,
                  skip=None if args.in_netns else 'only meaningful in the loopback-only namespace (--offline)')
        if args.in_netns and self.steps[-1]['status'] != 'skipped':
            delta = self.steps[-1].get('network') or {}
            self.check('offline.counters_see_the_doi_attempt', delta.get('ipv4_no_route', 0) + delta.get('ipv6_no_route', 0) > 0,
                       delta)
        self.step('teardown', self.teardown)
        return self.finish()

    def sanitize(self, text):
        """Evidence may be committed: machine paths become placeholders, tokens are removed."""
        args = self.args
        roots = [(self.work, '<work>'), (self.runtime_base, '<runtime>'), (self.out, '<out>'), (args.bin_dir, '<bin-dir>'),
                 (args.pdfium_dir and args.pdfium_dir.resolve(), '<pdfium-dir>'),
                 (args.owner_client and args.owner_client.parent, '<owner-client-dir>'),
                 (args.app and args.app.parent, '<app-dir>'), (ROOT, '<repo>'), (Path.home(), '<home>')]
        for path, placeholder in sorted(((p, n) for p, n in roots if p), key=lambda r: -len(str(r[0]))):
            text = text.replace(str(path), placeholder)
        text = re.sub(r'/tmp/cm[a-z]-[A-Za-z0-9_]+', '<tmp>', text)
        for secret in filter(None, (getattr(self, 'token', None), getattr(self, 'read_token', None))):
            text = text.replace(secret, '<token>')
        return text

    def finish(self):
        passed = bool(self.checks) and all(self.checks.values())
        report = dict(schema='release-e2e.v1', passed=passed, checks=self.checks,
                      counts=dict(passed=sum(self.checks.values()), failed=sum(not v for v in self.checks.values()),
                                  skipped_steps=[s['name'] for s in self.steps if s['status'] == 'skipped']),
                      steps=self.steps, ids=self.ids, results=self.results)
        text = json.dumps(report, indent=2, ensure_ascii=False, default=str)
        (self.out / 'report.json').write_text(self.sanitize(text) + '\n')
        for log in [*self.logs.iterdir(), *self.out.glob('browser-*.json')]:
            log.write_text(self.sanitize(log.read_text(errors='replace')))
        print(f'[release-e2e] {report["counts"]["passed"]} passed, {report["counts"]["failed"]} failed, '
              f'skipped steps: {report["counts"]["skipped_steps"]}', flush=True)
        print(f'[release-e2e] {"PASSED" if passed else "FAILED"}; report {self.out / "report.json"}', flush=True)
        if not self.args.keep:
            shutil.rmtree(self.work, ignore_errors=True)
        shutil.rmtree(self.runtime_base, ignore_errors=True)
        return 0 if passed else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument('--bin-dir', type=Path, help='Linux: release build directory (installed with install_linux.py)')
    source.add_argument('--app', type=Path, help='macOS: Citation Manager.app to install by copying')
    parser.add_argument('--owner-client', type=Path,
                        help='release_e2e_owner example (default: <bin-dir>/examples/release_e2e_owner)')
    parser.add_argument('--pdfium-dir', type=Path, help='Linux: install_linux.py --pdfium-dir (cached pinned archive)')
    parser.add_argument('--browser', help='chromium[=EXECUTABLE]: run the extension E2E against the installed host')
    parser.add_argument('--gui', action='store_true', help='Linux/X: start, search in, quit and restart the GUI')
    parser.add_argument('--offline', action='store_true', help='Linux: run in a loopback-only network namespace')
    parser.add_argument('--in-netns', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--work', type=Path, help='work directory (default: a new temporary directory)')
    parser.add_argument('--out', type=Path, required=True, help='report directory (report.json, logs/, screenshots/)')
    parser.add_argument('--keep', action='store_true', help='keep the work directory')
    args = parser.parse_args()
    if args.bin_dir:
        args.bin_dir = args.bin_dir.resolve()
        if args.owner_client is None and (args.bin_dir / 'examples/release_e2e_owner').is_file():
            args.owner_client = args.bin_dir / 'examples/release_e2e_owner'
    if args.owner_client:
        args.owner_client = args.owner_client.resolve()
    if args.app:
        args.app = args.app.resolve()
    if args.offline and not args.in_netns:
        if platform.system() != 'Linux' or not shutil.which('unshare'):
            parser.error('--offline needs Linux and unshare')
        argv = [a for a in sys.argv[1:] if a != '--offline']
        unshare = ['unshare', '-n'] if os.geteuid() == 0 else ['unshare', '-rn']
        return subprocess.call([*unshare, sys.executable, os.path.abspath(__file__), *argv, '--in-netns'])
    if args.in_netns:
        loopback_up()
    return Run(args).main()


if __name__ == '__main__':
    sys.exit(main())
