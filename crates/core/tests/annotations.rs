//! #19 service rules with an in-memory repository (SQLite behaviour: crates/storage tests).
use cm_core::{
    ServiceError,
    annotations::*,
    geometry::{Quad, Rect},
};
use cm_domain::{AttachmentId, PaperId};
use std::collections::BTreeMap;

#[derive(Default)]
struct Memory {
    rows: BTreeMap<String, Annotation>,
    attachments: Vec<AttachmentId>,
    clock: std::cell::Cell<u32>,
}
impl AnnotationRepository for &mut Memory {
    fn now(&self) -> Result<String, ServiceError> {
        self.clock.set(self.clock.get() + 1);
        Ok(format!("2026-01-01T00:00:{:02}Z", self.clock.get()))
    }
    fn insert(&mut self, a: &Annotation) -> Result<(), ServiceError> {
        self.rows.insert(a.id.0.to_string(), a.clone());
        Ok(())
    }
    fn insert_all(&mut self, rows: &[Annotation]) -> Result<(), ServiceError> {
        for a in rows {
            self.insert(a)?;
        }
        Ok(())
    }
    fn replace(&mut self, a: &Annotation) -> Result<(), ServiceError> {
        self.rows
            .get_mut(&a.id.0.to_string())
            .map(|row| *row = a.clone())
            .ok_or(ServiceError::NotFound)
    }
    fn delete(&mut self, id: AnnotationId) -> Result<bool, ServiceError> {
        Ok(self.rows.remove(&id.0.to_string()).is_some())
    }
    fn get(&self, id: AnnotationId) -> Result<Option<Annotation>, ServiceError> {
        Ok(self.rows.get(&id.0.to_string()).cloned())
    }
    fn for_attachment(&self, id: AttachmentId) -> Result<Vec<Annotation>, ServiceError> {
        Ok(self
            .rows
            .values()
            .filter(|a| a.attachment_id == id)
            .cloned()
            .collect())
    }
    fn for_paper(&self, _: PaperId) -> Result<Vec<Annotation>, ServiceError> {
        Ok(self.rows.values().cloned().collect())
    }
    fn attachment_exists(&self, id: AttachmentId) -> Result<bool, ServiceError> {
        Ok(self.attachments.contains(&id))
    }
}

fn attachment(n: u8) -> AttachmentId {
    AttachmentId(
        format!("00000000-0000-0000-0000-0000000000{n:02}")
            .parse()
            .unwrap(),
    )
}
fn quad(left: f32) -> Quad {
    Rect {
        left,
        bottom: 10.0,
        right: left + 5.0,
        top: 20.0,
    }
    .quad()
}
fn highlight(att: AttachmentId) -> NewAnnotation {
    NewAnnotation {
        attachment_id: att,
        kind: AnnotationKind::Highlight,
        source: Source::Human,
        anchor: Anchor::Resolved {
            page_index: 2,
            page_label: Some("iii".into()),
            quads: vec![quad(0.0)],
        },
        quote: "日本語".into(),
        context_before: Some("before ".into()),
        context_after: Some(" after".into()),
        color: Some("yellow".into()),
        comment: None,
    }
}
fn created(edit: Edit) -> Annotation {
    match edit {
        Edit::Created(a) => a,
        other => panic!("{other:?}"),
    }
}

#[test]
fn validation_rejects_bad_input_and_unknown_attachments() {
    let mut memory = Memory {
        attachments: vec![attachment(1)],
        ..Default::default()
    };
    let mut service = AnnotationService::new(&mut memory);
    let mut bad = highlight(attachment(1));
    bad.color = Some("red; DROP".into());
    assert!(matches!(
        service.create(bad),
        Err(ServiceError::InvalidInput(_))
    ));
    let mut bad = highlight(attachment(1));
    bad.anchor = Anchor::Resolved {
        page_index: 0,
        page_label: None,
        quads: vec![],
    };
    assert!(service.create(bad).is_err(), "highlight without quads");
    let mut bad = highlight(attachment(1));
    bad.quote = " ".into();
    assert!(service.create(bad).is_err(), "highlight without text");
    let mut bad = highlight(attachment(1));
    bad.kind = AnnotationKind::Note;
    assert!(service.create(bad).is_err(), "note without text");
    let mut bad = highlight(attachment(1));
    bad.context_before = Some("x".repeat(MAX_CONTEXT_CHARS + 1));
    assert!(service.create(bad).is_err());
    let mut bad = highlight(attachment(1));
    bad.anchor = Anchor::Unresolved { reason: " ".into() };
    assert!(service.create(bad).is_err());
    assert_eq!(
        service.create(highlight(attachment(9))).unwrap_err(),
        ServiceError::NotFound
    );
    assert!(valid_color("#a1B2c3") && !valid_color("#12345") && !valid_color("#12345g"));
}

