//! #20 annotated PDF export. The library thread validates the request and plans the
//! annotations; PDFium work, temp-file write, re-read verification and the final move run on
//! a helper thread (like page renders), so a large PDF never blocks library reads/writes.
use super::{Worker, files};
use cm_core::{
    annotations::AnnotationService,
    pdf_export::{ExportPlan, plan},
    ports::AttachmentRepository,
};
use cm_protocol::attachments::{PdfExportFailure, PdfExportReport, PdfExportRequest};
use std::path::{Path, PathBuf};

type Failure = PdfExportFailure;

fn destination_error(reason: impl Into<String>) -> Failure {
    Failure::Destination(reason.into())
}

/// Filesystem identity (device, inode) of a directory. The same directory has the same
/// identity under every spelling: other letter case on a case-insensitive volume, a bind
/// mount, `..`-free aliases. Lexical prefix checks miss those.
pub(crate) type DirIdentity = (u64, u64);

#[cfg(unix)]
pub(crate) fn identity(path: &Path) -> std::io::Result<DirIdentity> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path)?;
    Ok((m.dev(), m.ino()))
}

/// True when `dir` or any of its ancestors *is* one of the `protected` directories.
#[cfg(unix)]
pub(crate) fn within_any(dir: &Path, protected: &[DirIdentity]) -> std::io::Result<bool> {
    for ancestor in dir.ancestors().filter(|a| !a.as_os_str().is_empty()) {
        if protected.contains(&identity(ancestor)?) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The library directory and its managed attachment store: never export destinations.
#[cfg(unix)]
fn protected_roots(library: &Path) -> Result<Vec<DirIdentity>, Failure> {
    let mut roots = vec![
        identity(library)
            .map_err(|e| destination_error(format!("Cannot inspect the library folder: {e}")))?,
    ];
    if let Ok(store) = identity(&library.join("attachments")) {
        roots.push(store);
    }
    Ok(roots)
}

#[cfg(unix)]
fn inside_library(parent: &Path, library: &Path) -> Result<bool, Failure> {
    within_any(parent, &protected_roots(library)?)
        .map_err(|e| destination_error(format!("Cannot inspect destination folder: {e}")))
}
#[cfg(not(unix))]
fn inside_library(parent: &Path, library: &Path) -> Result<bool, Failure> {
    let library = library
        .canonicalize()
        .map_err(|e| destination_error(format!("Cannot inspect the library folder: {e}")))?;
    Ok(parent.starts_with(library))
}

/// Only PDFs are ever replaced (case-insensitive `.pdf`), never configs or other files.
fn replaceable_name(name: &str) -> bool {
    name.len() > 4 && name.to_ascii_lowercase().ends_with(".pdf")
}

/// Lexical and filesystem checks of the chosen destination. The parent must be an existing
/// directory reached without symlinks; the library directory (and so every stored original)
/// is never a destination, compared by filesystem identity; an existing file is only replaced
/// when the request carries `replace_existing` — authorized separately as
/// `Operation::PdfExportReplace`, which only the desktop preset holds — and is a `.pdf`.
pub(super) fn check_destination(
    destination: &Path,
    library: &Path,
    replace_existing: bool,
) -> Result<(), Failure> {
    let name = destination
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| destination_error("Choose a file name for the exported PDF"))?;
    if name.starts_with('.') && name.ends_with(".partial") {
        return Err(destination_error("Choose a different file name"));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| destination_error("Choose a destination folder"))?;
    files::safe(parent, false).map_err(|e| destination_error(e.to_string()))?;
    if inside_library(parent, library)? {
        return Err(destination_error(
            "Choose a folder outside the library; stored PDFs are never overwritten",
        ));
    }
    match std::fs::symlink_metadata(destination) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(destination_error(format!(
            "Cannot inspect destination: {e}"
        ))),
        Ok(_) if !replace_existing => Err(destination_error(
            "A file with this name already exists; choose a new name",
        )),
        Ok(_) if !replaceable_name(name) => Err(destination_error(
            "Only an existing .pdf file can be replaced; choose a new name",
        )),
        Ok(_) => files::safe(destination, true).map_err(|_| {
            destination_error("Only an ordinary file (not a link or folder) can be replaced")
        }),
    }
}

/// Everything the helper thread needs; no database handle.
#[cfg_attr(not(feature = "pdf-metadata"), allow(dead_code))]
pub(in crate::desktop_worker) struct PendingExport {
    pub(super) source: PathBuf,
    pub(super) sha256: [u8; 32],
    pub(super) plan: ExportPlan,
    pub(super) request: PdfExportRequest,
    /// Library directory, to repeat the destination checks right before writing.
    pub(super) library: PathBuf,
}

