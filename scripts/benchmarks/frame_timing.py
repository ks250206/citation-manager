#!/usr/bin/env python3
"""#86 perceived performance: frame times and named spans of the real release GUI.

Opt-in only (never part of `cargo test` or PR CI). Builds the release GUI and `papersd`,
generates and indexes the #30 representative library (10,000 papers / 1,000 PDFs / 20,000
pages by default, scripts/benchmarks/representative.py primitives), starts the GUI with the
recorder enabled (`CM_FRAME_TIMING=<this run>/frames.json`, see crates/desktop/src/frame_timing.rs)
and GPUI's own per-frame draw+present log (`ZED_MEASUREMENTS=1`, stderr), then drives
scripted input and reports p50/p95/max against the 60 Hz (16.7 ms) and 120 Hz (8.3 ms)
frame budgets.

Input: Linux uses an X server (`--xvfb` starts a private Xvfb) and xdotool/xclip. macOS posts
Quartz events through ctypes; if the process lacks the Accessibility permission macOS drops
them silently, which shows up as phases without the expected spans (reported, never
counted as measured). Results go to target/frame-timing-benchmark/<NAME>.

    python3 scripts/benchmarks/frame_timing.py --xvfb --out linux            # 10k/1k/20k
    python3 scripts/benchmarks/frame_timing.py --xvfb --small --out smoke    # tiny smoke
    python3 scripts/benchmarks/frame_timing.py --xvfb --reuse linux --out again  # same library
"""
import argparse
import ctypes
import ctypes.util
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import platform
import re
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
from benchmarks import representative as rep  # noqa: E402
from benchmarks.run import quantile  # noqa: E402

BUDGETS_MS = {'60hz': 1000 / 60, '120hz': 1000 / 120}
PARENT = ROOT / 'target/frame-timing-benchmark'
# Window-relative points (logical px) of the default 1320x860 window and layout, measured
# from Linux screenshots of this build; the doc keeps the screenshots that confirm them.
LIST_CENTER = (580, 450)
LIST_NEXT_PAGE = (884, 797)
EXPAND = (1269, 73)
PDF_PAGE = (660, 520)  # page column after Expand
GPUI_FRAME = re.compile(r'^frame duration: ([0-9.]+)(ns|µs|us|ms|s)$')
UNITS_MS = {'ns': 1e-6, 'µs': 1e-3, 'us': 1e-3, 'ms': 1.0, 's': 1000.0}


# ---------------------------------------------------------------- summary (pure, tested)

def stats(values, budgets=BUDGETS_MS):
    """Nearest-rank p50/p95 plus max and, for frame durations, the share over each budget (ms)."""
    if not values:
        return None
    row = dict(n=len(values), p50_ms=quantile(values, .5), p95_ms=quantile(values, .95), max_ms=max(values))
    for name, budget in budgets.items():
        over = sum(1 for v in values if v > budget)
        row[f'over_{name}'] = over
        row[f'over_{name}_share'] = over / len(values)
    return row


def parse_gpui_frame(line):
    """One GPUI `measure("frame duration")` stderr line (a Rust Debug duration) in ms, or None."""
    match = GPUI_FRAME.match(line.strip())
    return float(match.group(1)) * UNITS_MS[match.group(2)] if match else None


