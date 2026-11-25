//! Selection → annotation helpers (#19); persistence is tested in crates/daemon/tests.
use cm_desktop::annotations::*;
use cm_protocol::{
    annotations::{Anchor, Annotation, AnnotationId, AnnotationKind, Edit, MAX_CONTEXT_CHARS},
    attachments::{PageChar, Rect},
    domain::{AttachmentId, PaperId},
    geometry::{Origin, Point, Rotation, Viewport},
    library::ServiceError,
};

fn row(n: u8, kind: AnnotationKind, quads: Vec<Rect>) -> Annotation {
    let h = highlight(
        AttachmentId("00000000-0000-0000-0000-000000000001".parse().unwrap()),
        0,
        None,
        &chars(&[("quoted", 0.0)]),
        0,
        5,
        "yellow",
    )
    .unwrap();
    Annotation {
        id: AnnotationId(
            format!("00000000-0000-0000-0000-0000000000{n:02}")
                .parse()
                .unwrap(),
        ),
        attachment_id: h.attachment_id,
        kind,
        source: h.source,
        anchor: Anchor::Resolved {
            page_index: 0,
            page_label: None,
            quads: quads.into_iter().map(Rect::quad).collect(),
        },
        quote: h.quote,
        context_before: None,
        context_after: None,
        color: h.color,
        comment: Some("note".into()),
        confirmed_at: None,
        created_at: "t".into(),
        updated_at: "t".into(),
    }
}

#[test]
fn clicks_hit_the_topmost_annotation_and_page_notes_do_not_overlap() {
    // 200×200 pt page at zoom 1: display (x, y) = (pdf x, 200 - pdf y).
    let crop = Rect {
        left: 0.0,
        bottom: 0.0,
        right: 200.0,
        top: 200.0,
    };
    let vp = Viewport::new(crop, Rotation::R0, 1.0, Origin::TopLeft).unwrap();
    let r = |left: f32, bottom: f32, right: f32, top: f32| Rect {
        left,
        bottom,
        right,
        top,
    };
    let under = row(
        1,
        AnnotationKind::Highlight,
        vec![r(100.0, 150.0, 140.0, 160.0)],
    );
    let over = row(
        2,
        AnnotationKind::Highlight,
        vec![r(110.0, 145.0, 150.0, 158.0)],
    );
    let corner = row(
        3,
        AnnotationKind::Highlight,
        vec![r(0.0, 180.0, 40.0, 200.0)],
    );
    let first_note = row(4, AnnotationKind::Note, vec![]);
    let second_note = row(5, AnnotationKind::Note, vec![]);
    let page = [&under, &over, &corner, &first_note, &second_note];
    let at = |x: f32, y: f32| annotation_at(vp, &page, Point { x, y });
    // Overlapping highlights: the one drawn last (later in the list) wins.
    assert_eq!(at(120.0, 47.0), Some(over.id));
    assert_eq!(at(102.0, 45.0), Some(under.id));
    // Page notes stack down the margin with their own hit rectangles, above highlights.
    let (x0, y0) = marker_origin(vp, &first_note, 0);
    let (x1, y1) = marker_origin(vp, &second_note, 1);
    assert_eq!(x0, x1);
    assert!(y1 >= y0 + MARKER_H, "markers do not overlap");
    assert_eq!(at(x0 + 2.0, y0 + 2.0), Some(first_note.id));
    assert_eq!(at(x1 + 2.0, y1 + 2.0), Some(second_note.id));
    assert_eq!(
        at(39.0, 19.0),
        Some(corner.id),
        "highlight beside the marker"
    );
    assert_eq!(at(150.0, 150.0), None);
}

#[test]
fn history_steps_are_tagged_with_their_paper_and_survive_failures() {
    let paper = |n: u8| {
        PaperId(
            format!("00000000-0000-0000-0000-0000000001{n:02}")
                .parse()
                .unwrap(),
        )
    };
    let edit = |n: u8| {
        Edit::Created(row(
            n,
            AnnotationKind::Highlight,
            vec![Rect {
                left: 0.0,
                bottom: 0.0,
                right: 1.0,
                top: 1.0,
            }],
        ))
    };
    let mut h = PaperHistory::default();
    assert!(h.open(Some(paper(1))));
    assert!(h.record(paper(1), edit(1)));
    // One step in flight at a time; the buttons are disabled meanwhile.
    let (p, e) = h.begin(Step::Undo).unwrap();
    assert_eq!(p, paper(1));
    assert!(h.busy() && !h.can_undo() && h.begin(Step::Undo).is_none());
    // A failure other than a conflict keeps the step for a retry.
    assert_eq!(
        h.finish(Step::Undo, p, e, Err(ServiceError::Unavailable)),
        StepOutcome::Retained
    );
    assert!(h.can_undo());
    // A request that could not be queued puts the step back too.
    let (_, e) = h.begin(Step::Undo).unwrap();
    h.unsent(Step::Undo, e);
    assert!(h.can_undo() && !h.busy());
    // Success moves it to redo; a conflict drops it.
    let (p, e) = h.begin(Step::Undo).unwrap();
    assert_eq!(h.finish(Step::Undo, p, e, Ok(())), StepOutcome::Applied);
    assert!(h.can_redo() && !h.can_undo());
    let (p, e) = h.begin(Step::Redo).unwrap();
    assert_eq!(
        h.finish(Step::Redo, p, e, Err(ServiceError::Conflict)),
        StepOutcome::Skipped
    );
    assert!(!h.can_redo() && !h.can_undo());
    // Paper switch while a step is in flight: the late reply and late edits are ignored.
    h.record(paper(1), edit(2));
    let (p, e) = h.begin(Step::Undo).unwrap();
    assert!(h.open(Some(paper(2))));
    assert!(!h.busy());
    assert_eq!(h.finish(Step::Undo, p, e, Ok(())), StepOutcome::Ignored);
    assert!(!h.record(paper(1), edit(3)));
    assert!(!h.can_undo() && !h.can_redo());
    assert!(!h.open(Some(paper(2))), "same paper keeps its history");
}