impl Worker<'_> {
    /// Library thread: validate, resolve the hash-verified original and plan the annotations.
    pub(in crate::desktop_worker) fn begin_pdf_export(
        &mut self,
        request: PdfExportRequest,
    ) -> Result<PendingExport, Failure> {
        if request.cancel.is_cancelled() {
            return Err(Failure::Cancelled);
        }
        check_destination(
            &request.destination,
            self.directory.path(),
            request.replace_existing,
        )?;
        let attachment = self
            .db
            .attachments()
            .get(request.attachment)
            .map_err(|e| Failure::Other(e.to_string()))?
            .ok_or(Failure::MissingOrCorrupt)?;
        let source = cm_storage::attachments::ManagedStore::new(&self.directory)
            .and_then(|store| store.verified_path(&attachment))
            .map_err(|_| Failure::MissingOrCorrupt)?;
        let annotations = AnnotationService::new(self.db.annotations())
            .list(request.attachment)
            .map_err(|e| Failure::Other(e.to_string()))?;
        let plan = plan(&annotations);
        if !plan.omitted.is_empty() && !request.allow_omissions {
            return Err(Failure::Unexportable(plan.omitted));
        }
        self.pdf_export_runtime()?;
        Ok(PendingExport {
            source,
            sha256: attachment.sha256,
            plan,
            request,
            library: self.directory.path().to_path_buf(),
        })
    }
    #[cfg(feature = "pdf-metadata")]
    fn pdf_export_runtime(&mut self) -> Result<(), Failure> {
        self.pdf_worker()
            .map(|_| ())
            .map_err(|e| Failure::RuntimeUnavailable(format!("PDF runtime unavailable ({e})")))
    }
    #[cfg(not(feature = "pdf-metadata"))]
    fn pdf_export_runtime(&mut self) -> Result<(), Failure> {
        Err(Failure::RuntimeUnavailable(
            "This build has no PDF runtime".into(),
        ))
    }
}

impl PendingExport {
    /// Helper thread. Nothing is visible at the destination unless every step succeeded.
    #[cfg(feature = "pdf-metadata")]
    pub(in crate::desktop_worker) fn finish(self) -> Result<Box<PdfExportReport>, Failure> {
        runtime::finish(self)
    }
    #[cfg(not(feature = "pdf-metadata"))]
    pub(in crate::desktop_worker) fn finish(self) -> Result<Box<PdfExportReport>, Failure> {
        Err(Failure::RuntimeUnavailable(
            "This build has no PDF runtime".into(),
        ))
    }
}

#[cfg(feature = "pdf-metadata")]
mod runtime {
    use super::*;
    use cm_core::pdf_export::{pdf_name, verify};
    use cm_pdf::worker::{
        AnnotationsSnapshot, Cancellation, Highlight, Note, Operation, Output, PdfError, Source,
    };
    use cm_protocol::domain::PageIndex;
    use std::{
        io::Write,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    /// The client's cancellation, bridged to whichever PDF operation is running. Each
    /// operation gets a fresh worker token: a finished ticket cancels its own token on drop.
    struct Bridge {
        request: cm_protocol::attachments::Cancellation,
        current: Mutex<Cancellation>,
    }
    impl Bridge {
        fn token(&self) -> Result<Cancellation, Failure> {
            let token = Cancellation::default();
            *self.current.lock().unwrap_or_else(|e| e.into_inner()) = token.clone();
            // Checked after installing, so a cancel racing the swap is never lost.
            if self.request.is_cancelled() {
                return Err(Failure::Cancelled);
            }
            Ok(token)
        }
        fn cancel_current(&self) {
            self.current
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cancel();
        }
    }

    fn hex(bytes: &[u8; 32]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn file_sha256(path: &Path) -> std::io::Result<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let mut file = std::fs::File::open(path)?;
        let mut hash = Sha256::new();
        std::io::copy(&mut file, &mut hash)?;
        Ok(hash.finalize().into())
    }

    fn new_file_options() -> tempfile::Builder<'static, 'static> {
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // The process umask applies at creation: the result is 0666 & !umask.
            builder.permissions(std::fs::Permissions::from_mode(0o666));
        }
        builder
    }

    /// A replaced PDF keeps its permission bits (not owner/ACLs).
    fn keep_mode(file: &std::fs::File, existing: &Path) -> Result<(), Failure> {
        let permissions = std::fs::metadata(existing)
            .map_err(|e| destination_error(format!("Cannot inspect destination: {e}")))?
            .permissions();
        file.set_permissions(permissions)
            .map_err(|e| destination_error(format!("Cannot set file permissions: {e}")))
    }

    /// Per PDFium operation; the worker caps budgets at 60 s.
    const BUDGET: Duration = Duration::from_secs(60);