#[test]
fn undo_redo_replays_the_same_operations_and_refuses_stale_state() {
    let mut memory = Memory {
        attachments: vec![attachment(1)],
        ..Default::default()
    };
    let mut history = History::default();
    let mut service = AnnotationService::new(&mut memory);
    let edit = service.create(highlight(attachment(1))).unwrap();
    let a = created(edit.clone());
    history.record(edit);
    let edit = service
        .update(
            a.id,
            AnnotationPatch {
                color: Some(Some("green".into())),
                comment: Some(Some("important".into())),
            },
        )
        .unwrap();
    history.record(edit);
    assert_eq!(
        service.get(a.id).unwrap().unwrap().comment.as_deref(),
        Some("important")
    );
    // Undo the update, then the create.
    let e = history.take_undo().unwrap();
    service.undo(&e).unwrap();
    history.undone(e);
    let restored = service.get(a.id).unwrap().unwrap();
    assert_eq!(restored, a, "exact row restored, including timestamps");
    let e = history.take_undo().unwrap();
    service.undo(&e).unwrap();
    history.undone(e);
    assert!(service.get(a.id).unwrap().is_none());
    assert!(!history.can_undo() && history.can_redo());
    // Redo both, in order.
    let e = history.take_redo().unwrap();
    service.redo(&e).unwrap();
    history.redone(e);
    assert_eq!(service.get(a.id).unwrap().unwrap(), a);
    let e = history.take_redo().unwrap();
    service.redo(&e).unwrap();
    history.redone(e);
    assert_eq!(
        service.get(a.id).unwrap().unwrap().color.as_deref(),
        Some("green")
    );
    // Someone else edits; undoing our update must not clobber it.
    service
        .update(
            a.id,
            AnnotationPatch {
                comment: Some(Some("theirs".into())),
                color: None,
            },
        )
        .unwrap();
    let e = history.take_undo().unwrap();
    assert_eq!(service.undo(&e).unwrap_err(), ServiceError::Conflict);
    // Delete then undo restores the same id.
    let deleted = service.delete(a.id).unwrap();
    service.undo(&deleted).unwrap();
    assert!(service.get(a.id).unwrap().is_some());
    // A new edit clears redo.
    history.undone(deleted.clone());
    history.record(deleted);
    assert!(!history.can_redo());
}

#[test]
fn ai_annotations_keep_provenance_and_need_confirmation() {
    let mut memory = Memory {
        attachments: vec![attachment(1)],
        ..Default::default()
    };
    let mut service = AnnotationService::new(&mut memory);
    let mut input = highlight(attachment(1));
    input.source = Source::Ai;
    let ai = created(service.create(input).unwrap());
    assert!(ai.confirmed_at.is_none());
    let Edit::Updated { after, .. } = service.confirm_ai(ai.id).unwrap() else {
        panic!()
    };
    assert_eq!(after.source, Source::Ai);
    assert!(after.confirmed_at.is_some());
    let human = created(service.create(highlight(attachment(1))).unwrap());
    assert!(service.confirm_ai(human.id).is_err());
    // Rejecting is a normal, undoable delete.
    let rejected = service.delete(ai.id).unwrap();
    service.undo(&rejected).unwrap();
    assert_eq!(service.get(ai.id).unwrap().unwrap().source, Source::Ai);
}

