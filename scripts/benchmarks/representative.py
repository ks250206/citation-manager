#!/usr/bin/env python3
"""#30 representative load: N papers / M PDFs / M*P pages through the real release papersd.

Opt-in only (never part of `cargo test` or the default CI): builds release binaries,
generates a deterministic synthetic PDF corpus (scripts/benchmarks/pdfgen.py), seeds a
private new library, lets papersd index it with the pinned PDFium, then measures
warm start, collection/tag switches, metadata and full-text searches over real IPC,
Japanese short-term recall against a literal oracle, and process RSS/CPU/I/O. Results are
written under target/representative-benchmark/<name>; nothing outside it is touched.

    python3 scripts/benchmarks/representative.py --out linux-full            # 10k/1k/20k
    python3 scripts/benchmarks/representative.py --small --out smoke         # tiny smoke
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import resource
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
from benchmarks import pdfgen  # noqa: E402
from benchmarks.run import quantile  # noqa: E402

# DESIGN §16.2 initial targets (ms or MiB). GUI-only rows are reported as unmeasured
# unless a GUI measurement is supplied; nothing unmeasured is ever marked as met.
TARGETS = {
    'warm_start_ms': ('温かい起動 1秒程度', 1000.0),
    'collection_switch_p95_ms': ('コレクション切り替え p95 < 100 ms', 100.0),
    'metadata_search_p95_ms': ('メタデータ検索 p95 < 150 ms', 150.0),
    'fulltext_search_p95_ms': ('本文全文検索 p95 < 300 ms', 300.0),
    'gui_idle_rss_mib': ('GUI アイドル RSS < 200 MB', 200.0),
    'pdf_view_rss_mib': ('PDF 1本閲覧時 RSS < 350 MB', 350.0),
}


def run(args, **kwargs):
    return subprocess.run(args, cwd=ROOT, check=True, **kwargs)


def capture(args):
    try:
        return subprocess.check_output(args, cwd=ROOT, text=True, stderr=subprocess.DEVNULL).strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def environment():
    env = dict(started_utc=datetime.now(timezone.utc).isoformat(), system=platform.system(),
               machine=platform.machine(), platform=platform.platform(), python=platform.python_version(),
               rust=capture(['rustc', '-Vv']), cargo=capture(['cargo', '-V']),
               git_head=capture(['git', 'rev-parse', 'HEAD']),
               git_status=capture(['git', 'status', '--porcelain']),
               cargo_lock_sha256=hashlib.sha256((ROOT / 'Cargo.lock').read_bytes()).hexdigest(),
               build_environment={k: os.environ.get(k) for k in (
                   'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET', 'CARGO_TARGET_DIR')},
               ci=os.environ.get('GITHUB_ACTIONS') == 'true',
               runner=dict(name=os.environ.get('RUNNER_NAME'), os=os.environ.get('RUNNER_OS'),
                           arch=os.environ.get('RUNNER_ARCH'), image=os.environ.get('ImageOS'),
                           image_version=os.environ.get('ImageVersion')))
    if platform.system() == 'Darwin':
        env.update(model=capture(['sysctl', '-n', 'hw.model']),
                   cpu=capture(['sysctl', '-n', 'machdep.cpu.brand_string']),
                   physical_cpu_count=capture(['sysctl', '-n', 'hw.physicalcpu']),
                   logical_cpu_count=capture(['sysctl', '-n', 'hw.logicalcpu']),
                   ram_bytes=int(capture(['sysctl', '-n', 'hw.memsize']) or 0),
                   os=capture(['sw_vers']), virtualized=capture(['sysctl', '-n', 'kern.hv_vmm_present']))
    else:
        cpuinfo = Path('/proc/cpuinfo').read_text(errors='replace') if Path('/proc/cpuinfo').exists() else ''
        models = sorted({l.split(':', 1)[1].strip() for l in cpuinfo.splitlines() if l.startswith('model name')})
        meminfo = Path('/proc/meminfo').read_text() if Path('/proc/meminfo').exists() else ''
        total = next((int(l.split()[1]) * 1024 for l in meminfo.splitlines() if l.startswith('MemTotal:')), 0)
        env.update(cpu=models, logical_cpu_count=os.cpu_count(), ram_bytes=total,
                   os=capture(['sh', '-c', '. /etc/os-release && echo "$PRETTY_NAME"']),
                   kernel=platform.release(), virtualization=capture(['systemd-detect-virt']))
    return env


def build(args, name):
    """Release build; returns the executable Cargo reports for target `name`."""
    result = run(['cargo', 'build', '--release', '--locked', *args, '--message-format=json'],
                 stdout=subprocess.PIPE, text=True)
    found = [v for line in result.stdout.splitlines()
             if (v := json.loads(line)).get('reason') == 'compiler-artifact'
             and v['target']['name'] == name and v.get('executable')]
    if len(found) != 1:
        raise ValueError(f'expected one release artifact for {name}')
    artifact = found[0]
    if artifact['profile']['debug_assertions'] or artifact['profile']['opt_level'] != '3':
        raise ValueError('requires the optimized release profile')
    return Path(artifact['executable']), dict(
        cargo_args=args, features=artifact['features'], profile=artifact['profile'],
        sha256=hashlib.sha256(Path(artifact['executable']).read_bytes()).hexdigest())


def stage(papersd, pdfium, out, gui=None):
    """Installed layout so papersd finds the bundled PDFium exactly as in a release."""
    if platform.system() == 'Darwin':
        bin_dir = out / 'Bench.app/Contents/MacOS'
        lib = out / 'Bench.app/Contents/Frameworks/libpdfium.dylib'
    else:
        bin_dir = out / 'prefix/bin'
        lib = out / 'prefix/lib/citation-manager/libpdfium.so'
    bin_dir.mkdir(parents=True)
    lib.parent.mkdir(parents=True)
    shutil.copy2(papersd, bin_dir / 'papersd')
    shutil.copy2(pdfium, lib)
    if gui:
        shutil.copy2(gui, bin_dir / 'citation-manager')
    return bin_dir


class Sampler:
    """Samples RSS (KiB) and cumulative CPU of one pid with `ps` every `interval` seconds;
    on Linux also /proc/<pid>/io. Records (t, rss_kib, cpu_s, read_bytes, write_bytes)."""

    def __init__(self, pids, interval=0.25):
        self.pids = pids
        self.interval = interval
        self.samples = []
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self._loop, daemon=True)

    @staticmethod
    def cpu_seconds(text):
        days, _, rest = text.rpartition('-')
        parts = [float(p) for p in rest.split(':')]
        seconds = sum(v * 60 ** i for i, v in enumerate(reversed(parts)))
        return seconds + (int(days) * 86400 if days else 0)

    def _loop(self):
        start = time.monotonic()
        while not self.stop.is_set():
            row = dict(t=round(time.monotonic() - start, 3))
            for name, pid in self.pids().items():
                out = capture(['ps', '-o', 'rss=,time=', '-p', str(pid)])
                if not out:
                    continue
                rss, cpu = out.split()
                entry = dict(rss_kib=int(rss), cpu_s=self.cpu_seconds(cpu))
                io = Path(f'/proc/{pid}/io')
                try:
                    fields = dict(l.split(': ') for l in io.read_text().splitlines())
                    entry.update(read_bytes=int(fields['read_bytes']), write_bytes=int(fields['write_bytes']),
                                 rchar=int(fields['rchar']), wchar=int(fields['wchar']))
                except (OSError, KeyError, ValueError):
                    pass
                row[name] = entry
            self.samples.append(row)
            self.stop.wait(self.interval)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *exc):
        self.stop.set()
        self.thread.join()

    def peak(self, name):
        values = [s[name]['rss_kib'] for s in self.samples if name in s]
        return max(values) if values else None

    def last(self, name):
        rows = [s[name] for s in self.samples if name in s and s[name]['rss_kib'] > 0]
        return rows[-1] if rows else None


def rusage_dict(usage):
    # ru_maxrss: KiB on Linux, bytes on macOS. Block counts: 512-byte units on Linux
    # (filesystem block operations on macOS); both are process-lifetime totals.
    maxrss_kib = usage.ru_maxrss // 1024 if platform.system() == 'Darwin' else usage.ru_maxrss
    return dict(user_s=usage.ru_utime, system_s=usage.ru_stime, max_rss_kib=maxrss_kib,
                in_blocks=usage.ru_inblock, out_blocks=usage.ru_oublock,
                voluntary_ctx=usage.ru_nvcsw, involuntary_ctx=usage.ru_nivcsw)


def spawn_papersd(bin_dir, library, runtime, idle_ms, log):
    return subprocess.Popen([str(bin_dir / 'papersd'), '--library', str(library), '--runtime-dir',
                             str(runtime), '--idle-timeout-ms', str(idle_ms)],
                            stdin=subprocess.DEVNULL, stdout=log, stderr=log)


def wait_rusage(process, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        pid, status, usage = os.wait4(process.pid, os.WNOHANG)
        if pid:
            process.returncode = os.waitstatus_to_exitcode(status)
            return process.returncode, rusage_dict(usage)
        time.sleep(0.05)
    process.kill()
    raise ValueError('papersd did not exit in time')


def summarize(queries, warm, index, stats, oracle, gui):
    groups = {}
    for row in queries['rows']:
        groups.setdefault(row['case'], []).append(row)
    cases = {}
    for case, rows in sorted(groups.items()):
        values = [r['elapsed_ms'] for r in rows]
        cases[case] = dict(samples=len(values), p50_ms=quantile(values, .5), p95_ms=quantile(values, .95),
                           max_ms=max(values), returned=sorted({r['returned'] for r in rows}),
                           detail=rows[-1]['detail'])
    fulltext = [v for k, v in cases.items() if k.startswith('fulltext_default:')]
    # A complete answer is required for the full-text target; bounded short-term answers
    # that leave PDF pages unsearched are reported separately (see recall).
    fulltext_complete = [v for k, v in cases.items()
                         if k.startswith('fulltext_default:') and v['detail']['complete']]
    starts = [r['first_page_ms'] for r in warm['runs'][1:]] or [warm['runs'][0]['first_page_ms']]
    warm_rss = [r['rss_kib_after_first_page'] for r in warm['runs'] if r['rss_kib_after_first_page']]

    def all_fulltext_rows(prefix, complete_only):
        values = []
        for row in queries['rows']:
            if row['case'].startswith(prefix) and (not complete_only or row['detail']['complete']):
                values.append(row['elapsed_ms'])
        return values

    def p95(values):
        return quantile(values, .95) if values else None

    metadata_rows = [r['elapsed_ms'] for r in queries['rows']
                     if r['case'] in ('metadata_keyword', 'metadata_no_hit', 'metadata_combined')]
    window_calls = [ms for r in queries['recall'] if r['scope'] == 'AllPages' and r['scalars'] < 3
                    for ms in r['final_coverage'].get('call_ms', [])]

    measured = {
        'warm_start_ms': quantile(starts, .5),
        'warm_start_p95_ms': quantile(starts, .95),
        'collection_switch_p95_ms': cases['collection_switch']['p95_ms'],
        'tag_and_switch_p95_ms': cases['tag_and_switch']['p95_ms'],
        # Worst honest reading: every literal metadata case (hit, no-hit scan, combined).
        'metadata_search_p95_ms': p95(metadata_rows),
        'metadata_keyword_p95_ms': cases['metadata_keyword']['p95_ms'],
        'metadata_fts_p95_ms': cases['fts_bibliographic:needle']['p95_ms'],
        'fulltext_search_p95_ms': p95(all_fulltext_rows('fulltext_default:', True)),
        'fulltext_search_p95_ms_all_terms_default_scope': p95(all_fulltext_rows('fulltext_default:', False)),
        'fulltext_in_collection_p95_ms': p95(all_fulltext_rows('fulltext_in_collection:', False)),
        # Explicit complete scan of 1-2 character terms: every window/result-page call.
        'short_term_all_pages_call_p95_ms': p95(window_calls),
        'daemon_idle_rss_mib_after_warm_start': max(warm_rss) / 1024 if warm_rss else None,
        # Sampled `ps` RSS, not rusage: Linux keeps the pre-exec maximum (the forked Python
        # runner) in ru_maxrss, so rusage overstates papersd there.
        'daemon_peak_rss_mib_indexing': index['peak_rss_kib'] / 1024 if index.get('peak_rss_kib') else None,
        'daemon_peak_rss_mib_queries': (queries['peak_rss_kib_sampled'] / 1024
                                        if queries.get('peak_rss_kib_sampled') else None),
        'gui_idle_rss_mib': gui.get('gui_idle_rss_mib') if gui else None,
        'total_idle_rss_mib_gui_plus_daemon': gui.get('total_idle_rss_mib') if gui else None,
        'pdf_view_rss_mib': gui.get('pdf_view_rss_mib') if gui else None,
    }
    comparison = []
    for key, (label, limit) in TARGETS.items():
        value = measured.get(key)
        comparison.append(dict(metric=key, target=label, limit=limit, measured=value,
                               status='unmeasured' if value is None else ('met' if value < limit else 'missed')))
    recall = [dict(term=r['term'], scalars=r['scalars'], scope=r['scope'], expected=r['expected_pages'],
                   found=r['found_pages'], recall=r['recall'], false_positives=r['false_positive_pages'],
                   windows=r['windows'], calls=len(r['final_coverage'].get('call_ms', [])),
                   call_p95_ms=p95(r['final_coverage'].get('call_ms', [])),
                   total_ms=r['total_ms'], complete=r['final_coverage']['complete'],
                   unsearched=r['final_coverage']['unsearched']) for r in queries['recall']]
    return dict(quantile='nearest rank sorted[ceil(p*N)-1]', measured=measured, targets=comparison,
                cases=cases, recall=recall, fulltext_complete_cases=len(fulltext_complete),
                fulltext_cases=len(fulltext))


def gui_measure(bin_dir, library, out, settle_s, link, label='gui'):
    """Linux/Xvfb only: the real GUI on the indexed library. Phase 1 idle (library list
    loaded, no PDF): GUI + lazily started papersd RSS. Phase 2 (xdotool/xclip available):
    open the first corpus PDF through the GUI's own `paper://` link command (Ctrl+K palette,
    "Open paper:// Link from Clipboard"), expand the viewer and scroll 60 wheel steps;
    RSS after each.
    Screenshots are kept as evidence that each phase reached the intended screen."""
    display = os.environ.get('DISPLAY')
    if platform.system() != 'Linux' or not display:
        return dict(skipped='GUI measurement needs Linux with an X display (Xvfb)')
    runtime = Path(tempfile.mkdtemp(prefix='cmg-', dir='/tmp'))
    env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime))
    log = (out / f'{label}.log').open('w')
    start = time.monotonic()
    gui = subprocess.Popen([str(bin_dir / 'citation-manager'), '--library', str(library),
                            '--settings', str(out / f'{label}-settings.json')],
                           stdin=subprocess.DEVNULL, stdout=log, stderr=log, env=env)
    daemon_pid = {}

    def pids():
        found = dict(gui=gui.pid)
        if 'papersd' not in daemon_pid:
            listing = capture(['pgrep', '-f', f'papersd --library {library}'])
            if listing:
                daemon_pid['papersd'] = int(listing.split()[0])
        found.update(daemon_pid)
        return found

    def rss(sampler, name, last=6):
        values = [s[name]['rss_kib'] for s in sampler.samples[-last:] if name in s]
        return max(values) / 1024 if values else None

    def shot(name):
        if shutil.which('import'):
            subprocess.run(['import', '-window', 'root', str(out / f'{label}-{name}.png')], check=False)

    phases = {}
    tools = all(shutil.which(t) for t in ('xdotool', 'xclip'))
    with Sampler(pids, 0.5) as sampler:
        time.sleep(settle_s)
        phases['idle'] = dict(t=time.monotonic() - start, gui_rss_mib=rss(sampler, 'gui'),
                              daemon_rss_mib=rss(sampler, 'papersd'))
        shot('idle')
        if tools and link:
            subprocess.run(['xclip', '-selection', 'clipboard'], input=link.encode(), check=True)
            windows = capture(['xdotool', 'search', '--sync', '--onlyvisible', '--pid', str(gui.pid)])
            if windows:
                window = windows.split()[0]
                subprocess.run(['xdotool', 'windowactivate', '--sync', window], check=False)
                subprocess.run(['xdotool', 'key', '--window', window, 'ctrl+k'], check=False)
                time.sleep(1)
                subprocess.run(['xdotool', 'type', '--delay', '30', 'Open paper:// Link'], check=False)
                time.sleep(1)
                subprocess.run(['xdotool', 'key', 'Return'], check=False)
                time.sleep(settle_s)
                phases['pdf_open'] = dict(t=time.monotonic() - start, gui_rss_mib=rss(sampler, 'gui'),
                                          daemon_rss_mib=rss(sampler, 'papersd'))
                shot('pdf-open')
                # "Expand" (top right of the details pane on the fixed 1400x900 Xvfb
                # screen) widens the viewer; then scroll inside it.
                subprocess.run(['xdotool', 'mousemove', '1309', '93', 'click', '1'], check=False)
                time.sleep(3)
                shot('pdf-expanded')
                subprocess.run(['xdotool', 'mousemove', '700', '550'], check=False)
                subprocess.run(['xdotool', 'click', '--repeat', '60', '--delay', '120', '5'], check=False)
                time.sleep(settle_s)
                phases['pdf_scrolled'] = dict(t=time.monotonic() - start, gui_rss_mib=rss(sampler, 'gui'),
                                              daemon_rss_mib=rss(sampler, 'papersd'),
                                              peak_gui_rss_mib=sampler.peak('gui') / 1024)
                shot('pdf-scrolled')
                # Page through the whole document with the toolbar's next-page button so
                # every page is rendered once and the render cache reaches its bound.
                for _ in range(24):
                    subprocess.run(['xdotool', 'mousemove', '201', '317', 'click', '1'], check=False)
                    time.sleep(0.7)
                time.sleep(settle_s)
                phases['pdf_paged'] = dict(t=time.monotonic() - start, gui_rss_mib=rss(sampler, 'gui'),
                                           daemon_rss_mib=rss(sampler, 'papersd'),
                                           peak_gui_rss_mib=sampler.peak('gui') / 1024)
                shot('pdf-paged')
    gui.terminate()
    try:
        gui.wait(20)
    except subprocess.TimeoutExpired:
        gui.kill()
    log.close()
    idle = phases['idle']
    result = dict(settle_s=settle_s, samples=sampler.samples, gui_pid=gui.pid, phases=phases,
                  wall_s=time.monotonic() - start, link=link, interaction_tools=tools,
                  gui_idle_rss_mib=idle['gui_rss_mib'], daemon_rss_mib_with_gui=idle['daemon_rss_mib'],
                  method='ps RSS of the GUI process and of the papersd it started; Xvfb software rendering')
    if idle['gui_rss_mib'] and idle['daemon_rss_mib']:
        result['total_idle_rss_mib'] = idle['gui_rss_mib'] + idle['daemon_rss_mib']
    if 'pdf_open' in phases and phases['pdf_open']['gui_rss_mib']:
        # Total process RSS while one PDF is open (GUI + papersd, both run PDFium).
        result['pdf_view_rss_mib'] = max(
            (phases[k]['gui_rss_mib'] or 0) + (phases[k]['daemon_rss_mib'] or 0)
            for k in ('pdf_open', 'pdf_scrolled', 'pdf_paged') if k in phases)
        result['pdf_view_note'] = (f'verify {label}-pdf-*.png show the PDF and its last page; '
                                   'otherwise this value is not a PDF-view measurement')
    shutil.rmtree(runtime, ignore_errors=True)
    return result


def resummarize(directory):
    """Recomputes the derived summary fields from the raw JSON kept in `directory`."""
    load = lambda name: json.loads((directory / f'{name}.json').read_text())
    summary = load('summary')
    gui = load('gui') if (directory / 'gui.json').exists() else None
    summary.update(summarize(load('queries'), load('warm'), load('index'), load('stats'), load('oracle'), gui))
    (directory / 'summary.json').write_text(json.dumps(summary, indent=2, ensure_ascii=False) + '\n')
    print(json.dumps(summary['measured'], indent=2))


def gui_rerun(args):
    """Rebuilds only the GUI from the current source and repeats the GUI phases on the
    library of an earlier run, so render-cache settings can be compared on identical data."""
    out = ROOT / 'target/representative-benchmark' / args.reuse
    if not args.label.replace('-', '').isalnum() or (out / f'{args.label}.json').exists():
        raise ValueError('--label must be new and alphanumeric')
    bin_dir = out / 'prefix/bin'
    gui, evidence = build(['-p', 'cm-daemon', '--bin', 'citation-manager', '--features', 'desktop-bootstrap'],
                          'citation-manager')
    shutil.copy2(gui, bin_dir / 'citation-manager')
    seed = json.loads((out / 'seed.json').read_text())
    link = f"paper://{uuid.UUID(int=(1 << 96) | 1)}/attachment/{seed['attachments'][0]}/page/1"
    result = gui_measure(bin_dir, out / 'library', out, args.gui_settle, link, args.label)
    result['artifact'] = evidence
    result['git_head'] = capture(['git', 'rev-parse', 'HEAD'])
    result['git_diff_stat'] = capture(['git', 'diff', '--stat'])
    (out / f'{args.label}.json').write_text(json.dumps(result, indent=2) + '\n')
    print({k: v for k, v in result.items() if k in ('phases', 'gui_idle_rss_mib', 'pdf_view_rss_mib')})


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--out', required=True, help='new directory name under target/representative-benchmark')
    parser.add_argument('--papers', type=int, default=10000)
    parser.add_argument('--pdfs', type=int, default=1000)
    parser.add_argument('--pages', type=int, default=20)
    parser.add_argument('--samples', type=int, default=21)
    parser.add_argument('--warm-repeats', type=int, default=11)
    parser.add_argument('--small', action='store_true', help='smoke scale: 200 papers, 20 PDFs x 4 pages')
    parser.add_argument('--pdfium', help='pinned PDFium library; default: fetch with scripts/pdfium/fetch.py')
    parser.add_argument('--gui', action='store_true', help='Linux/Xvfb: also build and launch the GUI')
    parser.add_argument('--gui-settle', type=float, default=20.0)
    parser.add_argument('--reuse', help='GUI-only rerun on an earlier --out (e.g. another render cache build)')
    parser.add_argument('--label', default='gui', help='file prefix for a --reuse GUI rerun')
    parser.add_argument('--resummarize', help='recompute summary.json targets/cases from raw JSON in a directory')
    args = parser.parse_args()
    if args.resummarize:
        return resummarize(Path(args.resummarize))
    if args.reuse:
        return gui_rerun(args)
    if args.small:
        args.papers, args.pdfs, args.pages, args.samples, args.warm_repeats = 200, 20, 4, 5, 3
    if not args.out or Path(args.out).name != args.out or args.out in ('.', '..'):
        parser.error('--out must be a single new directory name')
    if not (1 <= args.pdfs <= args.papers and 1 <= args.pages <= 200 and 1 <= args.samples <= 200):
        parser.error('invalid scale')
    os.umask(0o077)
    parent = ROOT / 'target/representative-benchmark'
    parent.mkdir(parents=True, exist_ok=True)
    out = parent / args.out
    out.mkdir()  # refuse to overwrite earlier evidence
    env = environment()
    params = dict(papers=args.papers, pdfs=args.pdfs, pages_per_pdf=args.pages, pages=args.pdfs * args.pages,
                  samples=args.samples, warm_repeats=args.warm_repeats, seed=30)
    env['parameters'] = params
    (out / 'environment.json').write_text(json.dumps(env, indent=2) + '\n')
    timings = {}

    print('building release binaries', flush=True)
    t = time.monotonic()
    papersd, papersd_build = build(['-p', 'cm-daemon', '--bin', 'papersd', '--features', 'pdf-metadata'], 'papersd')
    ipc, ipc_build = build(['-p', 'cm-daemon', '--example', 'bench_representative_ipc'], 'bench_representative_ipc')
    seeder, seed_build = build(['-p', 'cm-storage', '--example', 'bench_representative_seed'], 'bench_representative_seed')
    gui = gui_build = None
    if args.gui:
        gui, gui_build = build(['-p', 'cm-daemon', '--bin', 'citation-manager', '--features', 'desktop-bootstrap'],
                               'citation-manager')
    timings['build_s'] = time.monotonic() - t
    env['artifacts'] = dict(papersd=papersd_build, ipc=ipc_build, seed=seed_build, gui=gui_build)
    if args.pdfium:
        pdfium = Path(args.pdfium).resolve()
    else:
        target = 'linux-x64' if platform.system() == 'Linux' else 'macos'
        fetched = subprocess.check_output([sys.executable, 'scripts/pdfium/fetch.py', '--platform', target,
                                           str(out / 'pdfium')], cwd=ROOT, text=True).strip()
        pdfium = Path(fetched)
    env['pdfium'] = dict(path=str(pdfium), sha256=hashlib.sha256(pdfium.read_bytes()).hexdigest())
    bin_dir = stage(papersd, pdfium, out, gui)
    (out / 'environment.json').write_text(json.dumps(env, indent=2) + '\n')

    print(f'generating corpus: {args.pdfs} PDFs x {args.pages} pages', flush=True)
    t = time.monotonic()
    oracle = pdfgen.write_corpus(out / 'corpus', 30, args.pdfs, args.pages)
    timings['corpus_s'] = time.monotonic() - t
    (out / 'oracle.json').write_text(json.dumps(oracle) + '\n')
    library = out / 'library'
    print(f'seeding {args.papers} papers', flush=True)
    with (out / 'seed.json').open('w') as fh:
        run([str(seeder), 'seed', str(library), str(out / 'corpus'), str(args.papers), str(args.pdfs)], stdout=fh)
    seed = json.loads((out / 'seed.json').read_text())

    runtime = Path(tempfile.mkdtemp(prefix='cm30-', dir='/tmp')).resolve()
    try:
        # Indexing: papersd starts automatic extraction of pending PDFs on start and exits
        # after the idle timeout once no job is active.
        print('indexing (automatic extraction by papersd)', flush=True)
        idle_ms = 2000
        log = (out / 'papersd-index.log').open('w')
        t = time.monotonic()
        daemon = spawn_papersd(bin_dir, library, runtime, idle_ms, log)
        with Sampler(lambda: dict(papersd=daemon.pid), 0.5) as sampler:
            code, usage = wait_rusage(daemon, 6 * 3600)
        wall = time.monotonic() - t
        log.close()
        if code != 0:
            raise ValueError(f'indexing papersd exited {code}')
        index = dict(wall_s_including_idle_exit=wall, idle_ms=idle_ms, rusage=usage,
                     peak_rss_kib=sampler.peak('papersd'), last_io=sampler.last('papersd'),
                     samples=sampler.samples)
        with (out / 'stats.json').open('w') as fh:
            run([str(seeder), 'stats', str(library)], stdout=fh)
        stats = json.loads((out / 'stats.json').read_text())
        extract_jobs = [j for j in stats['jobs'] if j['kind'] == 'fulltext.extract']
        if extract_jobs:
            index['extract_jobs_span_s'] = (max(j['updated_at_ms'] for j in extract_jobs)
                                            - min(j['created_at_ms'] for j in extract_jobs)) / 1000
        if stats['fulltext_pages'] != args.pdfs * args.pages:
            raise ValueError(f"indexed {stats['fulltext_pages']} pages, expected {args.pdfs * args.pages}")
        (out / 'index.json').write_text(json.dumps(index, indent=2) + '\n')

        print('warm start', flush=True)
        with (out / 'warm.json').open('w') as fh:
            run([str(ipc), 'warm-start', str(bin_dir / 'papersd'), str(library), str(runtime),
                 str(args.warm_repeats), '1500'], stdout=fh)
        warm = json.loads((out / 'warm.json').read_text())

        print('queries', flush=True)
        log = (out / 'papersd-queries.log').open('w')
        daemon = spawn_papersd(bin_dir, library, runtime, 1500, log)
        with Sampler(lambda: dict(papersd=daemon.pid), 0.25) as sampler:
            with (out / 'queries.json').open('w') as fh:
                run([str(ipc), 'queries', str(library), str(runtime), str(out / 'oracle.json'),
                     str(out / 'seed.json'), str(args.samples)], stdout=fh)
            code, usage = wait_rusage(daemon, 120)
        log.close()
        queries = json.loads((out / 'queries.json').read_text())
        queries['rusage'] = usage
        queries['peak_rss_kib_sampled'] = sampler.peak('papersd')
        queries['last_io'] = sampler.last('papersd')
        (out / 'queries.json').write_text(json.dumps(queries, indent=2) + '\n')
        (out / 'query-samples.json').write_text(json.dumps(sampler.samples) + '\n')

        gui_result = None
        if args.gui:
            print('GUI idle RSS (Xvfb)', flush=True)
            owner = int(seed.get('first_pdf_owner', 0))
            paper = uuid.UUID(int=(1 << 96) | (owner + 1))
            link = f"paper://{paper}/attachment/{seed['attachments'][0]}/page/1"
            gui_result = gui_measure(bin_dir, library, out, args.gui_settle, link)
            (out / 'gui.json').write_text(json.dumps(gui_result, indent=2) + '\n')
    finally:
        shutil.rmtree(runtime, ignore_errors=True)

    summary = summarize(queries, warm, index, stats, oracle, gui_result)
    summary.update(parameters=params, timings=timings, seed=dict(
        papers=seed['papers'], pdfs=seed['pdfs'], pdf_bytes=seed['pdf_bytes'], attach_ms=seed['attach_ms']),
        index=dict(wall_s_including_idle_exit=index['wall_s_including_idle_exit'],
                   extract_jobs_span_s=index.get('extract_jobs_span_s'), rusage=index['rusage'],
                   peak_rss_kib=index['peak_rss_kib'], last_io=index['last_io']),
        stats={k: stats[k] for k in ('integrity_check', 'papers', 'attachments', 'fulltext_pages',
                                     'fulltext_text_bytes', 'search_documents', 'extraction_states',
                                     'index_bytes', 'index_bytes_total', 'file_bytes')},
        corpus=dict(generated_chars=oracle['generated_chars'], pdf_bytes=oracle['pdf_bytes'],
                    corpus_sha256=oracle['corpus_sha256'],
                    terms={k: v['pages'] for k, v in oracle['terms'].items()}),
        warm_runs=[dict(repeat=r['repeat'], first_page_ms=r['first_page_ms'], connected_ms=r['connected_ms'],
                        rss_kib=r['rss_kib_after_first_page']) for r in warm['runs']],
        queries_rusage=queries['rusage'], queries_rss_kib_before=queries['rss_kib_before'],
        queries_rss_kib_after=queries['rss_kib_after'],
        gui={k: v for k, v in (gui_result or {}).items() if k != 'samples'},
        environment_file='environment.json')
    (out / 'summary.json').write_text(json.dumps(summary, indent=2, ensure_ascii=False) + '\n')
    for row in summary['targets']:
        print(f"{row['metric']}: {row['measured']} ({row['status']}; {row['target']})")
    print(out)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f'benchmark failed: {error}', file=sys.stderr)
        sys.exit(1)
