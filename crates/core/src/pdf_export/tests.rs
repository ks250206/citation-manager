use super::*;
use crate::annotations::Source;
use cm_domain::AttachmentId;

fn rect(left: f32, bottom: f32, right: f32, top: f32) -> Rect {
    Rect {
        left,
        bottom,
        right,
        top,
    }
}
fn id(n: u128) -> AnnotationId {
    AnnotationId(uuid::Uuid::from_u128(n))
}
fn annotation(n: u128, kind: AnnotationKind, anchor: Anchor, comment: Option<&str>) -> Annotation {
    Annotation {
        id: id(n),
        attachment_id: AttachmentId(uuid::Uuid::from_u128(99)),
        kind,
        source: Source::Human,
        anchor,
        quote: "q".into(),
        context_before: None,
        context_after: None,
        color: Some("blue".into()),
        comment: comment.map(str::to_owned),
        confirmed_at: None,
        created_at: String::new(),
        updated_at: String::new(),
    }
}
fn resolved(page_index: u32, quads: Vec<Quad>) -> Anchor {
    Anchor::Resolved {
        page_index,
        page_label: None,
        quads,
    }
}

#[test]
fn plan_maps_highlights_and_notes_and_lists_every_omission() {
    let lines = vec![
        rect(40., 400., 300., 412.).quad(),
        rect(40., 380., 200., 392.).quad(),
    ];
    let mut skewed = rect(40., 300., 80., 310.).quad();
    skewed.0[1].y += 3.;
    let annotations = vec![
        annotation(
            1,
            AnnotationKind::Highlight,
            resolved(2, lines.clone()),
            Some("コメント αβ"),
        ),
        annotation(
            2,
            AnnotationKind::Note,
            resolved(0, vec![]),
            Some("独立ノート"),
        ),
        annotation(
            3,
            AnnotationKind::Highlight,
            Anchor::Unresolved {
                reason: "Copied from a replaced PDF".into(),
            },
            None,
        ),
        annotation(
            4,
            AnnotationKind::Highlight,
            resolved(0, vec![skewed]),
            None,
        ),
        annotation(
            5,
            AnnotationKind::Note,
            resolved(1, vec![rect(100., 200., 150., 210.).quad()]),
            Some("anchored"),
        ),
    ];
    let plan = plan(&annotations);
    assert_eq!(plan.highlights.len(), 1);
    let h = &plan.highlights[0];
    assert_eq!(
        (h.id, h.page_index, &h.quads, h.contents.as_str()),
        (id(1), 2, &lines, "コメント αβ")
    );
    assert_eq!(h.color, [0x6f, 0xa8, 0xff]);
    assert_eq!(h.opacity, HIGHLIGHT_OPACITY);
    assert_eq!(plan.notes.len(), 2);
    assert_eq!(plan.notes[0].at, None);
    assert_eq!(plan.notes[1].at, Some(rect(100., 186., 124., 210.)));
    assert_eq!(
        plan.omitted.iter().map(|o| o.id).collect::<Vec<_>>(),
        vec![id(3), id(4)]
    );
    assert!(
        plan.omitted[0]
            .reason
            .contains("Copied from a replaced PDF")
    );
    assert!(!plan.is_empty());
}

#[test]
fn shared_palette_matches_named_and_hex_colours() {
    assert_eq!(rgb(None), [0xff, 0xd8, 0x4d]);
    assert_eq!(rgb(Some("green")), [0x7b, 0xd8, 0x8f]);
    assert_eq!(rgb(Some("#102030")), [0x10, 0x20, 0x30]);
    assert_eq!(rgb(Some("unknown")), rgb(None));
}

#[test]
fn note_slots_start_at_the_visible_top_left_for_every_rotation() {
    let crop = rect(20., 30., 380., 470.);
    // Visible top-left in unrotated PDF space per clockwise /Rotate.
    for (rotation, corner) in [
        (Rotation::R0, (20., 470.)),
        (Rotation::R90, (20., 30.)),
        (Rotation::R180, (380., 30.)),
        (Rotation::R270, (380., 470.)),
    ] {
        let slot = note_slot(crop, rotation, 0).unwrap();
        assert_eq!((slot.width(), slot.height()), (NOTE_ICON, NOTE_ICON));
        let dx = (slot.left - corner.0)
            .abs()
            .min((slot.right - corner.0).abs());
        let dy = (slot.bottom - corner.1)
            .abs()
            .min((slot.top - corner.1).abs());
        assert!(
            (dx - NOTE_MARGIN).abs() < 1e-3 && (dy - NOTE_MARGIN).abs() < 1e-3,
            "{rotation:?} {slot:?}"
        );
        assert!(crop.contains(Point {
            x: slot.left,
            y: slot.bottom
        }));
        assert!(crop.contains(Point {
            x: slot.right,
            y: slot.top
        }));
        let next = note_slot(crop, rotation, 1).unwrap();
        assert!(next.intersection(slot).is_none());
    }
    assert!(note_slot(rect(0., 0., 50., 50.), Rotation::R0, 0).is_none());
    // Every slot of a full page keeps one free icon size towards the far edges.
    for slot in 0..200 {
        let r = note_slot(crop, Rotation::R0, slot).unwrap();
        assert!(r.left >= crop.left + NOTE_MARGIN && r.right <= crop.right - NOTE_ICON);
        assert!(r.bottom >= crop.bottom + NOTE_ICON && r.top <= crop.top - NOTE_MARGIN);
    }
}

