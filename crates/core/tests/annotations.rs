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
    assert_eq!(proposal.quads.len(), 3);
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