#[test]
fn replaced_pdf_carries_annotations_unresolved_and_proposes_positions() {
    let mut memory = Memory {
        attachments: vec![attachment(1), attachment(2)],
        ..Default::default()
    };
    let mut service = AnnotationService::new(&mut memory);
    let original = created(service.create(highlight(attachment(1))).unwrap());
    assert!(service.carry_over(attachment(1), attachment(1)).is_err());
    let edits = service.carry_over(attachment(1), attachment(2)).unwrap();
    let copy = created(edits[0].clone());
    assert_eq!(copy.attachment_id, attachment(2));
    assert!(matches!(copy.anchor, Anchor::Unresolved { .. }));
    assert_eq!(copy.quote, original.quote);
    // New PDF: the quote occurs twice; only the second has matching context.
    let text = |s: &str, base: f32| -> Vec<(Option<char>, Quad)> {
        s.chars()
            .enumerate()
            .map(|(i, c)| (Some(c), quad(base + i as f32 * 5.0)))
            .collect()
    };
    let p0 = text("x 日本語 y", 0.0);
    let p1 = text("before 日本語 after", 100.0);
    let pages = [
        PageText {
            page_index: 0,
            page_label: None,
            chars: &p0,
        },
        PageText {
            page_index: 5,
            page_label: Some("6".into()),
            chars: &p1,
        },
    ];
    let proposal = propose_anchor(&copy, &pages).unwrap();
    assert_eq!(
        (
            proposal.page_index,
            proposal.matches,
            proposal.context_match
        ),
        (5, 2, true)
    );
    assert_eq!(
        proposal.quads.len(),
        1,
        "one quad per line, as the viewer stores"
    );
    // Nothing moved until the person confirms.
    assert!(matches!(
        service.get(copy.id).unwrap().unwrap().anchor,
        Anchor::Unresolved { .. }
    ));
    service
        .resolve(
            copy.id,
            proposal.page_index,
            proposal.page_label,
            proposal.quads,
        )
        .unwrap();
    assert!(matches!(
        service.get(copy.id).unwrap().unwrap().anchor,
        Anchor::Resolved { page_index: 5, .. }
    ));
    // Quote gone from the new PDF: no proposal.
    let empty = text("nothing here", 0.0);
    let none = [PageText {
        page_index: 0,
        page_label: None,
        chars: &empty,
    }];
    assert!(propose_anchor(&copy, &none).is_none());
}

fn ai(service: &mut AnnotationService<&mut Memory>) -> Annotation {
    let mut input = highlight(attachment(1));
    input.source = Source::Ai;
    created(service.create(input).unwrap())
}
fn invalid(r: Result<(), ServiceError>) -> bool {
    matches!(r, Err(ServiceError::InvalidInput(_)))
}

#[test]
fn redo_and_undo_validate_client_supplied_rows() {
    let mut memory = Memory {
        attachments: vec![attachment(1)],
        ..Default::default()
    };
    let mut service = AnnotationService::new(&mut memory);
    let human = created(service.create(highlight(attachment(1))).unwrap());

    // A forged "created" AI row that claims a confirmation nobody gave.
    let mut forged = human.clone();
    forged.id = AnnotationId(uuid::Uuid::now_v7());
    forged.source = Source::Ai;
    forged.confirmed_at = Some("2026-01-01T00:00:59Z".into());
    assert!(invalid(service.redo(&Edit::Created(forged.clone()))));
    assert!(service.get(forged.id).unwrap().is_none());
    // Confirmation on a non-AI row is never valid.
    let mut human_confirmed = forged.clone();
    human_confirmed.source = Source::Human;
    assert!(invalid(service.redo(&Edit::Created(human_confirmed))));
    // NaN geometry would make the stored JSON unreadable for the whole paper.
    let mut nan = human.clone();
    nan.id = AnnotationId(uuid::Uuid::now_v7());
    nan.anchor = Anchor::Resolved {
        page_index: 0,
        page_label: None,
        quads: vec![
            Rect {
                left: f32::NAN,
                bottom: 0.0,
                right: 1.0,
                top: 1.0,
            }
            .quad(),
        ],
    };
    assert!(invalid(service.redo(&Edit::Created(nan.clone()))));
    let mut input = highlight(attachment(1));
    input.anchor = nan.anchor.clone();
    assert!(service.create(input).is_err());
    // Other field rules apply to replayed rows too.
    let mut bad_color = human.clone();
    bad_color.id = AnnotationId(uuid::Uuid::now_v7());
    bad_color.color = Some("url(x)".into());
    assert!(invalid(service.redo(&Edit::Created(bad_color))));

    // Updates may not change identity, quoted text or provenance...
    let mut moved = human.clone();
    moved.quote = "rewritten".into();
    assert!(invalid(service.redo(&Edit::Updated {
        before: human.clone(),
        after: moved,
    })));
    let mut promoted = human.clone();
    promoted.source = Source::Ai;
    promoted.confirmed_at = Some("2026-01-01T00:00:58Z".into());
    assert!(invalid(service.redo(&Edit::Updated {
        before: human.clone(),
        after: promoted,
    })));
    let mut other = human.clone();
    other.id = AnnotationId(uuid::Uuid::now_v7());
    assert!(invalid(service.redo(&Edit::Updated {
        before: human.clone(),
        after: other,
    })));
    // ...nor invent a confirmation for an existing AI row.
    let pending = ai(&mut service);
    let mut confirmed = pending.clone();
    confirmed.confirmed_at = Some("2026-01-01T00:00:57Z".into());
    assert!(invalid(service.redo(&Edit::Updated {
        before: pending.clone(),
        after: confirmed,
    })));
    assert_eq!(service.get(pending.id).unwrap().unwrap(), pending);
    assert_eq!(service.get(human.id).unwrap().unwrap(), human);
}

