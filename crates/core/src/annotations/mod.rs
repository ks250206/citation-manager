//! Highlights, comments and standalone notes on immutable PDF attachments (#19).
//!
//! Positions are quads in unrotated PDF user space (`crate::geometry`), with the quoted text and
//! a short context kept for re-anchoring. Pages are 0-based here; UIs show 1-based numbers.
//! Every change goes through `AnnotationService`; `History` records the inverse edits so undo
//! and redo replay the same service operations and the DB never diverges from the UI.
use crate::{ServiceError, geometry::Quad};
use cm_domain::{AttachmentId, PaperId};
use serde::{Deserialize, Serialize};

pub const MAX_QUOTE_BYTES: usize = 64 * 1024;
pub const MAX_COMMENT_BYTES: usize = 64 * 1024;
pub const MAX_CONTEXT_CHARS: usize = 48;
pub const MAX_QUADS: usize = 1024;
pub const MAX_HISTORY: usize = 200;
/// Named highlight colours; `#RRGGBB` is also accepted.
pub const COLORS: &[&str] = &["yellow", "green", "blue", "pink", "purple", "orange"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnnotationId(pub uuid::Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnnotationKind {
    Highlight,
    Note,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Human,
    /// Proposed by an AI agent (MCP/API). Shown distinctly until a person confirms or removes it.
    Ai,
    Import,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Anchor {
    Resolved {
        page_index: u32,
        page_label: Option<String>,
        /// Empty for a page-level note.
        quads: Vec<Quad>,
    },
    /// Not placed on the current PDF (e.g. copied from a replaced PDF). Never guessed silently.
    Unresolved { reason: String },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub id: AnnotationId,
    pub attachment_id: AttachmentId,
    pub kind: AnnotationKind,
    pub source: Source,
    pub anchor: Anchor,
    pub quote: String,
    pub context_before: Option<String>,
    pub context_after: Option<String>,
    pub color: Option<String>,
    pub comment: Option<String>,
    /// When a person accepted an AI annotation; `None` for unreviewed AI and for non-AI sources.
    pub confirmed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewAnnotation {
    pub attachment_id: AttachmentId,
    pub kind: AnnotationKind,
    pub source: Source,
    pub anchor: Anchor,
    pub quote: String,
    pub context_before: Option<String>,
    pub context_after: Option<String>,
    pub color: Option<String>,
    pub comment: Option<String>,
}
/// `Some(None)` clears a field; `None` leaves it unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AnnotationPatch {
    pub color: Option<Option<String>>,
    pub comment: Option<Option<String>>,
}

/// Storage port. `insert` keeps the given id/timestamps (needed to undo a delete exactly).
pub trait AnnotationRepository {
    fn now(&self) -> Result<String, ServiceError>;
    fn insert(&mut self, annotation: &Annotation) -> Result<(), ServiceError>;
    fn replace(&mut self, annotation: &Annotation) -> Result<(), ServiceError>;
    fn delete(&mut self, id: AnnotationId) -> Result<bool, ServiceError>;
    fn get(&self, id: AnnotationId) -> Result<Option<Annotation>, ServiceError>;
    fn for_attachment(&self, id: AttachmentId) -> Result<Vec<Annotation>, ServiceError>;
    fn for_paper(&self, id: PaperId) -> Result<Vec<Annotation>, ServiceError>;
    /// The attachment exists and belongs to an active paper.
    fn attachment_exists(&self, id: AttachmentId) -> Result<bool, ServiceError>;
}

fn invalid(message: &str) -> ServiceError {
    ServiceError::InvalidInput(message.into())
}

pub fn valid_color(color: &str) -> bool {
    COLORS.contains(&color)
        || (color.len() == 7
            && color.starts_with('#')
            && color[1..].bytes().all(|b| b.is_ascii_hexdigit()))
}

fn validate(a: &NewAnnotation) -> Result<(), ServiceError> {
    if a.quote.len() > MAX_QUOTE_BYTES {
        return Err(invalid("Quoted text is too long"));
    }
    if a.comment
        .as_ref()
        .is_some_and(|c| c.len() > MAX_COMMENT_BYTES)
    {
        return Err(invalid("Comment is too long"));
    }
    if a.color.as_deref().is_some_and(|c| !valid_color(c)) {
        return Err(invalid("Unknown highlight colour"));
    }
    for context in [&a.context_before, &a.context_after].into_iter().flatten() {
        if context.chars().count() > MAX_CONTEXT_CHARS {
            return Err(invalid("Context is too long"));
        }
    }
    match (&a.kind, &a.anchor) {
        (_, Anchor::Unresolved { reason }) if reason.trim().is_empty() => {
            Err(invalid("Unresolved annotations need a reason"))
        }
        (AnnotationKind::Highlight, Anchor::Resolved { quads, .. }) => {
            if quads.is_empty() || quads.len() > MAX_QUADS || !quads.iter().all(|q| q.valid()) {
                Err(invalid("A highlight needs 1–1024 valid quads"))
            } else if a.quote.trim().is_empty() {
                Err(invalid("A highlight needs the selected text"))
            } else {
                Ok(())
            }
        }
        (AnnotationKind::Note, Anchor::Resolved { quads, .. }) => {
            if quads.len() > MAX_QUADS || !quads.iter().all(|q| q.valid()) {
                Err(invalid("Invalid note position"))
            } else if a.comment.as_deref().is_none_or(|c| c.trim().is_empty()) {
                Err(invalid("A note needs text"))
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

/// One recorded change; undo applies its inverse through the service.
// Edits are created one per user action and kept in a bounded history; boxing would only
// complicate every pattern match for no measurable gain.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Edit {
    Created(Annotation),
    Updated {
        before: Annotation,
        after: Annotation,
    },
    Deleted(Annotation),
}
impl Edit {
    fn inverse(self) -> Self {
        match self {
            Edit::Created(a) => Edit::Deleted(a),
            Edit::Deleted(a) => Edit::Created(a),
            Edit::Updated { before, after } => Edit::Updated {
                before: after,
                after: before,
            },
        }
    }
}

/// Bounded undo/redo stacks. A new edit clears redo.
#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
}
impl History {
    pub fn record(&mut self, edit: Edit) {
        self.redo.clear();
        self.undo.push(edit);
        if self.undo.len() > MAX_HISTORY {
            self.undo.remove(0);
        }
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn take_undo(&mut self) -> Option<Edit> {
        self.undo.pop()
    }
    pub fn take_redo(&mut self) -> Option<Edit> {
        self.redo.pop()
    }
    /// After a successful undo, the applied inverse becomes redoable.
    pub fn undone(&mut self, edit: Edit) {
        self.redo.push(edit);
    }
    /// After a successful redo, it becomes undoable again (without clearing redo).
    pub fn redone(&mut self, edit: Edit) {
        self.undo.push(edit);
    }
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

pub struct AnnotationService<R> {
    repo: R,
}
impl<R: AnnotationRepository> AnnotationService<R> {
    pub fn new(repo: R) -> Self {
        Self { repo }
    }
    pub fn get(&self, id: AnnotationId) -> Result<Option<Annotation>, ServiceError> {
        self.repo.get(id)
    }
    pub fn list(&self, attachment: AttachmentId) -> Result<Vec<Annotation>, ServiceError> {
        self.repo.for_attachment(attachment)
    }
    pub fn for_paper(&self, paper: PaperId) -> Result<Vec<Annotation>, ServiceError> {
        self.repo.for_paper(paper)
    }
    pub fn create(&mut self, input: NewAnnotation) -> Result<Edit, ServiceError> {
        validate(&input)?;
        if !self.repo.attachment_exists(input.attachment_id)? {
            return Err(ServiceError::NotFound);
        }
        let now = self.repo.now()?;
        let annotation = Annotation {
            id: AnnotationId(uuid::Uuid::now_v7()),
            attachment_id: input.attachment_id,
            kind: input.kind,
            source: input.source,
            anchor: input.anchor,
            quote: input.quote,
            context_before: input.context_before,
            context_after: input.context_after,
            color: input.color,
            comment: input.comment.filter(|c| !c.is_empty()),
            confirmed_at: None,
            created_at: now.clone(),
            updated_at: now,
        };
        self.repo.insert(&annotation)?;
        Ok(Edit::Created(annotation))
    }
    fn existing(&self, id: AnnotationId) -> Result<Annotation, ServiceError> {
        self.repo.get(id)?.ok_or(ServiceError::NotFound)
    }
    pub fn update(
        &mut self,
        id: AnnotationId,
        patch: AnnotationPatch,
    ) -> Result<Edit, ServiceError> {
        let before = self.existing(id)?;
        let mut after = before.clone();
        if let Some(color) = patch.color {
            after.color = color;
        }
        if let Some(comment) = patch.comment {
            after.comment = comment.filter(|c| !c.is_empty());
        }
        validate(&NewAnnotation {
            attachment_id: after.attachment_id,
            kind: after.kind,
            source: after.source,
            anchor: after.anchor.clone(),
            quote: after.quote.clone(),
            context_before: after.context_before.clone(),
            context_after: after.context_after.clone(),
            color: after.color.clone(),
            comment: after.comment.clone(),
        })?;
        after.updated_at = self.repo.now()?;
        self.repo.replace(&after)?;
        Ok(Edit::Updated { before, after })
    }
    pub fn delete(&mut self, id: AnnotationId) -> Result<Edit, ServiceError> {
        let before = self.existing(id)?;
        if !self.repo.delete(id)? {
            return Err(ServiceError::NotFound);
        }
        Ok(Edit::Deleted(before))
    }
    /// Accept an AI proposal: it stays `source=ai` (provenance) but is marked confirmed.
    pub fn confirm_ai(&mut self, id: AnnotationId) -> Result<Edit, ServiceError> {
        let before = self.existing(id)?;
        if before.source != Source::Ai {
            return Err(invalid("Only AI annotations need confirmation"));
        }
        let mut after = before.clone();
        let now = self.repo.now()?;
        after.confirmed_at = Some(now.clone());
        after.updated_at = now;
        self.repo.replace(&after)?;
        Ok(Edit::Updated { before, after })
    }
    /// Place an unresolved annotation after the person confirmed a proposed position.
    pub fn resolve(
        &mut self,
        id: AnnotationId,
        page_index: u32,
        page_label: Option<String>,
        quads: Vec<Quad>,
    ) -> Result<Edit, ServiceError> {
        let before = self.existing(id)?;
        let mut after = before.clone();
        after.anchor = Anchor::Resolved {
            page_index,
            page_label,
            quads,
        };
        validate(&NewAnnotation {
            attachment_id: after.attachment_id,
            kind: after.kind,
            source: after.source,
            anchor: after.anchor.clone(),
            quote: after.quote.clone(),
            context_before: after.context_before.clone(),
            context_after: after.context_after.clone(),
            color: after.color.clone(),
            comment: after.comment.clone(),
        })?;
        after.updated_at = self.repo.now()?;
        self.repo.replace(&after)?;
        Ok(Edit::Updated { before, after })
    }
    /// PDF replaced by a new attachment: copy annotations as *unresolved* on the new one.
    /// Nothing is placed until the person reviews each proposal (`propose_anchor` + `resolve`).
    pub fn carry_over(
        &mut self,
        from: AttachmentId,
        to: AttachmentId,
    ) -> Result<Vec<Edit>, ServiceError> {
        if from == to {
            return Err(invalid("Choose a different PDF"));
        }
        if !self.repo.attachment_exists(to)? {
            return Err(ServiceError::NotFound);
        }
        let mut edits = Vec::new();
        for old in self.repo.for_attachment(from)? {
            edits.push(self.create(NewAnnotation {
                attachment_id: to,
                kind: old.kind,
                source: old.source,
                anchor: Anchor::Unresolved {
                    reason: "Copied from a replaced PDF; confirm its position".into(),
                },
                quote: old.quote,
                context_before: old.context_before,
                context_after: old.context_after,
                color: old.color,
                comment: old.comment,
            })?);
        }
        Ok(edits)
    }
    /// Undo a recorded edit by performing its inverse through the same storage operations.
    pub fn undo(&mut self, edit: &Edit) -> Result<(), ServiceError> {
        self.perform(edit.clone().inverse())
    }
    /// Redo a previously undone edit.
    pub fn redo(&mut self, edit: &Edit) -> Result<(), ServiceError> {
        self.perform(edit.clone())
    }
    /// Rows are restored exactly (ids and timestamps), and only from the state the edit
    /// expects; anything changed meanwhile (another client, AI agent) is a Conflict.
    fn perform(&mut self, edit: Edit) -> Result<(), ServiceError> {
        match edit {
            Edit::Created(a) => {
                if self.repo.get(a.id)?.is_some() {
                    return Err(ServiceError::Conflict);
                }
                if !self.repo.attachment_exists(a.attachment_id)? {
                    return Err(ServiceError::NotFound);
                }
                self.repo.insert(&a)
            }
            Edit::Deleted(a) => {
                if self.existing(a.id)? != a {
                    return Err(ServiceError::Conflict);
                }
                if self.repo.delete(a.id)? {
                    Ok(())
                } else {
                    Err(ServiceError::NotFound)
                }
            }
            Edit::Updated { before, after } => {
                if self.existing(after.id)? != before {
                    return Err(ServiceError::Conflict);
                }
                self.repo.replace(&after)
            }
        }
    }
}

/// One page of the replacement PDF: index, label and characters with their quads.
pub struct PageText<'a> {
    pub page_index: u32,
    pub page_label: Option<String>,
    pub chars: &'a [(Option<char>, Quad)],
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub page_index: u32,
    pub page_label: Option<String>,
    pub quads: Vec<Quad>,
    /// Number of exact quote matches found (1 = unambiguous).
    pub matches: usize,
    /// Context agreed on both sides.
    pub context_match: bool,
}
/// Find the quote in the new PDF, preferring an occurrence whose surrounding text matches the
/// stored context. Returns None when the quote no longer exists. Never applied automatically.
pub fn propose_anchor(annotation: &Annotation, pages: &[PageText<'_>]) -> Option<Proposal> {
    let quote: Vec<char> = annotation.quote.chars().collect();
    if quote.is_empty() {
        return None;
    }
    let mut found: Vec<(u32, Option<String>, Vec<Quad>, bool)> = Vec::new();
    for page in pages {
        let text: Vec<char> = page
            .chars
            .iter()
            .map(|(c, _)| c.unwrap_or('\u{fffd}'))
            .collect();
        if text.len() < quote.len() {
            continue;
        }
        for start in 0..=text.len() - quote.len() {
            if text[start..start + quote.len()] != quote[..] {
                continue;
            }
            let end = start + quote.len();
            let before: String = text[start.saturating_sub(MAX_CONTEXT_CHARS)..start]
                .iter()
                .collect();
            let after: String = text[end..(end + MAX_CONTEXT_CHARS).min(text.len())]
                .iter()
                .collect();
            let context_match = annotation
                .context_before
                .as_deref()
                .is_none_or(|c| before.ends_with(c))
                && annotation
                    .context_after
                    .as_deref()
                    .is_none_or(|c| after.starts_with(c));
            let quads = page.chars[start..end].iter().map(|(_, q)| *q).collect();
            found.push((
                page.page_index,
                page.page_label.clone(),
                quads,
                context_match,
            ));
        }
    }
    let matches = found.len();
    let best = found
        .iter()
        .position(|f| f.3)
        .or((matches > 0).then_some(0))?;
    let (page_index, page_label, quads, context_match) = found.swap_remove(best);
    Some(Proposal {
        page_index,
        page_label,
        quads,
        matches,
        context_match,
    })
}