def summarize(doc, gpui, phases, budgets=BUDGETS_MS):
    """`doc`: the recorder file. `gpui`: [unix time the line arrived, draw+present ms] for GPUI's
    own per-draw log, timestamped by the harness as each stderr line arrived (frame end). GPUI
    also draws outside its frame callback (window open, resize), so the two series are not
    paired one-to-one; both are attributed to phases by time."""
    origin = doc['origin_unix_us']
    frames = [((origin + start) / 1e6, duration / 1000) for start, duration in doc['frames']]
    spans = [dict(s, start_unix=(origin + s['start_us']) / 1e6,
                  end_unix=None if s['end_us'] is None else (origin + s['end_us']) / 1e6)
             for s in doc['spans']]

    def in_window(t, phase):
        return phase['start_unix'] <= t <= phase['end_unix']

    def span_stats(rows):
        done = [s['duration_us'] / 1000 for s in rows if s['completed'] and s['duration_us'] is not None]
        return dict(completed=len(done), incomplete=sum(1 for s in rows if not s['completed']),
                    duration=stats(done, budgets={}))

    by_phase = {}
    for name, phase in phases.items():
        index = [i for i, (t, _) in enumerate(frames) if in_window(t, phase)]
        rows = [s for s in spans if in_window(s['start_unix'], phase)]
        kinds = sorted({s['kind'] for s in rows})
        entry = dict(window_s=phase['end_unix'] - phase['start_unix'],
                     app_frame=stats([frames[i][1] for i in index], budgets),
                     gpui_draw_present=stats([ms for t, ms in gpui if in_window(t, phase)], budgets),
                     spans={k: span_stats([s for s in rows if s['kind'] == k]) for k in kinds},
                     expected=phase.get('expect'))
        expect = phase.get('expect')
        entry['observed'] = (expect is None
                             or entry['spans'].get(expect, {}).get('completed', 0) > 0)
        if 'input_unix' in phase:
            first = sorted(s['end_unix'] for s in spans if s['kind'] == 'pdf_first_page'
                           and s['completed'] and s['end_unix'] and s['end_unix'] >= phase['input_unix'])
            entry['input_to_first_page_ms'] = (first[0] - phase['input_unix']) * 1000 if first else None
        by_phase[name] = entry
    kinds = sorted({s['kind'] for s in spans})
    return dict(
        quantile='nearest rank sorted[ceil(p*N)-1]',
        budgets_ms=budgets,
        frame_count=doc['frame_count'], recorded_frames=len(frames), dropped_frames=doc['dropped_frames'],
        gpui_logged_frames=len(gpui),
        all_frames=dict(app_frame=stats([d for _, d in frames], budgets),
                        gpui_draw_present=stats([ms for _, ms in gpui], budgets)),
        spans={k: span_stats([s for s in spans if s['kind'] == k]) for k in kinds},
        open_spans=len(doc.get('open_spans', [])),
        phases=by_phase)


# ---------------------------------------------------------------- input drivers

class X11:
    """xdotool/xclip on the X display in $DISPLAY (window-relative coordinates)."""
    name = 'xdotool'

    def __init__(self, pid):
        out = subprocess.check_output(['xdotool', 'search', '--sync', '--onlyvisible', '--pid', str(pid)],
                                      text=True, timeout=60)
        self.window = out.split()[0]
        subprocess.run(['xdotool', 'windowactivate', '--sync', self.window], check=False, timeout=10)
        geometry = subprocess.check_output(['xdotool', 'getwindowgeometry', '--shell', self.window], text=True)
        values = dict(line.split('=', 1) for line in geometry.split())
        self.origin = (int(values['X']), int(values['Y']))

    def _at(self, point):
        return [str(self.origin[0] + point[0]), str(self.origin[1] + point[1])]

    def wheel(self, point, down, count, delay_ms):
        subprocess.run(['xdotool', 'mousemove', *self._at(point)], check=True)
        subprocess.run(['xdotool', 'click', '--repeat', str(count), '--delay', str(delay_ms), '5' if down else '4'],
                       check=True)

    def click(self, point):
        subprocess.run(['xdotool', 'mousemove', *self._at(point), 'click', '1'], check=True)

    def key(self, key, count=1, delay_ms=50):
        subprocess.run(['xdotool', 'key', '--repeat', str(count), '--delay', str(delay_ms), key], check=True)

    def primary(self, key):
        self.key(f'ctrl+{key}')

    def type(self, text):
        subprocess.run(['xdotool', 'type', '--delay', '30', text], check=True)

    def clipboard(self, text):
        subprocess.run(['xclip', '-selection', 'clipboard'], input=text.encode(), check=True)

    def screenshot(self, path):
        if shutil.which('import'):
            subprocess.run(['import', '-window', 'root', str(path)], check=False, timeout=30)


