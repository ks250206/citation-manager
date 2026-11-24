//! Highlights, comments and standalone notes on immutable PDF attachments (#19).
//!
//! Positions are quads in unrotated PDF user space (`crate::geometry`), with the quoted text and
//! a short context kept for re-anchoring. Pages are 0-based here; UIs show 1-based numbers.
//! Every change goes through `AnnotationService`; `History` records the inverse edits so undo
//! and redo replay the same service operations and the DB never diverges from the UI.
//!
//! Text rule shared by the viewer and re-anchoring: quote and context are the characters of a
//! range that have a Unicode value; characters PDFium cannot map are skipped, never replaced.
use crate::{
    ServiceError,
    geometry::{Quad, Rect},
};
use cm_domain::{AttachmentId, PaperId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

pub const MAX_QUOTE_BYTES: usize = 64 * 1024;
pub const MAX_COMMENT_BYTES: usize = 64 * 1024;
pub const MAX_CONTEXT_CHARS: usize = 48;
pub const MAX_QUADS: usize = 1024;
pub const MAX_HISTORY: usize = 200;
/// Highest storable 0-based page index (its 1-based display number must fit in u32).
pub const MAX_PAGE_INDEX: u32 = u32::MAX - 1;
pub const MAX_PAGE_LABEL_BYTES: usize = 256;
pub const MAX_REASON_BYTES: usize = 1024;
pub const MAX_TIMESTAMP_BYTES: usize = 64;
/// Re-anchoring stops counting occurrences of a quote after this many.
pub const MAX_PROPOSAL_MATCHES: usize = 1000;
/// Confirmation timestamps remembered for undo/redo (see `Confirmations`).
const MAX_CONFIRMATIONS: usize = 4096;
/// Named highlight colours; `#RRGGBB` is also accepted.
pub const COLORS: &[&str] = &["yellow", "green", "blue", "pink", "purple", "orange"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnnotationId(pub uuid::Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// Inserts every row or none (one transaction).
    fn insert_all(&mut self, annotations: &[Annotation]) -> Result<(), ServiceError>;
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

/// Whitespace-only comments are stored as no comment, on every path.
fn clean_comment(comment: Option<String>) -> Option<String> {
    comment.filter(|c| !c.trim().is_empty())
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
    match &a.anchor {
        Anchor::Unresolved { reason } => {
            if reason.trim().is_empty() {
                return Err(invalid("Unresolved annotations need a reason"));
            }
            if reason.len() > MAX_REASON_BYTES {
                return Err(invalid("Unresolved reason is too long"));
            }
        }
        Anchor::Resolved {
            page_index,
            page_label,
            quads,
        } => {
            if *page_index > MAX_PAGE_INDEX {
                return Err(invalid("Page out of range"));
            }
            if page_label
                .as_ref()
                .is_some_and(|l| l.len() > MAX_PAGE_LABEL_BYTES)
            {
                return Err(invalid("Page label is too long"));
            }
            // Quad::valid requires finite coordinates: NaN/∞ would not survive JSON storage.
            if quads.len() > MAX_QUADS || !quads.iter().all(|q| q.valid()) {
                return Err(invalid("A position needs at most 1024 valid, finite quads"));
            }
            if a.kind == AnnotationKind::Highlight && quads.is_empty() {
                return Err(invalid("A highlight needs 1–1024 valid quads"));
            }
        }
    }
    match a.kind {
        AnnotationKind::Highlight if a.quote.trim().is_empty() => {
            Err(invalid("A highlight needs the selected text"))
        }
        AnnotationKind::Note if a.comment.as_deref().is_none_or(|c| c.trim().is_empty()) => {
            Err(invalid("A note needs text"))
        }
        _ => Ok(()),
    }
}

fn fields(a: &Annotation) -> NewAnnotation {
    NewAnnotation {
        attachment_id: a.attachment_id,
        kind: a.kind,
        source: a.source,
        anchor: a.anchor.clone(),
        quote: a.quote.clone(),
        context_before: a.context_before.clone(),
        context_after: a.context_after.clone(),
        color: a.color.clone(),
        comment: a.comment.clone(),
    }
}

/// A complete row as it would be written: field rules plus provenance and timestamps.
fn validate_row(a: &Annotation) -> Result<(), ServiceError> {
    validate(&fields(a))?;
    if a.confirmed_at.is_some() && a.source != Source::Ai {
        return Err(invalid("Only AI annotations carry a confirmation"));
    }
    if a.comment.as_deref().is_some_and(|c| c.trim().is_empty()) {
        return Err(invalid("Empty comments are stored as no comment"));
    }
    for at in [
        Some(&a.created_at),
        Some(&a.updated_at),
        a.confirmed_at.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if at.trim().is_empty() || at.len() > MAX_TIMESTAMP_BYTES {
            return Err(invalid("Invalid timestamp"));
        }
    }
    Ok(())
}

/// Fields an `Updated` edit may never change: identity, quoted text and provenance.
fn same_identity(a: &Annotation, b: &Annotation) -> bool {
    a.id == b.id
        && a.attachment_id == b.attachment_id
        && a.kind == b.kind
        && a.source == b.source
        && a.quote == b.quote
        && a.context_before == b.context_before
        && a.context_after == b.context_after
        && a.created_at == b.created_at
}

/// One recorded change; undo applies its inverse through the service.
// Edits are created one per user action and kept in a bounded history; boxing would only
// complicate every pattern match for no measurable gain.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
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
    /// A failed undo (not a conflict) keeps its step so the person can retry.
    pub fn restore_undo(&mut self, edit: Edit) {
        self.undo.push(edit);
    }
    /// A failed redo (not a conflict) keeps its step so the person can retry.
    pub fn restore_redo(&mut self, edit: Edit) {
        self.redo.push(edit);
    }
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

/// Confirmation timestamps this service wrote (Confirm, carry-over) or saw on rows it removed
/// or overwrote. Undo/redo may write a row with `confirmed_at` only when that exact
/// (id, timestamp) is known here or is already the row's current value, so replaying a
/// client-supplied edit can restore a confirmation exactly but never invent one. Keep one per
/// writer for the process lifetime (the UI's history lives no longer).
#[derive(Debug, Default)]
pub struct Confirmations {
    seen: VecDeque<(AnnotationId, String)>,
}
impl Confirmations {
    fn note(&mut self, a: &Annotation) {
        if let Some(at) = &a.confirmed_at
            && !self.knows(a.id, at)
        {
            self.seen.push_back((a.id, at.clone()));
            if self.seen.len() > MAX_CONFIRMATIONS {
                self.seen.pop_front();
            }
        }
    }
    fn knows(&self, id: AnnotationId, at: &str) -> bool {
        self.seen.iter().any(|(i, a)| *i == id && a == at)
    }
}

/// What makes two annotations "the same" for carry-over: kind, quoted text, context and, for
/// notes (which have no quote), the note text.
type CarryKey<'a> = (
    AnnotationKind,
    &'a str,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
);
fn carry_key(a: &Annotation) -> CarryKey<'_> {
    (
        a.kind,
        &a.quote,
        a.context_before.as_deref(),
        a.context_after.as_deref(),
        match a.kind {
            AnnotationKind::Note => a.comment.as_deref(),
            AnnotationKind::Highlight => None,
        },
    )
}
/// Annotations of `from` that `to` does not have yet. Each existing row on `to` accounts for
/// one matching row of `from`, so repeating a carry-over copies nothing twice.
pub fn pending_carry_over<'a>(
    from: impl IntoIterator<Item = &'a Annotation>,
    to: impl IntoIterator<Item = &'a Annotation>,
) -> Vec<&'a Annotation> {
    let mut present: HashMap<CarryKey<'a>, usize> = HashMap::new();
    for a in to {
        *present.entry(carry_key(a)).or_default() += 1;
    }
    from.into_iter()
        .filter(|a| match present.get_mut(&carry_key(a)) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .collect()
}

