//! Owned import/export UI DTOs. Coordinator state remains inside its worker.
use crate::import::*;
pub use cm_core::{
    export::{ExportReport, ExportSelection},
    metadata_resolve::{Cancellation, MetadataResolution},
};
use std::path::PathBuf;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Bibtex,
    Ris,
}
#[derive(Debug, Clone)]
pub struct FileDiagnostic {
    pub line: u64,
    pub message: String,
    pub error: bool,
}
#[derive(Debug, Clone)]
pub struct ParsedFile {
    pub records: usize,
    pub diagnostics: Vec<FileDiagnostic>,
    pub migration_notice: String,
}
#[derive(Debug, Clone)]
pub enum ImportRequestUi {
    Parse {
        path: PathBuf,
        format: Format,
    },
    PreviewFile {
        start: usize,
        count: usize,
        destination: ImportDestination,
    },
    Preview(ImportRequest),
    Commit(ImportCommit),
    Cancel(PreviewId),
}
#[derive(Debug, Clone)]
pub enum ImportReply {
    Parsed(ParsedFile),
    Preview(ImportPreview),
    Committed(ImportOutcome),
    Cancelled,
}
#[derive(Debug, Clone)]
pub struct ExportRequest {
    pub selection: ExportSelection,
    pub format: Format,
    pub path: PathBuf,
}
#[derive(Debug, Clone)]
pub struct Exported {
    pub path: PathBuf,
    pub records: usize,
    pub diagnostics: Vec<String>,
    pub migration_notice: String,
}
