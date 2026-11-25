//! #86 Opt-in perceived-performance recorder: per-frame durations and named spans.
//!
//! Off unless [`ENV`] names an **absolute** output file when the GUI starts. When off, every
//! hook is one `OnceLock` load and a branch: nothing is allocated, timed or written. When on,
//! the GUI thread only appends to in-memory buffers; a background thread moves the new
//! entries out about once a second and rewrites the JSON file atomically (temp + rename), so
//! a benchmark driver may stop the app with a signal and lose at most the last second.
//!
//! What a "frame" is here: the app-side work of one window draw, from the root view's
//! `render` to the paint of a marker element appended after the whole tree (render, layout,
//! prepaint and paint of the scene). Presenting the scene to the GPU/compositor happens after
//! that and is *not* included; see `scripts/benchmarks/frame_timing.py` for the separate
//! draw+present measurement.
//!
//! Spans:
//! - `list_scroll` / `pdf_scroll` (motion): open on the first frame whose scroll offset differs
//!   from the previous frame's, close [`MOTION_IDLE`] after the last frame that moved.
//! - `pdf_first_page`: a PDF viewer was created → the first frame that paints a visible page.
//! - `page_turn`: a page jump (toolbar ‹/›, page field, vim J/K) → the first frame that paints
//!   the target page while it is visible.
//!
//! Every timestamp is microseconds since a monotonic origin whose Unix time is recorded, so an
//! external driver can line the frames up with the input it sent.
use serde_json::{Value, json};
use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Environment variable naming the output file (absolute path). Unset or empty: disabled.
pub const ENV: &str = "CM_FRAME_TIMING";
pub const SCHEMA: &str = "cm-frame-timing/1";
/// A motion span ends this long after the last frame whose offset changed.
pub const MOTION_IDLE: Duration = Duration::from_millis(300);
/// Memory bound: about 2.3 h of continuous 60 fps frames; later frames are only counted.
pub const MAX_FRAMES: usize = 500_000;
pub const MAX_SPANS: usize = 100_000;
const FLUSH_EVERY: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
    ListScroll,
    PdfScroll,
    PdfFirstPage,
    PageTurn,
}

impl Span {
    pub fn name(self) -> &'static str {
        match self {
            Span::ListScroll => "list_scroll",
            Span::PdfScroll => "pdf_scroll",
            Span::PdfFirstPage => "pdf_first_page",
            Span::PageTurn => "page_turn",
        }
    }
}

/// One closed (or, in snapshots, still open) span.
#[derive(Clone, Debug, PartialEq)]
pub struct SpanRecord {
    pub kind: Span,
    pub start_us: u64,
    pub end_us: Option<u64>,
    /// Frame indices (0-based, in recording order) the span covers.
    pub first_frame: Option<u64>,
    pub last_frame: Option<u64>,
    /// Target page for `page_turn`.
    pub page: Option<u32>,
    /// `false` when a newer span of the same kind replaced it before it completed.
    pub completed: bool,
}

impl SpanRecord {
    fn to_json(&self) -> Value {
        json!({
            "kind": self.kind.name(),
            "start_us": self.start_us,
            "end_us": self.end_us,
            "duration_us": self.end_us.map(|end| end.saturating_sub(self.start_us)),
            "first_frame": self.first_frame,
            "last_frame": self.last_frame,
            "page": self.page,
            "completed": self.completed,
        })
    }
}

struct Target {
    record: SpanRecord,
    satisfied: bool,
}

struct Motion {
    kind: Span,
    last_value: Option<f32>,
    changed_this_frame: bool,
    open: Option<SpanRecord>,
    last_change: Option<Instant>,
}

/// New entries since the previous [`Recorder::drain`].
#[derive(Default, Debug)]
pub struct Delta {
    /// `(start_us, duration_us)` per frame.
    pub frames: Vec<(u64, u32)>,
    pub spans: Vec<SpanRecord>,
    /// Snapshot of spans still open.
    pub open: Vec<SpanRecord>,
    pub frame_count: u64,
}