pub struct AnnotationService<R> {
    repo: R,
    confirmations: Confirmations,
}
impl<R: AnnotationRepository> AnnotationService<R> {
    pub fn new(repo: R) -> Self {
        Self::with_confirmations(repo, Confirmations::default())
    }
    /// A long-lived writer passes its `Confirmations` in and takes them back afterwards.
    pub fn with_confirmations(repo: R, confirmations: Confirmations) -> Self {
        Self {
            repo,
            confirmations,
        }
    }
    pub fn into_confirmations(self) -> Confirmations {
        self.confirmations
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
    pub fn create(&mut self, mut input: NewAnnotation) -> Result<Edit, ServiceError> {
        input.comment = clean_comment(input.comment);
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
            comment: input.comment,
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
            after.comment = clean_comment(comment);
        }
        validate_row(&after)?;
        after.updated_at = self.repo.now()?;
        self.repo.replace(&after)?;
        Ok(Edit::Updated { before, after })
    }
    pub fn delete(&mut self, id: AnnotationId) -> Result<Edit, ServiceError> {
        let before = self.existing(id)?;
        if !self.repo.delete(id)? {
            return Err(ServiceError::NotFound);
        }
        self.confirmations.note(&before);
        Ok(Edit::Deleted(before))
    }
    /// Accept an AI proposal: it stays `source=ai` (provenance) but is marked confirmed.
    pub fn confirm_ai(&mut self, id: AnnotationId) -> Result<Edit, ServiceError> {
        let before = self.existing(id)?;
        if before.source != Source::Ai {
            return Err(invalid("Only AI annotations need confirmation"));
        }
        if before.confirmed_at.is_some() {
            return Err(invalid("This AI annotation is already confirmed"));
        }
        let mut after = before.clone();
        let now = self.repo.now()?;
        after.confirmed_at = Some(now.clone());
        after.updated_at = now;
        self.repo.replace(&after)?;
        self.confirmations.note(&after);
        Ok(Edit::Updated { before, after })
    }
    /// Place an unresolved annotation after the person confirmed a proposed position.
    /// Already-placed annotations are not moved this way.
    pub fn resolve(
        &mut self,
        id: AnnotationId,
        page_index: u32,
        page_label: Option<String>,
        quads: Vec<Quad>,
    ) -> Result<Edit, ServiceError> {
        let before = self.existing(id)?;
        if !matches!(before.anchor, Anchor::Unresolved { .. }) {
            return Err(invalid("This annotation is already placed"));
        }
        let mut after = before.clone();
        after.anchor = Anchor::Resolved {
            page_index,
            page_label,
            quads,
        };
        validate_row(&after)?;
        after.updated_at = self.repo.now()?;
        self.repo.replace(&after)?;
        Ok(Edit::Updated { before, after })
    }
    /// PDF replaced by a new attachment: copy annotations as *unresolved* on the new one, in one
    /// transaction. Annotations already copied (see `pending_carry_over`) are skipped; AI
    /// provenance and confirmation are kept. Nothing is placed until the person reviews each
    /// proposal (`propose_anchor` + `resolve`).
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
        let source = self.repo.for_attachment(from)?;
        let target = self.repo.for_attachment(to)?;
        let now = self.repo.now()?;
        let rows: Vec<Annotation> = pending_carry_over(&source, &target)
            .into_iter()
            .map(|old| Annotation {
                id: AnnotationId(uuid::Uuid::now_v7()),
                attachment_id: to,
                kind: old.kind,
                source: old.source,
                anchor: Anchor::Unresolved {
                    reason: "Copied from a replaced PDF; confirm its position".into(),
                },
                quote: old.quote.clone(),
                context_before: old.context_before.clone(),
                context_after: old.context_after.clone(),
                color: old.color.clone(),
                comment: old.comment.clone(),
                confirmed_at: old.confirmed_at.clone(),
                created_at: now.clone(),
                updated_at: now.clone(),
            })
            .collect();
        for row in &rows {
            validate_row(row)?;
        }
        self.repo.insert_all(&rows)?;
        for row in &rows {
            self.confirmations.note(row);
        }
        Ok(rows.into_iter().map(Edit::Created).collect())
    }
    /// Undo a recorded edit by performing its inverse through the same storage operations.
    pub fn undo(&mut self, edit: &Edit) -> Result<(), ServiceError> {
        self.perform(edit.clone().inverse())
    }
    /// Redo a previously undone edit.
    pub fn redo(&mut self, edit: &Edit) -> Result<(), ServiceError> {
        self.perform(edit.clone())
    }
    /// A written `confirmed_at` must be the row's current value or one this service recorded.
    fn check_confirmation(
        &self,
        row: &Annotation,
        current: Option<&Annotation>,
    ) -> Result<(), ServiceError> {
        match &row.confirmed_at {
            None => Ok(()),
            Some(at)
                if current.is_some_and(|c| c.confirmed_at.as_ref() == Some(at))
                    || self.confirmations.knows(row.id, at) =>
            {
                Ok(())
            }
            Some(_) => Err(invalid(
                "Only Confirm can mark an AI annotation as reviewed",
            )),
        }
    }
    /// Edits arrive from clients, so every row is validated like a new write: field rules,
    /// confirmation only on AI rows and only as recorded (`Confirmations`), and an update may
    /// not change identity, quoted text or provenance. Rows are then restored exactly (ids and
    /// timestamps), and only from the state the edit expects; anything changed meanwhile
    /// (another client, AI agent) is a Conflict.
    fn perform(&mut self, edit: Edit) -> Result<(), ServiceError> {
        match edit {
            Edit::Created(a) => {
                validate_row(&a)?;
                self.check_confirmation(&a, None)?;
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
                    self.confirmations.note(&a);
                    Ok(())
                } else {
                    Err(ServiceError::NotFound)
                }
            }
            Edit::Updated { before, after } => {
                if !same_identity(&before, &after) {
                    return Err(invalid(
                        "An edit cannot change an annotation's identity, text or provenance",
                    ));
                }
                validate_row(&after)?;
                if self.existing(after.id)? != before {
                    return Err(ServiceError::Conflict);
                }
                self.check_confirmation(&after, Some(&before))?;
                self.repo.replace(&after)?;
                self.confirmations.note(&before);
                Ok(())
            }
        }
    }
}