#[test]
fn confirmed_ai_rows_round_trip_through_undo_and_redo_exactly() {
    let mut memory = Memory {
        attachments: vec![attachment(1)],
        ..Default::default()
    };
    let mut service = AnnotationService::new(&mut memory);
    let a = ai(&mut service);
    // Confirm -> undo -> redo restores the server's own confirmation.
    let confirm = service.confirm_ai(a.id).unwrap();
    service.undo(&confirm).unwrap();
    assert!(service.get(a.id).unwrap().unwrap().confirmed_at.is_none());
    service.redo(&confirm).unwrap();
    let Edit::Updated { after, .. } = &confirm else {
        panic!()
    };
    assert_eq!(&service.get(a.id).unwrap().unwrap(), after);
    assert!(service.confirm_ai(a.id).is_err(), "already confirmed");
    // Remove a confirmed AI row -> undo brings it back confirmed; redo/undo again.
    let removed = service.delete(a.id).unwrap();
    service.undo(&removed).unwrap();
    assert_eq!(&service.get(a.id).unwrap().unwrap(), after);
    service.redo(&removed).unwrap();
    service.undo(&removed).unwrap();
    // A long-lived writer keeps the ledger across service instances.
    let ledger = service.into_confirmations();
    let mut service = AnnotationService::with_confirmations(&mut memory, ledger);
    let removed = service.delete(a.id).unwrap();
    let ledger = service.into_confirmations();
    let mut service = AnnotationService::with_confirmations(&mut memory, ledger);
    service.undo(&removed).unwrap();
    // A fresh ledger knows nothing: the same restore is refused.
    let removed = service.delete(a.id).unwrap();
    let mut service = AnnotationService::new(&mut memory);
    assert!(invalid(service.undo(&removed)));
}

#[test]
fn bounds_note_text_and_resolve_rules() {
    let mut memory = Memory {
        attachments: vec![attachment(1)],
        ..Default::default()
    };
    let mut service = AnnotationService::new(&mut memory);
    let page = |page_index: u32, label: Option<String>| Anchor::Resolved {
        page_index,
        page_label: label,
        quads: vec![quad(0.0)],
    };
    let mut input = highlight(attachment(1));
    input.anchor = page(u32::MAX, None);
    assert!(service.create(input).is_err(), "beyond the display range");
    let mut input = highlight(attachment(1));
    input.anchor = page(MAX_PAGE_INDEX, None);
    assert!(service.create(input).is_ok());
    let mut input = highlight(attachment(1));
    input.anchor = page(0, Some("x".repeat(MAX_PAGE_LABEL_BYTES + 1)));
    assert!(service.create(input).is_err());
    let mut input = highlight(attachment(1));
    input.anchor = Anchor::Unresolved {
        reason: "r".repeat(MAX_REASON_BYTES + 1),
    };
    assert!(service.create(input).is_err());
    // A note needs text wherever it is anchored, and whitespace is no text.
    let note = |anchor: Anchor, comment: &str| NewAnnotation {
        kind: AnnotationKind::Note,
        anchor,
        quote: String::new(),
        context_before: None,
        context_after: None,
        comment: Some(comment.into()),
        ..highlight(attachment(1))
    };
    let unresolved = Anchor::Unresolved {
        reason: "moved".into(),
    };
    assert!(service.create(note(unresolved.clone(), "  ")).is_err());
    let n = created(service.create(note(unresolved, "keep")).unwrap());
    let blank = AnnotationPatch {
        comment: Some(Some(" \n ".into())),
        color: None,
    };
    assert!(service.update(n.id, blank.clone()).is_err());
    // Whitespace comments on highlights are stored as none.
    let h = created(service.create(highlight(attachment(1))).unwrap());
    let Edit::Updated { after, .. } = service.update(h.id, blank).unwrap() else {
        panic!()
    };
    assert_eq!(after.comment, None);
    // Resolve only places unresolved annotations.
    assert!(service.resolve(h.id, 0, None, vec![quad(1.0)]).is_err());
    service.resolve(n.id, 0, None, vec![]).unwrap();
}

