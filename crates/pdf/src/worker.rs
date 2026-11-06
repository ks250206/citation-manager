//! A single dedicated serial thread; PDFium values never cross the channel.
//! Cancellation is cooperative between native calls, not native-parser preemption.
use crate::geometry::{Quad, Rect, Rotation};
use cm_domain::PageIndex;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const PDFIUM_BUILD: &str = "158.0.8125.0";
pub const PDFIUM_ABI: &str = "pdfium-render=0.9.5;pdfium_8125;thread_safe";
pub const PDFIUM_SHA256: &str = "8d1e46a6dbe32d7cb95279ab3def2e71e557a3d4cc49162453afa3e3b250dc5e";
pub const MAX_INPUT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_PIXELS: usize = 4 * 1024 * 1024;
pub const MAX_CHARS: usize = 100_000;
pub const QUEUE_CAPACITY: usize = 8;
// pdfium-render 0.9.5 retains process-global bindings in OnceLock, even after Pdfium drops.
static STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PdfError {
    Io,
    LibraryPath,
    LibraryChecksum,
    LibraryLoad,
    AlreadyStarted,
    QueueFull,
    WorkerClosed,
    Cancelled,
    Deadline,
    InvalidPage,
    InvalidGeometry,
    InvalidText,
    InvalidSource,
    LimitExceeded,
    Malformed,
    PasswordRequiredOrIncorrect,
    UnsupportedSecurity,
    NativeFailure,
}
impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for PdfError {}