pub struct Recorder {
    origin: Instant,
    origin_unix_us: u64,
    frame_open: Option<Instant>,
    frame_count: u64,
    frames: Vec<(u64, u32)>,
    spans: Vec<SpanRecord>,
    targets: Vec<Target>,
    motions: Vec<Motion>,
}

impl Recorder {
    pub fn new(origin: Instant, origin_unix_us: u64) -> Self {
        Self {
            origin,
            origin_unix_us,
            frame_open: None,
            frame_count: 0,
            frames: Vec::new(),
            spans: Vec::new(),
            targets: Vec::new(),
            motions: Vec::new(),
        }
    }

    pub fn origin_unix_us(&self) -> u64 {
        self.origin_unix_us
    }

    fn us(&self, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.origin).as_micros()).unwrap_or(u64::MAX)
    }

    /// Root view render begins. A frame that never reached its end marker is discarded.
    pub fn frame_start(&mut self, now: Instant) {
        self.close_idle_motions(now);
        self.frame_open = Some(now);
        for motion in &mut self.motions {
            motion.changed_this_frame = false;
        }
    }

    /// The end marker painted: the frame's app-side work is done.
    pub fn frame_end(&mut self, now: Instant) {
        let Some(start) = self.frame_open.take() else {
            return;
        };
        let index = self.frame_count;
        self.frame_count += 1;
        let duration =
            u32::try_from(now.saturating_duration_since(start).as_micros()).unwrap_or(u32::MAX);
        let start_us = self.us(start);
        let end_us = self.us(now);
        self.frames.push((start_us, duration));
        for motion in &mut self.motions {
            if motion.changed_this_frame {
                motion.last_change = Some(now);
                if let Some(open) = &mut motion.open {
                    open.first_frame.get_or_insert(index);
                    open.last_frame = Some(index);
                    open.end_us = Some(end_us);
                }
            }
        }
        let mut closed = Vec::new();
        self.targets.retain_mut(|target| {
            target.record.first_frame.get_or_insert(index);
            if target.satisfied {
                target.record.last_frame = Some(index);
                target.record.end_us = Some(end_us);
                target.record.completed = true;
                closed.push(target.record.clone());
                false
            } else {
                true
            }
        });
        self.spans.extend(closed);
    }

    /// A scroll offset observed while rendering the current frame.
    pub fn motion(&mut self, kind: Span, value: f32) {
        let Some(frame_start) = self.frame_open else {
            return;
        };
        let start_us = self.us(frame_start);
        let motion = match self.motions.iter().position(|m| m.kind == kind) {
            Some(i) => &mut self.motions[i],
            None => {
                self.motions.push(Motion {
                    kind,
                    last_value: None,
                    changed_this_frame: false,
                    open: None,
                    last_change: None,
                });
                self.motions.last_mut().expect("just pushed")
            }
        };
        let previous = motion.last_value.replace(value);
        if previous.is_some_and(|p| p != value) {
            motion.changed_this_frame = true;
            motion.open.get_or_insert(SpanRecord {
                kind,
                start_us,
                end_us: None,
                first_frame: None,
                last_frame: None,
                page: None,
                completed: true,
            });
        }
    }

    fn close_idle_motions(&mut self, now: Instant) {
        for motion in &mut self.motions {
            let idle = motion
                .last_change
                .is_none_or(|at| now.saturating_duration_since(at) >= MOTION_IDLE);
            if idle && let Some(span) = motion.open.take() {
                self.spans.push(span);
            }
        }
    }

    /// Starts a span completed by a later painted frame; replaces an open one of that kind.
    pub fn begin(&mut self, kind: Span, page: Option<u32>, now: Instant) {
        let start_us = self.us(now);
        let mut replaced = Vec::new();
        self.targets.retain(|target| {
            if target.record.kind == kind {
                let mut record = target.record.clone();
                record.end_us = Some(start_us);
                record.completed = false;
                replaced.push(record);
                false
            } else {
                true
            }
        });
        self.spans.extend(replaced);
        self.targets.push(Target {
            record: SpanRecord {
                kind,
                start_us,
                end_us: None,
                first_frame: None,
                last_frame: None,
                page,
                completed: false,
            },
            satisfied: false,
        });
    }

    /// PDF pages laid out in the current frame: the visible range and the pages that have a
    /// bitmap to paint.
    pub fn pdf_painted(&mut self, visible: Range<u32>, painted: &[u32]) {
        if self.frame_open.is_none() {
            return;
        }
        for target in &mut self.targets {
            let shown = |page: &u32| visible.contains(page) && painted.contains(page);
            target.satisfied |= match (target.record.kind, target.record.page) {
                (Span::PdfFirstPage, _) => painted.iter().any(shown),
                (Span::PageTurn, Some(page)) => shown(&page),
                _ => false,
            };
        }
    }

    pub fn drain(&mut self, now: Instant) -> Delta {
        if self.frame_open.is_none() {
            self.close_idle_motions(now);
        }
        let open = self
            .targets
            .iter()
            .map(|t| t.record.clone())
            .chain(self.motions.iter().filter_map(|m| m.open.clone()))
            .collect();
        Delta {
            frames: std::mem::take(&mut self.frames),
            spans: std::mem::take(&mut self.spans),
            open,
            frame_count: self.frame_count,
        }
    }
}