/// Merge per-character quads into one rectangle per text line: consecutive glyphs whose
/// vertical extents overlap by at least half of the smaller one share a line.
pub fn merge_line_quads(quads: impl IntoIterator<Item = Quad>) -> Vec<Quad> {
    let mut lines: Vec<Rect> = Vec::new();
    for q in quads {
        let b = q.bounds();
        if !b.valid() {
            continue;
        }
        match lines.last_mut() {
            Some(line) if same_line(*line, b) => {
                line.left = line.left.min(b.left);
                line.right = line.right.max(b.right);
                line.bottom = line.bottom.min(b.bottom);
                line.top = line.top.max(b.top);
            }
            _ => lines.push(b),
        }
    }
    lines.into_iter().map(Rect::quad).collect()
}
fn same_line(a: Rect, b: Rect) -> bool {
    let overlap = a.top.min(b.top) - a.bottom.max(b.bottom);
    overlap >= 0.5 * a.height().min(b.height())
}

/// One page of the replacement PDF: index, label and characters with their quads.
pub struct PageText<'a> {
    pub page_index: u32,
    pub page_label: Option<String>,
    pub chars: &'a [(Option<char>, Quad)],
}
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub page_index: u32,
    pub page_label: Option<String>,
    /// One quad per line of the found text (as the viewer stores highlights).
    pub quads: Vec<Quad>,
    /// Exact quote matches found (1 = unambiguous), counted up to `MAX_PROPOSAL_MATCHES`.
    pub matches: usize,
    /// Context agreed on both sides.
    pub context_match: bool,
}