#[test]
fn carry_over_is_idempotent_and_keeps_provenance() {
    let mut memory = Memory {
        attachments: vec![attachment(1), attachment(2)],
        ..Default::default()
    };
    let mut service = AnnotationService::new(&mut memory);
    created(service.create(highlight(attachment(1))).unwrap());
    let a = ai(&mut service);
    service.confirm_ai(a.id).unwrap();
    let note = |text: &str| NewAnnotation {
        kind: AnnotationKind::Note,
        anchor: Anchor::Resolved {
            page_index: 0,
            page_label: None,
            quads: vec![],
        },
        quote: String::new(),
        context_before: None,
        context_after: None,
        comment: Some(text.into()),
        ..highlight(attachment(1))
    };
    service.create(note("first")).unwrap();
    service.create(note("second")).unwrap();
    let first = service.carry_over(attachment(1), attachment(2)).unwrap();
    assert_eq!(first.len(), 4, "two distinct notes are both copied");
    let copies = service.list(attachment(2)).unwrap();
    let ai_copies: Vec<_> = copies.iter().filter(|c| c.source == Source::Ai).collect();
    assert_eq!(ai_copies.len(), 1);
    assert!(ai_copies[0].confirmed_at.is_some(), "confirmation kept");
    // Second run copies nothing.
    assert!(
        service
            .carry_over(attachment(1), attachment(2))
            .unwrap()
            .is_empty()
    );
    assert_eq!(service.list(attachment(2)).unwrap().len(), 4);
    let from = service.list(attachment(1)).unwrap();
    let to = service.list(attachment(2)).unwrap();
    assert!(pending_carry_over(&from, &to).is_empty());
    // A new annotation on the old PDF is the only one left to copy.
    service.create(note("third")).unwrap();
    let from = service.list(attachment(1)).unwrap();
    assert_eq!(pending_carry_over(&from, &to).len(), 1);
    assert_eq!(
        service
            .carry_over(attachment(1), attachment(2))
            .unwrap()
            .len(),
        1
    );
    // Undo and redo of a copied confirmed row restore it exactly.
    let copy = first
        .iter()
        .find(|e| matches!(e, Edit::Created(c) if c.source == Source::Ai))
        .unwrap()
        .clone();
    service.undo(&copy).unwrap();
    service.redo(&copy).unwrap();
}

#[test]
fn proposals_skip_unmapped_characters_and_cap_matches() {
    let mut memory = Memory {
        attachments: vec![attachment(1)],
        ..Default::default()
    };
    let mut service = AnnotationService::new(&mut memory);
    let mut input = highlight(attachment(1));
    input.quote = "ab".into();
    input.context_before = None;
    input.context_after = None;
    let a = created(service.create(input).unwrap());
    // The viewer drops characters without Unicode from quotes; so does the search.
    let chars: Vec<(Option<char>, Quad)> = vec![
        (Some('x'), quad(0.0)),
        (Some('a'), quad(5.0)),
        (None, quad(10.0)),
        (Some('b'), quad(15.0)),
    ];
    let page = PageText {
        page_index: 0,
        page_label: None,
        chars: &chars,
    };
    let p = propose_anchor(&a, &[page]).unwrap();
    assert_eq!(p.matches, 1);
    let b = p.quads[0].bounds();
    assert_eq!((b.left, b.right), (5.0, 20.0));
    // Pathological repetition stops at the cap instead of collecting every match.
    let many: Vec<(Option<char>, Quad)> = (0..(MAX_PROPOSAL_MATCHES * 2 + 10))
        .map(|i| (Some(if i % 2 == 0 { 'a' } else { 'b' }), quad(i as f32)))
        .collect();
    let page = PageText {
        page_index: 3,
        page_label: None,
        chars: &many,
    };
    let p = propose_anchor(&a, &[page]).unwrap();
    assert_eq!((p.page_index, p.matches), (3, MAX_PROPOSAL_MATCHES));
}