class Quartz:
    """macOS: Quartz events posted with ctypes (no third-party module). Without the
    Accessibility permission macOS drops them silently; the summary then shows the phases
    as not observed instead of pretending they were measured."""
    name = 'quartz-cgevent'
    KEYS = {'Return': 36, 'j': 38, 'k': 40, 'J': 38, 'K': 40, 'q': 12}
    SHIFT, COMMAND = 0x20000, 0x100000

    class Point(ctypes.Structure):
        _fields_ = [('x', ctypes.c_double), ('y', ctypes.c_double)]

    class Rect(ctypes.Structure):
        _fields_ = [('x', ctypes.c_double), ('y', ctypes.c_double), ('w', ctypes.c_double), ('h', ctypes.c_double)]

    def __init__(self, pid, size=(1320, 860)):
        cg = ctypes.cdll.LoadLibrary(ctypes.util.find_library('ApplicationServices'))
        cf = ctypes.cdll.LoadLibrary(ctypes.util.find_library('CoreFoundation'))
        self.cg, self.cf = cg, cf
        cg.CGMainDisplayID.restype = ctypes.c_uint32
        cg.CGDisplayBounds.restype = self.Rect
        cg.CGDisplayBounds.argtypes = [ctypes.c_uint32]
        cg.CGEventCreateMouseEvent.restype = ctypes.c_void_p
        cg.CGEventCreateMouseEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint32, self.Point, ctypes.c_uint32]
        # The non-variadic variant (macOS 13+): variadic ctypes calls are unreliable on arm64.
        cg.CGEventCreateScrollWheelEvent2.restype = ctypes.c_void_p
        cg.CGEventCreateScrollWheelEvent2.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32,
                                                      ctypes.c_int32, ctypes.c_int32, ctypes.c_int32]
        cg.CGEventCreateKeyboardEvent.restype = ctypes.c_void_p
        cg.CGEventCreateKeyboardEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint16, ctypes.c_bool]
        cg.CGEventSetFlags.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
        cg.CGEventKeyboardSetUnicodeString.argtypes = [ctypes.c_void_p, ctypes.c_ulong,
                                                       ctypes.POINTER(ctypes.c_uint16)]
        cg.CGEventPost.argtypes = [ctypes.c_uint32, ctypes.c_void_p]
        cf.CFRelease.argtypes = [ctypes.c_void_p]
        cg.AXIsProcessTrusted.restype = ctypes.c_bool
        self.trusted = bool(cg.AXIsProcessTrusted())
        display = cg.CGDisplayBounds(cg.CGMainDisplayID())
        self.display = (display.w, display.h)
        # GPUI centres the window on the main display (the title bar sits above the content).
        self.origin = ((display.w - size[0]) / 2, (display.h - size[1]) / 2 + 14)
        subprocess.run(['osascript', '-e', f'tell application "System Events" to set frontmost of '
                        f'(first process whose unix id is {pid}) to true'], check=False, timeout=20,
                       capture_output=True)

    def _post(self, event):
        self.cg.CGEventPost(0, event)
        self.cf.CFRelease(event)

    def _point(self, point):
        return self.Point(self.origin[0] + point[0], self.origin[1] + point[1])

    def click(self, point):
        at = self._point(point)
        self._post(self.cg.CGEventCreateMouseEvent(None, 5, at, 0))  # moved
        time.sleep(0.05)
        self._post(self.cg.CGEventCreateMouseEvent(None, 1, at, 0))  # left down
        self._post(self.cg.CGEventCreateMouseEvent(None, 2, at, 0))  # left up

    def wheel(self, point, down, count, delay_ms):
        self._post(self.cg.CGEventCreateMouseEvent(None, 5, self._point(point), 0))
        for _ in range(count):
            self._post(self.cg.CGEventCreateScrollWheelEvent2(None, 1, 1, -3 if down else 3, 0, 0))
            time.sleep(delay_ms / 1000)

    def _key(self, code, flags=0):
        for down in (True, False):
            event = self.cg.CGEventCreateKeyboardEvent(None, code, down)
            if flags:
                self.cg.CGEventSetFlags(event, flags)
            self._post(event)

    def key(self, key, count=1, delay_ms=50):
        for _ in range(count):
            self._key(self.KEYS[key], self.SHIFT if key.isupper() else 0)
            time.sleep(delay_ms / 1000)

    def primary(self, key):
        self._key(self.KEYS.get(key, 40), self.COMMAND)

    def type(self, text):
        for char in text:
            for down in (True, False):
                event = self.cg.CGEventCreateKeyboardEvent(None, 0, down)
                units = (ctypes.c_uint16 * 1)(ord(char))
                self.cg.CGEventKeyboardSetUnicodeString(event, 1, units)
                self._post(event)
            time.sleep(0.03)

    def clipboard(self, text):
        subprocess.run(['pbcopy'], input=text.encode(), check=True)

    def screenshot(self, path):
        subprocess.run(['screencapture', '-x', str(path)], check=False, timeout=30)