    fn failure(e: PdfError) -> Failure {
        use PdfError::*;
        match e {
            PasswordRequiredOrIncorrect | UnsupportedSecurity => Failure::Encrypted,
            Malformed | InvalidSource => Failure::Damaged,
            InvalidPage | InvalidGeometry | InvalidText => Failure::InvalidAnnotation,
            Cancelled => Failure::Cancelled,
            QueueFull | Deadline => Failure::Busy,
            LibraryPath | LibraryChecksum | LibraryLoad | AlreadyStarted | WorkerClosed => {
                Failure::RuntimeUnavailable(format!("PDF runtime unavailable ({e})"))
            }
            LimitExceeded => {
                Failure::Other("The PDF or its annotations exceed export limits".into())
            }
            Io => Failure::MissingOrCorrupt,
            NativeFailure => Failure::Other("PDF export failed in the PDF engine".into()),
        }
    }

    fn run(op: Operation, cancel: &Bridge) -> Result<Output, Failure> {
        let worker = crate::fulltext::pdf_runtime::shared(None)
            .map_err(|e| Failure::RuntimeUnavailable(format!("PDF runtime unavailable ({e})")))?;
        worker
            .submit(op, cancel.token()?, BUDGET)
            .map_err(failure)?
            .wait(BUDGET + Duration::from_secs(1))
            .map_err(failure)
    }

    fn annotations(path: &Path, cancel: &Bridge) -> Result<AnnotationsSnapshot, Failure> {
        match run(
            Operation::Annotations {
                source: Source {
                    path: path.to_path_buf(),
                    password: None,
                },
            },
            cancel,
        )? {
            Output::Annotations(snapshot) => Ok(*snapshot),
            _ => Err(Failure::Other("Unexpected PDF worker reply".into())),
        }
    }

    fn page(index: u32) -> Result<PageIndex, Failure> {
        PageIndex::try_from(index).map_err(|_| Failure::InvalidAnnotation)
    }

