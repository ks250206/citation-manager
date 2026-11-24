//! #19 annotation requests. The same DTOs serve the GUI now and REST/MCP (#27/#29) later.
use crate::domain::{AttachmentId, PaperId};
pub use cm_core::annotations::{
    Anchor, Annotation, AnnotationId, AnnotationKind, AnnotationPatch, COLORS, Edit, History,
    MAX_CONTEXT_CHARS, MAX_PROPOSAL_MATCHES, NewAnnotation, Proposal, Source, merge_line_quads,
    pending_carry_over, valid_color,
};
use cm_core::{geometry::Quad, metadata_resolve::Cancellation};

#[derive(Debug, Clone)]
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
    Propose {
        id: AnnotationId,
        cancel: Cancellation,
    },
    Resolve {
        id: AnnotationId,
        page_index: u32,
        page_label: Option<String>,
        quads: Vec<Quad>,
    },
}
#[derive(Debug, Clone)]
pub enum AnnotationReply {
    List(Vec<Annotation>),
    Edit(Box<Edit>),
    Edits(Vec<Edit>),
    Done,
    Proposal(Option<Proposal>),
}