/// Page-by-page search for an annotation's quote, so callers can stream pages from a PDF
/// without holding the whole document. Prefers the first occurrence whose stored context
/// matches; otherwise the first occurrence. Never applied automatically.
pub struct AnchorSearch<'a> {
    annotation: &'a Annotation,
    quote: Vec<char>,
    /// KMP failure table of `quote`.
    table: Vec<usize>,
    matches: usize,
    best: Option<Proposal>,
}
impl<'a> AnchorSearch<'a> {
    pub fn new(annotation: &'a Annotation) -> Self {
        let quote: Vec<char> = annotation.quote.chars().collect();
        let mut table = vec![0; quote.len()];
        let mut k = 0;
        for i in 1..quote.len() {
            while k > 0 && quote[i] != quote[k] {
                k = table[k - 1];
            }
            if quote[i] == quote[k] {
                k += 1;
            }
            table[i] = k;
        }
        Self {
            annotation,
            quote,
            table,
            matches: 0,
            best: None,
        }
    }
    /// True once further pages cannot change the result (empty quote or match cap reached).
    pub fn done(&self) -> bool {
        self.quote.is_empty() || self.matches >= MAX_PROPOSAL_MATCHES
    }
    pub fn page(&mut self, page: &PageText<'_>) {
        if self.done() {
            return;
        }
        // Same text rule as the viewer: characters without Unicode are skipped.
        let (text, origin): (Vec<char>, Vec<usize>) = page
            .chars
            .iter()
            .enumerate()
            .filter_map(|(i, (c, _))| c.map(|c| (c, i)))
            .unzip();
        let n = self.quote.len();
        let mut k = 0;
        for (i, c) in text.iter().enumerate() {
            while k > 0 && *c != self.quote[k] {
                k = self.table[k - 1];
            }
            if *c == self.quote[k] {
                k += 1;
            }
            if k < n {
                continue;
            }
            k = self.table[k - 1];
            let (start, end) = (i + 1 - n, i + 1);
            self.matches += 1;
            let have_context_match = self.best.as_ref().is_some_and(|b| b.context_match);
            if !have_context_match {
                let context_match = self.context_matches(&text, start, end);
                if context_match || self.best.is_none() {
                    self.best = Some(Proposal {
                        page_index: page.page_index,
                        page_label: page.page_label.clone(),
                        quads: merge_line_quads(
                            page.chars[origin[start]..=origin[end - 1]]
                                .iter()
                                .map(|(_, q)| *q),
                        ),
                        matches: 0,
                        context_match,
                    });
                }
            }
            if self.done() {
                return;
            }
        }
    }
    fn context_matches(&self, text: &[char], start: usize, end: usize) -> bool {
        let before: String = text[start.saturating_sub(MAX_CONTEXT_CHARS)..start]
            .iter()
            .collect();
        let after: String = text[end..(end + MAX_CONTEXT_CHARS).min(text.len())]
            .iter()
            .collect();
        self.annotation
            .context_before
            .as_deref()
            .is_none_or(|c| before.ends_with(c))
            && self
                .annotation
                .context_after
                .as_deref()
                .is_none_or(|c| after.starts_with(c))
    }
    pub fn finish(self) -> Option<Proposal> {
        let matches = self.matches;
        self.best.map(|best| Proposal { matches, ..best })
    }
}

/// Find the quote in the new PDF, preferring an occurrence whose surrounding text matches the
/// stored context. Returns None when the quote no longer exists. Never applied automatically.
pub fn propose_anchor(annotation: &Annotation, pages: &[PageText<'_>]) -> Option<Proposal> {
    let mut search = AnchorSearch::new(annotation);
    for page in pages {
        if search.done() {
            break;
        }
        search.page(page);
    }
    search.finish()
}