/// Accumulates drained deltas (bounded) and renders the output document.
#[derive(Default)]
pub struct Log {
    frames: Vec<(u64, u32)>,
    spans: Vec<SpanRecord>,
    open: Vec<SpanRecord>,
    frame_count: u64,
    dropped_frames: u64,
    dropped_spans: u64,
    /// Set when absorbed changes have not reached the file yet; cleared only by a
    /// successful write, so a failed write is retried on the next flush even when the GUI
    /// is idle and no new samples arrive.
    unwritten: bool,
}

impl Log {
    /// Returns whether anything changed.
    pub fn absorb(&mut self, delta: Delta) -> bool {
        let changed =
            !delta.frames.is_empty() || !delta.spans.is_empty() || delta.open != self.open;
        for frame in delta.frames {
            if self.frames.len() < MAX_FRAMES {
                self.frames.push(frame);
            } else {
                self.dropped_frames += 1;
            }
        }
        for span in delta.spans {
            if self.spans.len() < MAX_SPANS {
                self.spans.push(span);
            } else {
                self.dropped_spans += 1;
            }
        }
        self.open = delta.open;
        self.frame_count = delta.frame_count;
        self.unwritten |= changed;
        changed
    }

    /// Whether the file is behind the log (new changes, or an earlier write failed).
    pub fn needs_write(&self) -> bool {
        self.unwritten
    }

    pub fn mark_written(&mut self) {
        self.unwritten = false;
    }

    pub fn to_json(&self, origin_unix_us: u64) -> Value {
        json!({
            "schema": SCHEMA,
            "origin_unix_us": origin_unix_us,
            "frame_definition": "root render start to end-marker paint (render+layout+prepaint+paint; present excluded)",
            "frames_format": ["start_us", "duration_us"],
            "frames": self.frames.iter().map(|(s, d)| json!([s, d])).collect::<Vec<_>>(),
            "frame_count": self.frame_count,
            "dropped_frames": self.dropped_frames,
            "spans": self.spans.iter().map(SpanRecord::to_json).collect::<Vec<_>>(),
            "dropped_spans": self.dropped_spans,
            "open_spans": self.open.iter().map(SpanRecord::to_json).collect::<Vec<_>>(),
            "motion_idle_ms": MOTION_IDLE.as_millis() as u64,
        })
    }
}

/// Parses the variable's value: `None` (disabled) when unset or empty, an error when it is
/// not an absolute path (a relative path would depend on the launcher's working directory).
pub fn output_path(value: Option<&std::ffi::OsStr>) -> Result<Option<PathBuf>, String> {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(format!(
            "{ENV} must be an absolute file path; recording disabled"
        ));
    }
    Ok(Some(path))
}