# ---------------------------------------------------------------- run

@contextmanager
def xvfb(enabled):
    if not enabled:
        yield
        return
    for number in range(91, 120):
        if not Path(f'/tmp/.X11-unix/X{number}').exists():
            break
    server = subprocess.Popen(['Xvfb', f':{number}', '-screen', '0', '1400x900x24', '-nolisten', 'tcp'],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    previous = os.environ.get('DISPLAY')
    os.environ['DISPLAY'] = f':{number}'
    try:
        time.sleep(1.5)
        yield
    finally:
        server.terminate()
        server.wait(10)
        if previous is None:
            os.environ.pop('DISPLAY', None)
        else:
            os.environ['DISPLAY'] = previous


def prepare_library(out, args, env):
    """#30 dataset in `out`: release binaries staged with the pinned PDFium, the seeded
    library and its full-text index (so no extraction job competes with the GUI)."""
    papersd, papersd_build = rep.build(['-p', 'cm-daemon', '--bin', 'papersd', '--features', 'pdf-metadata'],
                                       'papersd')
    seeder, seed_build = rep.build(['-p', 'cm-storage', '--example', 'bench_representative_seed'],
                                   'bench_representative_seed')
    gui, gui_build = build_gui()
    env['artifacts'] = dict(papersd=papersd_build, seed=seed_build, gui=gui_build)
    if args.pdfium:
        pdfium = Path(args.pdfium).resolve()
    else:
        target = 'linux-x64' if platform.system() == 'Linux' else 'macos'
        pdfium = Path(subprocess.check_output([sys.executable, 'scripts/pdfium/fetch.py', '--platform', target,
                                               str(out / 'pdfium')], cwd=ROOT, text=True).strip())
    env['pdfium_sha256'] = hashlib.sha256(pdfium.read_bytes()).hexdigest()
    bin_dir = rep.stage(papersd, pdfium, out, gui)
    print(f'generating corpus: {args.pdfs} PDFs x {args.pages} pages', flush=True)
    pdfgen.write_corpus(out / 'corpus', 30, args.pdfs, args.pages)
    library = out / 'library'
    print(f'seeding {args.papers} papers', flush=True)
    with (out / 'seed.json').open('w') as fh:
        rep.run([str(seeder), 'seed', str(library), str(out / 'corpus'), str(args.papers), str(args.pdfs)],
                stdout=fh)
    print('indexing (automatic extraction by papersd)', flush=True)
    runtime = Path(tempfile.mkdtemp(prefix='cm86-', dir='/tmp')).resolve()
    try:
        with (out / 'papersd-index.log').open('w') as log:
            started = time.monotonic()
            code, _ = rep.wait_rusage(rep.spawn_papersd(bin_dir, library, runtime, 2000, log), 6 * 3600)
        if code != 0:
            raise ValueError(f'indexing papersd exited {code}')
        env['index_wall_s'] = time.monotonic() - started
    finally:
        shutil.rmtree(runtime, ignore_errors=True)
    shutil.rmtree(out / 'corpus', ignore_errors=True)  # imported copies live in the library
    return bin_dir, library


def build_gui():
    return rep.build(['-p', 'cm-daemon', '--bin', 'citation-manager', '--features', 'desktop-bootstrap'],
                     'citation-manager')


def load(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def wait_span(path, kind, after_unix, timeout):
    """Polls the recorder file until a completed span of `kind` ends after `after_unix`."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        doc = load(path)
        if doc:
            for span in doc['spans']:
                end = span['end_us']
                if span['kind'] == kind and span['completed'] and end is not None \
                        and (doc['origin_unix_us'] + end) / 1e6 >= after_unix:
                    return True
        time.sleep(0.5)
    return False


def drive(bin_dir, library, out, link, settle_s):
    frames_path = out / 'frames.json'
    runtime = Path(tempfile.mkdtemp(prefix='cmf-', dir='/tmp'))
    env = dict(os.environ, CM_FRAME_TIMING=str(frames_path), ZED_MEASUREMENTS='1', XDG_RUNTIME_DIR=str(runtime))
    log = (out / 'gui.log').open('w')
    started = time.time()
    gui = subprocess.Popen([str(bin_dir / 'citation-manager'), '--library', str(library),
                            '--settings', str(out / 'gui-settings.json')],
                           stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.PIPE, env=env,
                           text=True, errors='replace', bufsize=1)
    gpui = []

    def read_stderr():
        # GPUI prints each draw+present duration with eprintln (unbuffered): the arrival time
        # of the line is the frame's end within pipe latency.
        for line in gui.stderr:
            ms = parse_gpui_frame(line)
            if ms is None:
                log.write(line)
            else:
                gpui.append([time.time(), ms])

    reader = threading.Thread(target=read_stderr, daemon=True)
    reader.start()
    phases = {}
    notes = []
    driver = None

    @contextmanager
    def phase(name, expect=None, tail_s=1.5):
        entry = dict(start_unix=time.time(), expect=expect)
        try:
            yield entry
        finally:
            time.sleep(tail_s)
            entry['end_unix'] = time.time()
            phases[name] = entry

    try:
        driver = X11(gui.pid) if platform.system() == 'Linux' else Quartz(gui.pid)
        if isinstance(driver, Quartz):
            notes.append(f'macOS AXIsProcessTrusted={driver.trusted}, main display {driver.display}')
        with phase('startup', tail_s=0) as entry:
            entry['start_unix'] = started
            time.sleep(settle_s)
        driver.screenshot(out / 'shot-idle.png')
        with phase('list_wheel', expect='list_scroll'):
            for _ in range(3):
                driver.wheel(LIST_CENTER, True, 40, 40)
                driver.wheel(LIST_CENTER, False, 40, 40)
        with phase('list_keys', expect='list_scroll'):
            driver.click(LIST_CENTER)
            time.sleep(1)
            driver.key('j', 80, 60)   # vim Normal mode (default on): next row, details follow
            driver.key('k', 80, 60)
        with phase('list_pages', expect=None, tail_s=2):
            for _ in range(5):
                driver.click(LIST_NEXT_PAGE)
                time.sleep(1.5)
        driver.screenshot(out / 'shot-list.png')
        with phase('pdf_open', expect='pdf_first_page', tail_s=1) as entry:
            driver.clipboard(link)
            driver.click(LIST_CENTER)  # focus the table so the palette shortcut is in scope
            time.sleep(0.5)
            driver.primary('k')
            time.sleep(1)
            driver.type('Open paper:// Link')
            time.sleep(1)
            entry['input_unix'] = time.time()
            driver.key('Return')
            entry['first_page_seen'] = wait_span(frames_path, 'pdf_first_page', entry['input_unix'], 60)
        time.sleep(2)
        driver.screenshot(out / 'shot-pdf-open.png')
        driver.click(EXPAND)  # the reading layout: a wide page column, larger bitmaps
        time.sleep(3)
        driver.screenshot(out / 'shot-pdf-expanded.png')
        driver.click(PDF_PAGE)  # focus the page column for the vim J/K/j/k keys
        time.sleep(1)
        with phase('page_turn_forward', expect='page_turn'):
            driver.key('J', 19, 700)
        driver.screenshot(out / 'shot-pdf-last.png')
        with phase('page_turn_back', expect='page_turn'):
            driver.key('K', 19, 700)
        with phase('pdf_wheel', expect='pdf_scroll'):
            for _ in range(2):
                driver.wheel(PDF_PAGE, True, 60, 40)
                driver.wheel(PDF_PAGE, False, 60, 40)
        with phase('pdf_keys', expect='pdf_scroll'):
            driver.key('j', 60, 50)
            driver.key('k', 60, 50)
        driver.screenshot(out / 'shot-pdf-end.png')
        with phase('idle', tail_s=0):
            time.sleep(5)
        driver.primary('q')  # Quit: the app writes the recorder file once more on exit
        try:
            gui.wait(15)
        except subprocess.TimeoutExpired:
            notes.append('quit shortcut did not end the GUI; terminated (last <1 s of frames may be missing)')
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        notes.append(f'driver error: {error!r}')
    finally:
        if gui.poll() is None:
            gui.terminate()
            try:
                gui.wait(20)
            except subprocess.TimeoutExpired:
                gui.kill()
                gui.wait()
        reader.join(10)
        log.close()
        shutil.rmtree(runtime, ignore_errors=True)
    return dict(started_unix=started, phases=phases, notes=notes, exit_code=gui.returncode,
                driver=getattr(driver, 'name', None)), gpui


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--out', required=True, help='new directory name under target/frame-timing-benchmark')
    parser.add_argument('--papers', type=int, default=10000)
    parser.add_argument('--pdfs', type=int, default=1000)
    parser.add_argument('--pages', type=int, default=20)
    parser.add_argument('--small', action='store_true', help='smoke scale: 200 papers, 20 PDFs x 4 pages')
    parser.add_argument('--pdfium', help='pinned PDFium library; default: fetch with scripts/pdfium/fetch.py')
    parser.add_argument('--reuse', help='earlier --out of this script: same library, rebuilt GUI')
    parser.add_argument('--xvfb', action='store_true', help='Linux: run on a private Xvfb 1400x900x24')
    parser.add_argument('--settle', type=float, default=15.0, help='seconds after launch before input')
    args = parser.parse_args()
    if args.small:
        args.papers, args.pdfs, args.pages = 200, 20, 4
    if Path(args.out).name != args.out or args.out in ('.', '..'):
        parser.error('--out must be a single new directory name')
    os.umask(0o077)
    PARENT.mkdir(parents=True, exist_ok=True)
    out = PARENT / args.out
    out.mkdir()  # refuse to overwrite earlier evidence
    env = rep.environment()
    env['parameters'] = dict(papers=args.papers, pdfs=args.pdfs, pages_per_pdf=args.pages, seed=30,
                             reuse=args.reuse, xvfb=args.xvfb, settle_s=args.settle)
    if args.reuse:
        source = PARENT / args.reuse
        bin_dir = next(p for p in (source / 'prefix/bin', source / 'Bench.app/Contents/MacOS') if p.exists())
        gui, env['artifacts'] = build_gui()
        shutil.copy2(gui, bin_dir / 'citation-manager')
        library = source / 'library'
        env['parameters'].update(json.loads((source / 'environment.json').read_text())['parameters'],
                                 reuse=args.reuse)
        seed = json.loads((source / 'seed.json').read_text())
    else:
        bin_dir, library = prepare_library(out, args, env)
        seed = json.loads((out / 'seed.json').read_text())
    paper = uuid.UUID(int=(1 << 96) | (int(seed.get('first_pdf_owner', 0)) + 1))
    link = f"paper://{paper}/attachment/{seed['attachments'][0]}/page/1"
    if platform.system() == 'Linux':
        env['x_tools'] = {t: shutil.which(t) is not None for t in ('Xvfb', 'xdotool', 'xclip', 'import')}
        env['vulkan_icds'] = sorted(p.name for p in Path('/usr/share/vulkan/icd.d').glob('*.json')) \
            if Path('/usr/share/vulkan/icd.d').exists() else []
    (out / 'environment.json').write_text(json.dumps(env, indent=2) + '\n')
    print('driving the GUI', flush=True)
    with xvfb(args.xvfb):
        env['display'] = os.environ.get('DISPLAY')
        run, gpui = drive(bin_dir, library, out, link, args.settle)
    (out / 'environment.json').write_text(json.dumps(env, indent=2) + '\n')
    (out / 'phases.json').write_text(json.dumps(run, indent=2) + '\n')
    (out / 'gpui-frames.json').write_text(json.dumps(dict(
        format=['arrival_unix_s', 'draw_present_ms'],
        source='GPUI ZED_MEASUREMENTS=1 "frame duration" (Window::draw + present), stderr', frames=gpui)) + '\n')
    doc = load(out / 'frames.json')
    if doc is None:
        raise ValueError('the GUI wrote no frames.json (recorder not enabled or GUI failed); see gui.log')
    summary = summarize(doc, gpui, run['phases'])
    summary.update(notes=run['notes'], driver=run['driver'], gui_exit_code=run['exit_code'],
                   environment_file='environment.json')
    (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    for name, entry in summary['phases'].items():
        frame = entry['app_frame'] or {}
        present = entry['gpui_draw_present'] or {}
        print(f"{name}: frames={frame.get('n')} app p50/p95/max={frame.get('p50_ms')}/{frame.get('p95_ms')}/"
              f"{frame.get('max_ms')} ms; draw+present p95={present.get('p95_ms')}; "
              f"spans={ {k: v['duration'] for k, v in entry['spans'].items()} }; observed={entry['observed']}")
    print(out)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f'benchmark failed: {error}', file=sys.stderr)
        sys.exit(1)