fn read(
    page_index: u32,
    subtype: &str,
    name: Option<String>,
    contents: &str,
    rect: Rect,
    quads: Vec<Quad>,
    fill: [u8; 4],
) -> ReadAnnotation {
    ReadAnnotation {
        page_index,
        subtype: subtype.into(),
        name,
        contents: Some(contents.into()),
        rect,
        quads,
        appearance: Some(ReadAppearance {
            objects: 1,
            fill: Some(fill),
        }),
    }
}

fn fixture() -> (
    ExportPlan,
    Vec<Rect>,
    Vec<ReadAnnotation>,
    Vec<ReadAnnotation>,
) {
    let quads = vec![
        rect(40., 400., 300., 412.).quad(),
        rect(40., 380., 200., 392.).quad(),
    ];
    let plan = ExportPlan {
        highlights: vec![PlannedHighlight {
            id: id(1),
            page_index: 0,
            quads: quads.clone(),
            color: [255, 255, 0],
            opacity: 0.5,
            contents: "検証 café".into(),
        }],
        notes: vec![PlannedNote {
            id: id(2),
            page_index: 0,
            at: None,
            color: [255, 204, 0],
            contents: "ノート".into(),
        }],
        omitted: vec![],
    };
    let placed = vec![rect(28., 438., 52., 462.)];
    let existing = read(
        0,
        "Square",
        None,
        "old",
        rect(1., 1., 9., 9.),
        vec![],
        [0, 0, 255, 255],
    );
    let before = vec![existing.clone()];
    let after = vec![
        existing,
        read(
            0,
            "Highlight",
            Some(pdf_name(id(1))),
            "検証 café",
            rect(40., 380., 300., 412.),
            quads,
            [255, 255, 0, 128],
        ),
        read(
            0,
            "Text",
            Some(pdf_name(id(2))),
            "ノート",
            placed[0],
            vec![],
            [255, 204, 0, 255],
        ),
    ];
    (plan, placed, before, after)
}

#[test]
fn verify_accepts_a_faithful_export_within_tolerance() {
    let (plan, placed, before, mut after) = fixture();
    after[1].quads[0].0[0].x += 0.005;
    let v = verify(&plan, &placed, &before, &after).unwrap();
    assert_eq!((v.highlights, v.notes, v.preserved), (1, 1, 1));
    assert!(v.max_position_error <= POSITION_TOLERANCE);
}

#[test]
fn verify_reports_every_mismatch_instead_of_success() {
    let check = |edit: &dyn Fn(&mut Vec<ReadAnnotation>), needle: &str| {
        let (plan, placed, before, mut after) = fixture();
        edit(&mut after);
        let problems = verify(&plan, &placed, &before, &after).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains(needle)),
            "{needle}: {problems:?}"
        );
    };
    check(&|a| a[1].quads[1].0[3].y += 0.5, "QuadPoints");
    check(&|a| a[1].contents = Some("other".into()), "comment");
    check(&|a| a[1].appearance = None, "no persistent appearance");
    check(
        &|a| a[1].appearance.as_mut().unwrap().fill = Some([255, 255, 0, 255]),
        "colour/opacity",
    );
    check(&|a| a[2].rect.left += 3., "position");
    check(&|a| a[2].subtype = "FreeText".into(), "written as FreeText");
    check(&|a| a[0].contents = Some("recoloured".into()), "preserved");
    check(&|a| a[0].appearance = None, "preserved");
    check(&|a| drop(a.remove(2)), "missing");
    check(
        &|a| {
            let extra = a[2].clone();
            a.push(extra)
        },
        "Expected 2",
    );
    check(
        &|a| {
            let mut extra = a[2].clone();
            extra.name = None;
            a.push(extra)
        },
        "Unexpected Text",
    );
    check(&|a| a[1].page_index = 1, "written as Highlight on page 2");
}