/// Writes `value` to `path` via a sibling temporary file and rename.
pub fn write_atomically(path: &Path, value: &Value) -> std::io::Result<()> {
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    std::fs::write(&temp, serde_json::to_vec(value)?)?;
    std::fs::rename(&temp, path)
}

static RECORDER: OnceLock<Option<Mutex<Recorder>>> = OnceLock::new();
static OUTPUT: OnceLock<(PathBuf, Mutex<Log>)> = OnceLock::new();

#[inline]
fn recorder() -> Option<&'static Mutex<Recorder>> {
    RECORDER.get().and_then(Option::as_ref)
}

#[inline]
fn with(f: impl FnOnce(&mut Recorder)) {
    if let Some(recorder) = recorder()
        && let Ok(mut recorder) = recorder.lock()
    {
        f(&mut recorder);
    }
}

/// Reads [`ENV`] once. Only when it names an absolute path does recording start (and the
/// writer thread with it); otherwise every hook stays a no-op for the process lifetime.
pub fn init_from_env() {
    RECORDER.get_or_init(|| {
        let path = match output_path(std::env::var_os(ENV).as_deref()) {
            Ok(Some(path)) => path,
            Ok(None) => return None,
            Err(message) => {
                eprintln!("{message}");
                return None;
            }
        };
        let origin = Instant::now();
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX));
        if OUTPUT.set((path, Mutex::new(Log::default()))).is_err() {
            return None;
        }
        let spawned = std::thread::Builder::new()
            .name("frame-timing".into())
            .spawn(|| {
                loop {
                    std::thread::sleep(FLUSH_EVERY);
                    flush();
                }
            });
        if spawned.is_err() {
            eprintln!("{ENV}: could not start the writer thread; recording disabled");
            return None;
        }
        eprintln!("{ENV}: recording frame timings");
        Some(Mutex::new(Recorder::new(origin, unix)))
    });
}

#[inline]
pub fn enabled() -> bool {
    recorder().is_some()
}

#[inline]
pub fn frame_start() {
    with(|r| r.frame_start(Instant::now()));
}

#[inline]
pub fn frame_end() {
    with(|r| r.frame_end(Instant::now()));
}

#[inline]
pub fn motion(kind: Span, value: f32) {
    with(|r| r.motion(kind, value));
}

#[inline]
pub fn begin(kind: Span, page: Option<u32>) {
    with(|r| r.begin(kind, page, Instant::now()));
}

#[inline]
pub fn pdf_painted(visible: Range<u32>, painted: &[u32]) {
    with(|r| r.pdf_painted(visible, painted));
}