/// Trusted adapter input, not an API authorization boundary. Never log passwords or PDF text.
#[derive(Clone)]
pub struct Source {
    pub path: PathBuf,
    pub password: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Character {
    pub index: usize,
    pub unicode: Option<char>,
    pub bounds: Rect,
    /// Axis-aligned loose glyph bounds in unrotated PDF space, not an exact ink outline.
    pub quad: Quad,
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum TextStatus {
    Extracted,
    NoExtractableText,
}
#[derive(Debug, Clone, Serialize)]
pub struct AnnotationSnapshot {
    pub kind: String,
    pub contents: Option<String>,
    pub bounds: Rect,
    pub quads: Vec<Quad>,
}
#[derive(Debug, Serialize)]
pub struct PageSnapshot {
    pub page_index: PageIndex,
    pub physical_page: u32,
    pub page_label: Option<String>,
    pub page_count: i32,
    pub crop: Rect,
    pub rotation: Rotation,
    pub text: String,
    pub text_status: TextStatus,
    pub characters: Vec<Character>,
    pub annotations: Vec<AnnotationSnapshot>,
    #[serde(skip)]
    pub bitmap: Option<Bitmap>,
}
#[derive(Debug)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Highlight {
    pub page_index: PageIndex,
    pub quads: Vec<Quad>,
    pub comment: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct Note {
    pub page_index: PageIndex,
    pub bounds: Rect,
    pub contents: String,
}
#[derive(Debug, Clone)]
pub struct MetadataSnapshot {
    pub sha256: [u8; 32],
    pub draft: cm_core::import::PaperDraft,
    pub page_count: i32,
    pub warnings: Vec<String>,
}
pub enum Operation {
    Metadata {
        source: Source,
    },
    Page {
        source: Source,
        page_index: PageIndex,
        zoom: Option<f32>,
    },
    /// Returns a separate PDF in memory; the caller must write with create_new/atomic storage.
    Export {
        source: Source,
        highlights: Vec<Highlight>,
        notes: Vec<Note>,
    },
}
/// Native output is an intermediate: PDFium 8125 does not generate persistent /AP here.
/// The PoC's explicit appearance completion script is required for portable export.
pub struct NativeExport {
    pub bytes: Vec<u8>,
    pub appearance: AppearanceStatus,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppearanceStatus {
    RequiresExportCompletion,
}
pub enum Output {
    Metadata(Box<MetadataSnapshot>),
    Page(Box<PageSnapshot>),
    Export(NativeExport),
}
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}
pub(crate) struct Context {
    pub cancel: Cancellation,
    pub deadline: Instant,
}
impl Context {
    pub fn check(&self) -> Result<(), PdfError> {
        if self.cancel.0.load(Ordering::Acquire) {
            Err(PdfError::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(PdfError::Deadline)
        } else {
            Ok(())
        }
    }
}
struct Request {
    op: Operation,
    context: Context,
    reply: SyncSender<Result<Output, PdfError>>,
}
pub struct Ticket {
    reply: Receiver<Result<Output, PdfError>>,
    cancel: Cancellation,
}
impl Ticket {
    pub fn wait(self, timeout: Duration) -> Result<Output, PdfError> {
        match self.reply.recv_timeout(timeout) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.cancel.cancel();
                Err(PdfError::Deadline)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(PdfError::WorkerClosed),
        }
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
pub struct PdfWorker {
    sender: Option<SyncSender<Request>>,
    thread: Option<JoinHandle<()>>,
}
impl PdfWorker {
    /// Absolute developer override or bundle-resolved path only; verifies pinned original bytes.
    /// One start per process, because the binding's global library cannot be replaced/unloaded.
    pub fn start(library: PathBuf) -> Result<Self, PdfError> {
        let (sender, receiver) = mpsc::sync_channel::<Request>(QUEUE_CAPACITY);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("cm-pdf-serial".into())
            .spawn(move || {
                let pdfium = match crate::adapter::load(&library) {
                    Ok(pdfium) => pdfium,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                while let Ok(request) = receiver.recv() {
                    let result = request.context.check().and_then(|()| {
                        crate::adapter::execute(&pdfium, request.op, &request.context)
                    });
                    let _ = request.reply.send(result);
                }
                // All pages, text pages, bitmaps, annotations and documents were dropped per operation.
            })
            .map_err(|_| PdfError::WorkerClosed)?;
        match ready_rx.recv().map_err(|_| PdfError::WorkerClosed)? {
            Ok(()) => Ok(Self {
                sender: Some(sender),
                thread: Some(thread),
            }),
            Err(e) => {
                let _ = thread.join();
                Err(e)
            }
        }
    }
    pub fn submit(
        &self,
        op: Operation,
        cancel: Cancellation,
        budget: Duration,
    ) -> Result<Ticket, PdfError> {
        validate_operation(&op)?;
        if budget > Duration::from_secs(60) {
            return Err(PdfError::LimitExceeded);
        }
        let (reply_tx, reply) = mpsc::sync_channel(1);
        let request = Request {
            op,
            context: Context {
                cancel: cancel.clone(),
                deadline: Instant::now() + budget,
            },
            reply: reply_tx,
        };
        match self
            .sender
            .as_ref()
            .ok_or(PdfError::WorkerClosed)?
            .try_send(request)
        {
            Ok(()) => Ok(Ticket { reply, cancel }),
            Err(TrySendError::Full(_)) => Err(PdfError::QueueFull),
            Err(TrySendError::Disconnected(_)) => Err(PdfError::WorkerClosed),
        }
    }
    /// Drain queued requests and join. Cancel outstanding tickets first for prompt cooperative exit.
    /// A native call can still delay shutdown; process isolation is a later hard-timeout boundary.
    pub fn shutdown(mut self) -> Result<(), PdfError> {
        self.join()
    }
    fn join(&mut self) -> Result<(), PdfError> {
        self.sender.take();
        self.thread
            .take()
            .map_or(Ok(()), |t| t.join().map_err(|_| PdfError::WorkerClosed))
    }
}
impl Drop for PdfWorker {
    fn drop(&mut self) {
        let _ = self.join();
    }
}
pub(crate) fn process_started() -> bool {
    STARTED.load(Ordering::Acquire)
}
pub(crate) fn claim_process() -> Result<(), PdfError> {
    STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| PdfError::AlreadyStarted)
}
fn validate_operation(op: &Operation) -> Result<(), PdfError> {
    let source = match op {
        Operation::Page { source, .. }
        | Operation::Export { source, .. }
        | Operation::Metadata { source } => source,
    };
    if !source.path.is_absolute()
        || source
            .password
            .as_ref()
            .is_some_and(|p| p.len() > 1024 || p.contains('\0'))
    {
        return Err(PdfError::InvalidSource);
    }
    if let Operation::Export {
        highlights, notes, ..
    } = op
    {
        if highlights.iter().any(|h| h.comment.contains('\0'))
            || notes.iter().any(|n| n.contents.contains('\0'))
        {
            return Err(PdfError::InvalidText);
        }
        let count = highlights.len().saturating_add(notes.len());
        let quads = highlights
            .iter()
            .fold(0usize, |n, h| n.saturating_add(h.quads.len()));
        let text = highlights
            .iter()
            .map(|h| h.comment.len())
            .chain(notes.iter().map(|n| n.contents.len()))
            .fold(0usize, usize::saturating_add);
        if count > 1024
            || quads > 4096
            || text > 1024 * 1024
            || highlights
                .iter()
                .any(|h| h.quads.is_empty() || h.quads.len() > 1024 || h.comment.len() > 65536)
            || notes.iter().any(|n| n.contents.len() > 65536)
        {
            return Err(PdfError::LimitExceeded);
        }
    }
    Ok(())
}
/// #34: pass current_exe(), no CWD/PATH/system-library fallback or implicit env override.
pub fn bundled_library(executable: &Path) -> Result<PathBuf, PdfError> {
    if !executable.is_absolute()
        || executable
            .parent()
            .and_then(Path::file_name)
            .is_none_or(|p| p != "MacOS")
    {
        return Err(PdfError::LibraryPath);
    }
    let contents = executable
        .parent()
        .and_then(Path::parent)
        .ok_or(PdfError::LibraryPath)?;
    if contents.file_name().is_none_or(|p| p != "Contents") {
        return Err(PdfError::LibraryPath);
    }
    Ok(contents.join("Frameworks/libpdfium.dylib"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn op() -> Operation {
        Operation::Page {
            source: Source {
                path: PathBuf::from("/synthetic.pdf"),
                password: None,
            },
            page_index: PageIndex::try_from(0).unwrap(),
            zoom: None,
        }
    }
    #[test]
    fn queue_admission_bounds_and_closed_worker() {
        let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        let worker = PdfWorker {
            sender: Some(tx),
            thread: None,
        };
        let mut tickets = Vec::new();
        for _ in 0..QUEUE_CAPACITY {
            tickets.push(
                worker
                    .submit(op(), Cancellation::default(), Duration::from_secs(1))
                    .unwrap(),
            );
        }
        assert!(matches!(
            worker.submit(op(), Cancellation::default(), Duration::from_secs(1)),
            Err(PdfError::QueueFull)
        ));
        drop(rx);
        assert!(matches!(
            worker.submit(op(), Cancellation::default(), Duration::from_secs(1)),
            Err(PdfError::WorkerClosed)
        ));
        for t in tickets {
            assert!(matches!(
                t.wait(Duration::ZERO),
                Err(PdfError::WorkerClosed)
            ));
        }
    }
    #[test]
    fn resources_use_absolute_bundle_location() {
        assert_eq!(
            bundled_library(Path::new("/tmp/移動した App.app/Contents/MacOS/papersd")).unwrap(),
            PathBuf::from("/tmp/移動した App.app/Contents/Frameworks/libpdfium.dylib")
        );
        assert_eq!(
            bundled_library(Path::new("target/papersd")),
            Err(PdfError::LibraryPath)
        );
        assert_eq!(
            bundled_library(Path::new("/usr/bin/papersd")),
            Err(PdfError::LibraryPath)
        );
    }
    #[test]
    fn annotations_reject_nul_without_truncation() {
        let source = Source {
            path: PathBuf::from("/synthetic.pdf"),
            password: None,
        };
        let note = Note {
            page_index: PageIndex::try_from(0).unwrap(),
            bounds: Rect {
                left: 1.,
                bottom: 1.,
                right: 3.,
                top: 3.,
            },
            contents: "before\0after".into(),
        };
        assert_eq!(
            validate_operation(&Operation::Export {
                source,
                highlights: vec![],
                notes: vec![note]
            }),
            Err(PdfError::InvalidText)
        );
    }
    #[test]
    fn ticket_drop_cancels_and_wait_is_bounded() {
        let (tx, rx) = mpsc::sync_channel(1);
        let cancel = Cancellation::default();
        let ticket = Ticket {
            reply: rx,
            cancel: cancel.clone(),
        };
        assert!(matches!(
            ticket.wait(Duration::ZERO),
            Err(PdfError::Deadline)
        ));
        assert!(cancel.0.load(Ordering::Acquire));
        drop(tx);
    }
}
