use super::{
    Worker,
    files::{self, invalid},
};
use cm_core::{
    attachments::{AttachmentService, ImportAttachment},
    import::{file_metadata::*, *},
    ports::AttachmentRepository,
};
use cm_protocol::{attachments::*, library::ServiceError};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};
const MAX_FILES: usize = 1000;
const MAX_BYTES: u64 = 256 * 1024 * 1024;
impl Worker<'_> {
    pub(super) fn pdf(
        &mut self,
        request: PdfRequest,
        progress: PdfProgress,
    ) -> Result<PdfReply, ServiceError> {
        match request {
            PdfRequest::List(id) => {
                let attachments = self.db.attachments().for_paper(id)?;
                let service = AttachmentService::new(
                    self.db
                        .managed_attachments(&self.directory)
                        .map_err(|e| invalid(e.to_string()))?,
                );
                let mut result = Vec::new();
                for a in attachments {
                    let status = service.status(a.id).map_err(|e| invalid(e.to_string()))?;
                    result.push((a, status));
                }
                Ok(PdfReply::Attachments(result))
            }
            PdfRequest::Inspect {
                selection,
                intent,
                cancel,
            } => {
                if let Some((_, id)) = self.pdf_preview.take() {
                    let _ = self.imports.cancel(&id);
                }
                self.pdfs = PdfReport::default();
                let paths = inventory(selection, &cancel)?;
                let mut selected = Vec::new();
                for (path, inventory_error) in paths {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unreadable filename")
                        .to_owned();
                    let mut file = PdfFile {
                        path: path.clone(),
                        name: name.clone(),
                        sha256: None,
                        bytes: 0,
                        draft: None,
                        messages: vec![],
                        disposition: FileDisposition::Ready,
                        paper: None,
                        attachment: None,
                    };
                    let m = fs::symlink_metadata(&path);
                    let regular = m.as_ref().is_ok_and(|m| m.is_file());
                    let unsafe_path = files::safe(&path, true).is_err();
                    file.bytes = m.as_ref().map_or(0, |m| m.len());
                    let input = SelectedFile {
                        in_selected_scope: true,
                        symlink_or_unsafe_ancestor: unsafe_path,
                        regular,
                        filename: name.clone(),
                        byte_size: file.bytes,
                        sha256: None,
                    };
                    selected.push(input);
                    file.disposition = plan_folder(
                        &selected,
                        &FolderPolicy::default(),
                        cancel.is_cancelled().then_some(selected.len() - 1),
                    )
                    .files
                    .last()
                    .expect("added")
                    .disposition
                    .clone();
                    if let Some(error) = inventory_error {
                        file.disposition = FileDisposition::Failed(error);
                    }
                    if file.disposition == FileDisposition::Ready {
                        match digest(&path, &cancel) {
                            Ok((hash, size)) => {
                                file.sha256 = Some(hash);
                                file.bytes = size;
                                selected.last_mut().expect("added").sha256 = Some(hash);
                                selected.last_mut().expect("added").byte_size = size;
                            }
                            Err(e) => {
                                file.disposition = if cancel.is_cancelled() {
                                    FileDisposition::Cancelled
                                } else {
                                    FileDisposition::Failed(e.to_string())
                                };
                            }
                        }
                        if matches!(intent, PdfIntent::RegisterNew)
                            && let Some(partial) = self
                                .partial_pdfs
                                .iter()
                                .find(|p| p.path == file.path && p.sha256 == file.sha256)
                        {
                            file.paper = partial.paper;
                            file.messages = partial.messages.clone();
                            file.draft = partial.draft.clone();
                            file.disposition = FileDisposition::Failed("A previous registration saved this paper; retry its attachment without another paper commit".into());
                        }
                        if matches!(intent, PdfIntent::RegisterNew)
                            && file.disposition == FileDisposition::Ready
                        {
                            let (embedded, messages) =
                                self.metadata(&path, file.sha256.expect("hashed"));
                            file.messages.extend(messages);
                            match propose(FileMetadataCandidate {
                                intent: intent.clone(),
                                filename: name,
                                explicit: None,
                                embedded,
                            }) {
                                Ok(PdfProposal::Register { draft, unresolved }) => {
                                    file.draft = Some(*draft);
                                    file.messages.extend(unresolved);
                                }
                                Ok(_) => (),
                                Err(e) => file.disposition = FileDisposition::Failed(e.to_string()),
                            }
                        }
                    }
                    self.pdfs.assessed +=
                        usize::from(file.disposition != FileDisposition::Cancelled);
                    self.pdfs.cancelled |= file.disposition == FileDisposition::Cancelled;
                    self.pdfs.files.push(file);
                    progress(self.pdfs.clone());
                }
                self.pdfs.cancelled |= cancel.is_cancelled();
                let plan = plan_folder(&selected, &FolderPolicy::default(), None);
                for (file, assessed) in self.pdfs.files.iter_mut().zip(plan.files) {
                    if matches!(file.disposition, FileDisposition::Ready)
                        && assessed.disposition != FileDisposition::Ready
                    {
                        file.disposition = assessed.disposition;
                        file.draft = None;
                    }
                }
                Ok(PdfReply::Report(self.pdfs.clone()))
            }
            PdfRequest::Attach {
                indices,
                paper,
                cancel,
            } => {
                if indices.len() > MAX_FILES {
                    return Err(invalid("Too many selected files"));
                }
                for index in indices {
                    if cancel.is_cancelled() {
                        self.pdfs.cancelled = true;
                        break;
                    }
                    self.attach(index, paper)?;
                    progress(self.pdfs.clone());
                }
                Ok(PdfReply::Report(self.pdfs.clone()))
            }
            PdfRequest::PreviewNew {
                index,
                explicit,
                destination,
            } => {
                if let Some((_, id)) = self.pdf_preview.take() {
                    let _ = self.imports.cancel(&id);
                }
                let file = self.pdfs.files.get(index).ok_or(ServiceError::NotFound)?;
                if file.paper.is_some() || file.disposition != FileDisposition::Ready {
                    return Err(invalid(
                        "Select an inspected ready file; attachment-only retry is required after a paper commit",
                    ));
                }
                let PdfProposal::Register { mut draft, .. } = propose(FileMetadataCandidate {
                    intent: PdfIntent::RegisterNew,
                    filename: file.name.clone(),
                    explicit: explicit.map(|d| *d),
                    embedded: file.draft.clone(),
                })?
                else {
                    return Err(ServiceError::Unavailable);
                };
                draft.source = ImportSource {
                    adapter: "pdf-local".into(),
                    locator: file.path.to_str().map(str::to_owned),
                    row: None,
                    sha256: file.sha256,
                };
                let preview = self.imports.preview(ImportRequest {
                    candidates: vec![*draft],
                    destination,
                })?;
                self.pdf_preview = Some((index, preview.id.clone()));
                Ok(PdfReply::Preview(preview))
            }
            PdfRequest::CommitNew {
                index,
                commit,
                cancel,
            } => {
                if self.pdf_preview.as_ref() != Some(&(index, commit.preview_id.clone())) {
                    return Err(invalid("PDF preview/source mismatch"));
                }
                if cancel.is_cancelled() {
                    return Err(invalid("Cancelled before paper commit"));
                }
                let file = self.pdfs.files.get(index).ok_or(ServiceError::NotFound)?;
                // Recheck selected file before committing bibliography AND again during atomic ingestion.
                let (hash, size) = digest(&file.path, &cancel)?;
                if file.sha256 != Some(hash) || file.bytes != size {
                    return Err(invalid(
                        "Source changed since review; select and preview again",
                    ));
                }
                if self.partial_pdfs.len() >= MAX_FILES {
                    return Err(invalid(
                        "Resolve retained partial attachments before registering more files",
                    ));
                }
                let result = self.imports.commit(commit)?;
                self.pdf_preview = None;
                let paper = result.rows.first().and_then(|r| r.paper);
                if let Some(paper) = paper {
                    let file = &mut self.pdfs.files[index];
                    file.paper = Some(paper);
                    if cancel.is_cancelled() {
                        file.disposition = FileDisposition::Failed("Paper saved; attachment cancelled. Retry attachment against this paper".into());
                        self.pdfs.cancelled = true;
                    } else {
                        self.attach(index, paper)?;
                    }
                } else {
                    self.pdfs.files[index].disposition = FileDisposition::Failed(
                        "Skipped by explicit decision; no paper or attachment saved".into(),
                    );
                }
                self.retain_partial(index);
                Ok(PdfReply::Report(self.pdfs.clone()))
            }
            PdfRequest::RetryAttachment { index, cancel } => {
                if cancel.is_cancelled() {
                    return Err(invalid("Retry cancelled"));
                }
                let paper = self
                    .pdfs
                    .files
                    .get(index)
                    .and_then(|f| f.paper)
                    .ok_or_else(|| invalid("No committed paper; review registration first"))?;
                self.attach(index, paper)?;
                self.retain_partial(index);
                Ok(PdfReply::Report(self.pdfs.clone()))
            }
        }
    }
    fn retain_partial(&mut self, index: usize) {
        let file = self.pdfs.files[index].clone();
        self.partial_pdfs
            .retain(|p| !(p.path == file.path && p.sha256 == file.sha256));
        if file.paper.is_some() && file.attachment.is_none() {
            self.partial_pdfs.push(file);
        }
    }
    #[cfg(feature = "pdf-metadata")]
    fn metadata(&mut self, path: &Path, digest: [u8; 32]) -> (Option<PaperDraft>, Vec<String>) {
        use cm_pdf::worker::*;
        use std::time::Duration;
        if self.pdf.is_none() {
            self.pdf = Some(
                std::env::current_exe()
                    .map_err(|e| e.to_string())
                    .and_then(|exe| bundled_library(&exe).map_err(|e| e.to_string()))
                    .and_then(|path| PdfWorker::start(path).map_err(|e| e.to_string())),
            );
        }
        let result: Result<MetadataSnapshot, String> = (|| {
            let worker = self
                .pdf
                .as_ref()
                .expect("initialized")
                .as_ref()
                .map_err(Clone::clone)?;
            let output = worker
                .submit(
                    Operation::Metadata {
                        source: Source {
                            path: path.into(),
                            password: None,
                        },
                    },
                    Cancellation::default(),
                    Duration::from_secs(10),
                )
                .map_err(|e| e.to_string())?
                .wait(Duration::from_secs(11))
                .map_err(|e| e.to_string())?;
            let Output::Metadata(snapshot) = output else {
                return Err("Unexpected PDF worker reply".into());
            };
            if snapshot.sha256 != digest {
                return Err("Source changed during metadata read".into());
            }
            Ok(*snapshot)
        })();
        match result {
            Ok(snapshot) => (Some(snapshot.draft), snapshot.warnings),
            Err(e) => (
                None,
                vec![format!(
                    "PDF metadata unresolved: {e}. Explicit filename registration remains available; original bytes are preserved"
                )],
            ),
        }
    }
    #[cfg(not(feature = "pdf-metadata"))]
    fn metadata(&mut self, _path: &Path, _digest: [u8; 32]) -> (Option<PaperDraft>, Vec<String>) {
        (None, vec!["PDF metadata unresolved: this build has no PDF runtime. Explicit filename/manual registration remains available".into()])
    }
    fn attach(
        &mut self,
        index: usize,
        paper: cm_protocol::domain::PaperId,
    ) -> Result<(), ServiceError> {
        let file = self
            .pdfs
            .files
            .get(index)
            .ok_or(ServiceError::NotFound)?
            .clone();
        if file.sha256.is_none()
            || !matches!(
                file.disposition,
                FileDisposition::Ready | FileDisposition::Failed(_)
            )
        {
            return Err(invalid("File must be inspected before attachment"));
        }
        self.pdfs.files[index].paper = Some(paper);
        let result = (|| {
            cm_core::papers::PaperService::new(self.db.papers()).get(paper)?;
            files::safe(&file.path, true)?;
            let (hash, size) = digest(&file.path, &Cancellation::default())?;
            if file.sha256 != Some(hash) || file.bytes != size {
                return Err(invalid(
                    "Source/digest changed; reselect original or inspect a new source",
                ));
            }
            // A lost prior attachment response must not append an identical reference.
            for a in self.db.attachments().for_paper(paper)? {
                if a.sha256 == hash {
                    let service = AttachmentService::new(
                        self.db
                            .managed_attachments(&self.directory)
                            .map_err(|e| invalid(e.to_string()))?,
                    );
                    if service.status(a.id).map_err(|e| invalid(e.to_string()))?
                        != AttachmentStatus::Present
                    {
                        return Err(invalid(
                            "Existing shared attachment missing or corrupt; no replacement performed",
                        ));
                    }
                    return Ok(a);
                }
            }
            AttachmentService::new(
                self.db
                    .managed_attachments(&self.directory)
                    .map_err(|e| invalid(e.to_string()))?,
            )
            .import(ImportAttachment {
                paper_id: paper,
                source: file.path,
                original_name: file.name,
                media_type: "application/pdf".into(),
                expected_sha256: file.sha256,
            })
            .map_err(|e| invalid(e.to_string()))
        })();
        let file = &mut self.pdfs.files[index];
        match result {
            Ok(a) => {
                file.attachment = Some(a.id);
                file.disposition = FileDisposition::Imported(AttachmentIdRef(a.id));
            }
            Err(e) => {
                file.disposition = FileDisposition::Failed(format!(
                    "Paper retained; attachment failed: {e}. Retry attachment without creating another paper"
                ));
            }
        }
        Ok(())
    }
}
fn inventory(
    selection: Selection,
    cancel: &Cancellation,
) -> Result<Vec<(PathBuf, Option<String>)>, ServiceError> {
    match selection {
        Selection::Files(paths) => {
            if paths.is_empty() || paths.len() > MAX_FILES {
                return Err(invalid("Select 1..=1000 files"));
            }
            Ok(paths.into_iter().map(|p| (p, None)).collect())
        }
        Selection::Folder { path, recursive } => {
            files::safe(&path, false)?;
            let mut result = Vec::new();
            let mut directories = vec![path.clone()];
            let mut visited = 0;
            while let Some(directory) = directories.pop() {
                visited += 1;
                if visited > MAX_FILES {
                    return Err(invalid(
                        "Folder traversal exceeds 1000 directories; select a smaller scope",
                    ));
                }
                if cancel.is_cancelled() {
                    break;
                }
                if let Err(e) = files::safe(&directory, false) {
                    result.push((directory, Some(e.to_string())));
                    continue;
                }
                let entries = match fs::read_dir(&directory) {
                    Ok(v) => v,
                    Err(e) => {
                        result.push((
                            directory,
                            Some(format!("Cannot read selected directory: {e}")),
                        ));
                        continue;
                    }
                };
                for item in entries {
                    let entry = match item {
                        Ok(v) => v,
                        Err(e) => {
                            result.push((
                                directory.clone(),
                                Some(format!("Cannot read directory entry: {e}")),
                            ));
                            continue;
                        }
                    };
                    let child = entry.path();
                    if !child.starts_with(&path) {
                        return Err(invalid("Entry outside selected folder"));
                    }
                    let meta = match fs::symlink_metadata(&child) {
                        Ok(v) => v,
                        Err(e) => {
                            result
                                .push((child, Some(format!("Cannot inspect selected entry: {e}"))));
                            continue;
                        }
                    };
                    if meta.is_dir() && !meta.file_type().is_symlink() {
                        if recursive {
                            directories.push(child);
                        }
                    } else {
                        result.push((child, None));
                    }
                    if result.len() + directories.len() > MAX_FILES {
                        return Err(invalid(
                            "Selected scope exceeds 1000 entries; select a smaller folder",
                        ));
                    }
                }
            }
            result.sort();
            Ok(result)
        }
    }
}
fn digest(path: &Path, cancel: &Cancellation) -> Result<([u8; 32], u64), ServiceError> {
    files::safe(path, true)?;
    let mut f = File::open(path).map_err(|e| invalid(e.to_string()))?;
    let mut hash = Sha256::new();
    let mut size = 0;
    let mut buffer = [0u8; 65536];
    loop {
        if cancel.is_cancelled() {
            return Err(invalid("Cancelled while hashing selected file"));
        }
        let n = f.read(&mut buffer).map_err(|e| invalid(e.to_string()))?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > MAX_BYTES {
            return Err(invalid("PDF exceeds 256 MiB"));
        }
        hash.update(&buffer[..n]);
    }
    Ok((hash.finalize().into(), size))
}