fn chars(lines: &[(&str, f32)]) -> Vec<PageChar> {
    let mut out = vec![];
    for (text, bottom) in lines {
        for (i, c) in text.chars().enumerate() {
            out.push(PageChar {
                unicode: Some(c),
                quad: Rect {
                    left: 10.0 + i as f32 * 6.0,
                    bottom: *bottom,
                    right: 16.0 + i as f32 * 6.0,
                    top: bottom + 10.0,
                }
                .quad(),
            });
        }
    }
    out
}

#[test]
fn multi_line_selection_becomes_one_quad_per_line_with_quote_and_context() {
    let page = chars(&[("前置き Hello", 100.0), ("world 後書き", 80.0)]);
    let att = AttachmentId("00000000-0000-0000-0000-000000000001".parse().unwrap());
    // "Hello" + "world" (indices 4..=13 span the line break).
    let a = highlight(att, 3, Some("iv".into()), &page, 4, 13, "green").unwrap();
    assert_eq!(a.quote, "Helloworld");
    let Anchor::Resolved {
        page_index,
        page_label,
        quads,
    } = &a.anchor
    else {
        panic!()
    };
    assert_eq!((*page_index, page_label.as_deref()), (3, Some("iv")));
    assert_eq!(quads.len(), 2, "one rectangle per line");
    let first = quads[0].bounds();
    assert_eq!((first.left, first.right), (34.0, 64.0));
    assert_eq!(a.context_before.as_deref(), Some("前置き "));
    assert_eq!(a.context_after.as_deref(), Some(" 後書き"));
    assert_eq!(a.color.as_deref(), Some("green"));
    // Out-of-range or whitespace-only selections create nothing.
    assert!(highlight(att, 0, None, &page, 5, 99, "yellow").is_none());
    assert!(highlight(att, 0, None, &page, 3, 3, "yellow").is_none());
    assert!(highlight(att, 0, None, &[], 0, 0, "yellow").is_none());
}

#[test]
fn context_is_bounded_and_colors_map_safely() {
    let long = "x".repeat(200);
    let page = chars(&[(&long, 0.0)]);
    let (before, after) = context(&page, 100, 101);
    assert_eq!(before.unwrap().chars().count(), MAX_CONTEXT_CHARS);
    assert_eq!(after.unwrap().chars().count(), MAX_CONTEXT_CHARS);
    assert_eq!(context(&page, 0, 199), (None, None));
    assert_eq!(rgb(Some("#102030")), 0x102030);
    assert_eq!(rgb(Some("bogus")), rgb(None));
}

#[test]
fn a_hit_or_link_selects_its_annotation_only_once_the_list_has_arrived() {
    let list = [
        row(1, AnnotationKind::Highlight, vec![]),
        row(2, AnnotationKind::Note, vec![]),
    ];
    let wanted = list[1].id;
    assert_eq!(pending_selection(None, Some(&list)), PendingSelection::Idle);
    // Before the paper's annotation list arrives, nothing may be selected: the comment
    // editor would show a stale comment that Save would write over the stored one.
    assert_eq!(
        pending_selection(Some(wanted), None),
        PendingSelection::Wait
    );
    // An empty list that has arrived is a real answer, not "still loading".
    assert_eq!(
        pending_selection(Some(wanted), Some(&[])),
        PendingSelection::Missing(wanted)
    );
    assert_eq!(
        pending_selection(Some(wanted), Some(&list)),
        PendingSelection::Select(wanted)
    );
    let other = row(9, AnnotationKind::Highlight, vec![]).id;
    assert_eq!(
        pending_selection(Some(other), Some(&list)),
        PendingSelection::Missing(other)
    );
}
