//! #19 annotation requests. The same DTOs serve the GUI now and REST/MCP (#27/#29) later.
use crate::domain::{AttachmentId, PaperId};
pub use cm_core::annotations::{
    Anchor, Annotation, AnnotationId, AnnotationKind, AnnotationPatch, COLORS, Edit, History,
    MAX_CONTEXT_CHARS, MAX_PROPOSAL_MATCHES, NewAnnotation, Proposal, Source, merge_line_quads,
    pending_carry_over, valid_color,
};
use cm_core::{geometry::Quad, metadata_resolve::Cancellation};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AnnotationRequest {
    List(AttachmentId),
    ForPaper(PaperId),
    Create(Box<NewAnnotation>),
    Update(AnnotationId, AnnotationPatch),
    Delete(AnnotationId),
    /// Accept an AI-proposed annotation (keeps source=ai, records confirmation).
    ConfirmAi(AnnotationId),
    Undo(Box<Edit>),
    Redo(Box<Edit>),
    /// The paper's PDF was replaced: copy annotations to `to` as unresolved.
    CarryOver {
        from: AttachmentId,
        to: AttachmentId,
    },
    /// Search the annotation's PDF for its quote/context. Never applied automatically.
    /// Runs off the library worker under an overall deadline; `cancel` stops it early
    /// (e.g. the paper was switched).
    ///
    /// Over IPC (#25) `cancel` is local to each side and never serialized: the client
    /// watches it and sends `ClientFrame::Cancel` for the request id, and papersd installs
    /// its own per-request token (cancelled by that frame or by the client disconnecting).
    Propose {
        id: AnnotationId,
        #[serde(skip)]
        cancel: Cancellation,
    },
    Resolve {
        id: AnnotationId,
        page_index: u32,
        page_label: Option<String>,
        quads: Vec<Quad>,
    },
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AnnotationReply {
    List(Vec<Annotation>),
    Edit(Box<Edit>),
    Edits(Vec<Edit>),
    Done,
    Proposal(Option<Proposal>),
}

impl AnnotationRequest {
    /// Reads need LibraryRead; any change (including AI-proposed annotations) needs LibraryWrite.
    pub fn operation(&self) -> cm_core::policy::Operation {
        use cm_core::policy::Operation as O;
        match self {
            Self::List(_) | Self::ForPaper(_) | Self::Propose { .. } => O::AnnotationRead,
            _ => O::AnnotationWrite,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cancellation token is process-local: it never goes on the wire and a decoded
    /// Propose starts with a fresh, uncancelled token (papersd then installs its own).
    #[test]
    fn propose_crosses_ipc_without_its_cancellation_token() {
        let id = AnnotationId(uuid::Uuid::from_u128(7));
        let cancel = Cancellation::default();
        cancel.cancel();
        let json = serde_json::to_string(&AnnotationRequest::Propose { id, cancel }).unwrap();
        assert!(!json.contains("cancel"), "{json}");
        let AnnotationRequest::Propose {
            id: decoded,
            cancel,
        } = serde_json::from_str(&json).unwrap()
        else {
            panic!("{json}")
        };
        assert_eq!(decoded, id);
        assert!(!cancel.is_cancelled());
    }
}
