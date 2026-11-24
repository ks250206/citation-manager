//! Wire form of `attachments::PdfRequest`: the in-process `Cancellation` token is replaced
//! by a `ClientFrame::Cancel` for the request id, which the daemon maps to a fresh token.
use crate::{
    attachments::{PdfIntent, PdfRequest, Selection},
    domain::PaperId,
    exchange::Cancellation,
    import::{ImportCommit, ImportDestination, PaperDraft},
};
use cm_core::policy::Operation;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PdfCommand {
    Inspect {
        selection: Selection,
        intent: PdfIntent,
    },
    Attach {
        indices: Vec<usize>,
        paper: PaperId,
    },
    PreviewNew {
        index: usize,
        explicit: Option<Box<PaperDraft>>,
        destination: ImportDestination,
    },
    CommitNew {
        index: usize,
        commit: ImportCommit,
    },
    RetryAttachment {
        index: usize,
    },
    List(PaperId),
    /// #17 viewer page render (the token stays client-side; see `ClientFrame::Cancel`).
    Render {
        attachment: crate::domain::AttachmentId,
        page: u32,
        zoom: f32,
    },
    /// #20 annotated copy (the token stays client-side; see `ClientFrame::Cancel`).
    Export {
        attachment: crate::domain::AttachmentId,
        destination: std::path::PathBuf,
        replace_existing: bool,
        allow_omissions: bool,
    },
}
impl PdfCommand {
    /// Splits the cancellation token (kept by the client) from the serializable command.
    pub fn from_request(request: PdfRequest) -> (Self, Option<Cancellation>) {
        match request {
            PdfRequest::Inspect {
                selection,
                intent,
                cancel,
            } => (Self::Inspect { selection, intent }, Some(cancel)),
            PdfRequest::Attach {
                indices,
                paper,
                cancel,
            } => (Self::Attach { indices, paper }, Some(cancel)),
            PdfRequest::PreviewNew {
                index,
                explicit,
                destination,
            } => (
                Self::PreviewNew {
                    index,
                    explicit,
                    destination,
                },
                None,
            ),
            PdfRequest::CommitNew {
                index,
                commit,
                cancel,
            } => (Self::CommitNew { index, commit }, Some(cancel)),
            PdfRequest::RetryAttachment { index, cancel } => {
                (Self::RetryAttachment { index }, Some(cancel))
            }
            PdfRequest::List(paper) => (Self::List(paper), None),
            PdfRequest::Render(r) => (
                Self::Render {
                    attachment: r.attachment,
                    page: r.page,
                    zoom: r.zoom,
                },
                Some(r.cancel),
            ),
            PdfRequest::Export(r) => (
                Self::Export {
                    attachment: r.attachment,
                    destination: r.destination,
                    replace_existing: r.replace_existing,
                    allow_omissions: r.allow_omissions,
                },
                Some(r.cancel),
            ),
        }
    }
    /// Daemon side: bind the command to a daemon-owned token for this request.
    pub fn into_request(self, cancel: Cancellation) -> PdfRequest {
        match self {
            Self::Inspect { selection, intent } => PdfRequest::Inspect {
                selection,
                intent,
                cancel,
            },
            Self::Attach { indices, paper } => PdfRequest::Attach {
                indices,
                paper,
                cancel,
            },
            Self::PreviewNew {
                index,
                explicit,
                destination,
            } => PdfRequest::PreviewNew {
                index,
                explicit,
                destination,
            },
            Self::CommitNew { index, commit } => PdfRequest::CommitNew {
                index,
                commit,
                cancel,
            },
            Self::RetryAttachment { index } => PdfRequest::RetryAttachment { index, cancel },
            Self::List(paper) => PdfRequest::List(paper),
            Self::Render {
                attachment,
                page,
                zoom,
            } => PdfRequest::Render(crate::attachments::RenderRequest {
                attachment,
                page,
                zoom,
                cancel,
            }),
            Self::Export {
                attachment,
                destination,
                replace_existing,
                allow_omissions,
            } => PdfRequest::Export(crate::attachments::PdfExportRequest {
                attachment,
                destination,
                replace_existing,
                allow_omissions,
                cancel,
            }),
        }
    }
    pub fn operation(&self) -> Operation {
        match self {
            Self::Inspect { .. } => Operation::PdfInspect,
            Self::Attach { .. } => Operation::PdfAttach,
            Self::PreviewNew { .. } => Operation::PdfPreviewNew,
            Self::CommitNew { .. } => Operation::PdfCommitNew,
            Self::RetryAttachment { .. } => Operation::PdfRetryAttachment,
            Self::List(_) => Operation::AttachmentList,
            Self::Render { .. } => Operation::PdfRender,
            Self::Export { .. } => Operation::PdfExport,
        }
    }
}