/// Moves new entries out of the recorder (short lock) and rewrites the file if anything
/// changed. Called by the writer thread and on quit.
pub fn flush() {
    let (Some(recorder), Some((path, log))) = (recorder(), OUTPUT.get()) else {
        return;
    };
    let Ok(mut log) = log.lock() else { return };
    let (delta, origin) = match recorder.lock() {
        Ok(mut r) => (r.drain(Instant::now()), r.origin_unix_us()),
        Err(_) => return,
    };
    log.absorb(delta);
    if !log.needs_write() {
        return;
    }
    match write_atomically(path, &log.to_json(origin)) {
        Ok(()) => log.mark_written(),
        // Kept unwritten: the next flush (writer thread or quit) retries.
        Err(error) => eprintln!("{ENV}: write failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(origin: Instant, ms: u64) -> Instant {
        origin + Duration::from_millis(ms)
    }

    #[test]
    fn disabled_unless_an_absolute_path_is_given() {
        assert_eq!(output_path(None), Ok(None));
        assert_eq!(output_path(Some("".as_ref())), Ok(None));
        assert!(output_path(Some("relative.json".as_ref())).is_err());
        assert_eq!(
            output_path(Some("/x/frames.json".as_ref())),
            Ok(Some(PathBuf::from("/x/frames.json")))
        );
        // Never initialised in unit tests: hooks are no-ops and nothing is written.
        frame_start();
        frame_end();
        assert!(!enabled());
    }

    #[test]
    fn frames_and_page_turn_spans() {
        let o = Instant::now();
        let mut r = Recorder::new(o, 1_000);
        r.frame_start(at(o, 0));
        r.frame_end(at(o, 5));
        r.begin(Span::PageTurn, Some(3), at(o, 10));
        r.frame_start(at(o, 12));
        r.pdf_painted(2..4, &[2]);
        r.frame_end(at(o, 20));
        r.frame_start(at(o, 40));
        r.pdf_painted(3..5, &[3, 4]);
        r.frame_end(at(o, 46));
        let d = r.drain(at(o, 50));
        assert_eq!(d.frames, vec![(0, 5_000), (12_000, 8_000), (40_000, 6_000)]);
        assert_eq!(d.spans.len(), 1);
        let s = &d.spans[0];
        assert_eq!(
            (s.kind, s.page, s.completed),
            (Span::PageTurn, Some(3), true)
        );
        assert_eq!((s.start_us, s.end_us), (10_000, Some(46_000)));
        assert_eq!((s.first_frame, s.last_frame), (Some(1), Some(2)));
        assert!(d.open.is_empty());
    }

    #[test]
    fn newer_span_replaces_unfinished_one_and_first_page_needs_visibility() {
        let o = Instant::now();
        let mut r = Recorder::new(o, 0);
        r.begin(Span::PdfFirstPage, None, at(o, 0));
        r.frame_start(at(o, 1));
        r.pdf_painted(0..2, &[5]); // painted but not visible
        r.frame_end(at(o, 2));
        r.begin(Span::PdfFirstPage, None, at(o, 3));
        r.frame_start(at(o, 4));
        r.pdf_painted(0..2, &[1]);
        r.frame_end(at(o, 9));
        let d = r.drain(at(o, 10));
        let done: Vec<_> = d.spans.iter().map(|s| (s.completed, s.end_us)).collect();
        assert_eq!(done, vec![(false, Some(3_000)), (true, Some(9_000))]);
    }

    #[test]
    fn motion_span_closes_after_idle_and_counts_frames() {
        let o = Instant::now();
        let mut r = Recorder::new(o, 0);
        for (i, offset) in [0.0, -32.0, -64.0, -64.0].into_iter().enumerate() {
            let t = 20 * i as u64;
            r.frame_start(at(o, t));
            r.motion(Span::ListScroll, offset);
            r.frame_end(at(o, t + 4));
        }
        // Still within the idle window: reported as open.
        let d = r.drain(at(o, 100));
        assert!(d.spans.is_empty());
        assert_eq!(d.open.len(), 1);
        let d = r.drain(at(o, 1_000));
        assert_eq!(d.spans.len(), 1);
        let s = &d.spans[0];
        assert_eq!(s.kind, Span::ListScroll);
        assert_eq!((s.start_us, s.end_us), (20_000, Some(44_000)));
        assert_eq!((s.first_frame, s.last_frame), (Some(1), Some(2)));
    }

    #[test]
    fn log_is_bounded_and_serialises() {
        let mut log = Log::default();
        let delta = Delta {
            frames: vec![(1, 2); 3],
            spans: vec![],
            open: vec![],
            frame_count: 3,
        };
        assert!(log.absorb(delta));
        let value = log.to_json(42);
        assert_eq!(value["schema"], SCHEMA);
        assert_eq!(value["frames"][0], json!([1, 2]));
        assert_eq!(value["frame_count"], 3);
        assert!(log.needs_write());
        // A failed write leaves the log unwritten, so an idle flush (no new samples)
        // still retries; only a successful write clears it.
        assert!(!log.absorb(Delta {
            frame_count: 3,
            ..Delta::default()
        }));
        assert!(log.needs_write(), "retried after a failed write");
        log.mark_written();
        assert!(!log.absorb(Delta {
            frame_count: 3,
            ..Delta::default()
        }));
        assert!(!log.needs_write(), "nothing new after a successful write");
    }
}