    pub(super) fn finish(pending: PendingExport) -> Result<Box<PdfExportReport>, Failure> {
        // Bridge the client's cancellation (IPC Cancel frame) to the PDF worker.
        let bridge = Arc::new(Bridge {
            request: pending.request.cancel.clone(),
            current: Mutex::new(Cancellation::default()),
        });
        let done = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (done, bridge) = (done.clone(), bridge.clone());
            std::thread::Builder::new()
                .name("pdf-export-cancel".into())
                .spawn(move || {
                    while !done.load(Ordering::Relaxed) {
                        if bridge.request.is_cancelled() {
                            bridge.cancel_current();
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                })
                .ok()
        };
        let result = export(&pending, &bridge);
        done.store(true, Ordering::Relaxed);
        if let Some(watcher) = watcher {
            let _ = watcher.join();
        }
        result.map(Box::new)
    }

    fn export(pending: &PendingExport, cancel: &Bridge) -> Result<PdfExportReport, Failure> {
        let request = &pending.request;
        let cancelled = || {
            if request.cancel.is_cancelled() {
                Err(Failure::Cancelled)
            } else {
                Ok(())
            }
        };
        let before = annotations(&pending.source, cancel)?;
        if before.sha256 != pending.sha256 {
            return Err(Failure::OriginalChanged);
        }
        let plan = &pending.plan;
        let highlights = plan
            .highlights
            .iter()
            .map(|h| {
                Ok(Highlight {
                    name: pdf_name(h.id),
                    page_index: page(h.page_index)?,
                    quads: h.quads.clone(),
                    comment: h.contents.clone(),
                    color: h.color,
                    opacity: h.opacity,
                })
            })
            .collect::<Result<Vec<_>, Failure>>()?;
        let notes = plan
            .notes
            .iter()
            .map(|n| {
                Ok(Note {
                    name: pdf_name(n.id),
                    page_index: page(n.page_index)?,
                    bounds: n.at,
                    contents: n.contents.clone(),
                    color: n.color,
                })
            })
            .collect::<Result<Vec<_>, Failure>>()?;
        let Output::Export(export) = run(
            Operation::Export {
                source: Source {
                    path: pending.source.clone(),
                    password: None,
                },
                highlights,
                notes,
            },
            cancel,
        )?
        else {
            return Err(Failure::Other("Unexpected PDF worker reply".into()));
        };
        cancelled()?;
        let parent = request
            .destination
            .parent()
            .ok_or_else(|| destination_error("Choose a destination folder"))?;
        // The folder may have changed since the library thread checked it (seconds of PDF
        // work): repeat every destination check right before creating anything there.
        check_destination(
            &request.destination,
            &pending.library,
            request.replace_existing,
        )?;
        // Same directory as the destination, so the final move is a rename. Dropped (deleted)
        // on every early return below. Created 0666 & !umask, like any new file the person
        // saves; when replacing, the replaced file's mode is kept (below).
        let mut temp = new_file_options()
            .prefix(".citation-manager-export-")
            .suffix(".pdf.partial")
            .tempfile_in(parent)
            .map_err(|e| destination_error(format!("Cannot write in that folder: {e}")))?;
        temp.write_all(&export.bytes)
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|e| destination_error(format!("Writing the copy failed: {e}")))?;
        let after = annotations(temp.path(), cancel)?;
        {
            use sha2::{Digest, Sha256};
            if after.sha256 != <[u8; 32]>::from(Sha256::digest(&export.bytes))
                || after.page_count != before.page_count
            {
                return Err(Failure::VerificationFailed(vec![
                    "The written copy does not match the exported bytes".into(),
                ]));
            }
        }
        let verified = verify(
            plan,
            &export.note_rects,
            &before.annotations,
            &after.annotations,
        )
        .map_err(Failure::VerificationFailed)?;
        // The original is only ever read; prove it is byte-identical after the export.
        if file_sha256(&pending.source).map_err(|_| Failure::OriginalChanged)? != pending.sha256 {
            return Err(Failure::OriginalChanged);
        }
        cancelled()?;
        let replaced = if request.replace_existing {
            // Re-check just before the rename: still absent, or an ordinary file. A rename
            // replaces a link itself and never writes through it.
            let existed = match std::fs::symlink_metadata(&request.destination) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => {
                    return Err(destination_error(format!(
                        "Cannot inspect destination: {e}"
                    )));
                }
                Ok(_) => {
                    let name = request.destination.file_name().and_then(|n| n.to_str());
                    if !name.is_some_and(replaceable_name) {
                        return Err(destination_error(
                            "Only an existing .pdf file can be replaced; choose a new name",
                        ));
                    }
                    files::safe(&request.destination, true).map_err(|_| {
                        destination_error(
                            "Only an ordinary file (not a link or folder) can be replaced",
                        )
                    })?;
                    true
                }
            };
            if existed {
                keep_mode(temp.as_file(), &request.destination)?;
            }
            temp.persist(&request.destination)
                .map_err(|e| destination_error(format!("Saving the copy failed: {}", e.error)))?;
            existed
        } else {
            temp.persist_noclobber(&request.destination).map_err(|e| {
                if e.error.kind() == std::io::ErrorKind::AlreadyExists {
                    destination_error(
                        "A file with this name appeared meanwhile; nothing was replaced",
                    )
                } else {
                    destination_error(format!("Saving the copy failed: {}", e.error))
                }
            })?;
            false
        };
        let _ = std::fs::File::open(parent).and_then(|d| d.sync_all());
        Ok(PdfExportReport {
            path: request.destination.clone(),
            highlights: verified.highlights,
            notes: verified.notes,
            preserved: verified.preserved,
            omitted: plan.omitted.clone(),
            original_sha256: hex(&pending.sha256),
            max_position_error: verified.max_position_error,
            replaced,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn library_containment_compares_directory_identity_not_spelling() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let library = root.join("library");
        std::fs::create_dir_all(library.join("attachments/ab")).unwrap();
        std::fs::create_dir_all(root.join("out/deeper")).unwrap();
        let protected = protected_roots(&library).unwrap();
        assert_eq!(protected.len(), 2);
        for inside in [
            library.clone(),
            library.join("attachments"),
            library.join("attachments/ab"),
        ] {
            assert!(within_any(&inside, &protected).unwrap(), "{inside:?}");
        }
        assert!(!within_any(&root.join("out/deeper"), &protected).unwrap());
        assert!(!within_any(&root, &protected).unwrap());
        // An alias of the library (another spelling / mount of the same directory) has the
        // library's identity, so anything below it is refused although no prefix matches.
        let alias = vec![identity(&library).unwrap()];
        assert!(within_any(&library.join("attachments/ab"), &alias).unwrap());
        assert!(!root.join("out").starts_with(&library));
        // Missing components are an error, not "outside".
        assert!(within_any(&root.join("missing/dir"), &protected).is_err());
    }

    #[test]
    fn only_pdf_files_are_replaceable() {
        for ok in ["a.pdf", "Paper (annotated).PDF", "x.Pdf"] {
            assert!(replaceable_name(ok), "{ok}");
        }
        for refused in [
            ".pdf",
            ".profile",
            "config.toml",
            "a.pdf.bak",
            "pdf",
            "notes.pdfx",
        ] {
            assert!(!replaceable_name(refused), "{refused}");
        }
    }
}
