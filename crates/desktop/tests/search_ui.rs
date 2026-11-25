//! #21 pure search presentation, deep links and in-PDF find.
use cm_desktop::{
    deep_links::{DeepLink, Target},
    search::*,
};
use cm_protocol::{
    attachments::{PageChar, Rect},
    domain::{AttachmentId, CollectionId, PaperId},
    library_query::PaperVisibility,
    search::{
        DocumentSource, ScanStatus, SearchCoverage, SearchHit, ShortTermScope, SnippetRange,
        Unresolved, UnsearchedScope,
    },
};

fn id(n: u8) -> uuid::Uuid {
    format!("00000000-0000-0000-0000-0000000000{n:02}")
        .parse()
        .unwrap()
}
fn hit(source: DocumentSource, page: Option<u32>, label: Option<&str>) -> SearchHit {
    SearchHit {
        paper_id: PaperId(id(1)),
        paper_title: "T".into(),
        doi: None,
        deleted: false,
        source,
        attachment_id: page.map(|_| AttachmentId(id(2))),
        annotation_id: None,
        page_index: page,
        page_label: label.map(Into::into),
        matched_fields: vec![],
        snippet_field: None,
        snippet: "…the attention mechanism…".into(),
        // Byte offsets: '…' is 3 bytes, so "attention" is bytes 7..16.
        highlights: vec![SnippetRange { start: 7, end: 16 }],
        snippet_truncated: false,
        score: 1.0,
    }
}

#[test]
fn requests_keep_the_sidebar_scope() {
    let r = request("  attention ", Some(CollectionId(id(3))), true, true);
    assert_eq!(r.query, "attention");
    assert_eq!(r.collection, Some(CollectionId(id(3))));
    assert_eq!(r.visibility, PaperVisibility::Deleted);
    assert_eq!(r.short_term_scope, ShortTermScope::AllPages);
    assert!(r.validate().is_ok());
    assert_eq!(
        request("x", None, false, false).short_term_scope,
        ShortTermScope::Bounded
    );
}

#[test]
fn labels_snippets_and_focus_text() {
    assert_eq!(
        source_label(&hit(DocumentSource::Page, Some(4), Some("iv"))),
        "p. 5 (label iv)"
    );
    assert_eq!(
        source_label(&hit(DocumentSource::Page, Some(4), Some("5"))),
        "p. 5"
    );
    assert_eq!(
        source_label(&hit(DocumentSource::Paper, None, None)),
        "Metadata"
    );
    let h = hit(DocumentSource::Page, Some(0), None);
    let runs = snippet_runs(&h.snippet, &h.highlights);
    assert_eq!(
        runs.iter()
            .filter(|r| r.1)
            .map(|r| r.0.as_str())
            .collect::<String>(),
        "attention"
    );
    assert_eq!(focus_text(&h).as_deref(), Some("attention"));
    // Out-of-range highlight ranges are clamped.
    let runs = snippet_runs("日本語", &[SnippetRange { start: 3, end: 99 }]);
    assert_eq!(runs, vec![("日".into(), false), ("本語".into(), true)]);
}

#[test]
fn coverage_never_hides_unsearched_areas() {
    let coverage = SearchCoverage {
        complete: false,
        fts_truncated: true,
        timed_out: false,
        short_terms: vec!["AI".into()],
        scan: ScanStatus::Partial,
        scanned_documents: 512,
        unsearched: vec![
            UnsearchedScope::PdfPagesForShortTerms,
            UnsearchedScope::PendingExtraction { attachments: 2 },
            UnsearchedScope::NoExtractableText { attachments: 1 },
        ],
        continuation: None,
        unresolved: vec![Unresolved::Tag("nope".into())],
    };
    let notes = coverage_notes(&coverage).join("\n");
    for needle in [
        "AI",
        "512",
        "PDF text was not searched",
        "2 PDF(s) are still being indexed",
        "1 PDF(s) have no searchable text",
        "nope",
        "best-ranked",
    ] {
        assert!(notes.contains(needle), "{needle} in {notes}");
    }
}

#[test]
fn find_in_page_spans_line_breaks_case_and_unicode() {
    let chars: Vec<PageChar> = "Machine\r\nLearning 日本語 machine learning"
        .chars()
        .enumerate()
        .map(|(i, c)| PageChar {
            unicode: Some(c),
            quad: Rect {
                left: i as f32,
                bottom: 0.0,
                right: i as f32 + 1.0,
                top: 1.0,
            }
            .quad(),
        })
        .collect();
    let found = find_in_page(&chars, "machine  LEARNING");
    assert_eq!(found.len(), 2);
    assert_eq!(found[0], (0, 16));
    assert_eq!(find_in_page(&chars, "日本"), vec![(18, 19)]);
    assert!(find_in_page(&chars, "   ").is_empty());
    assert!(find_in_page(&chars, "absent").is_empty());
}

#[test]
fn deep_links_round_trip_and_reject_garbage() {
    let page = DeepLink {
        paper: PaperId(id(1)),
        target: Target::Page(AttachmentId(id(2)), 0),
    };
    let text = page.to_string();
    assert_eq!(
        text,
        format!("paper://{}/attachment/{}/page/1", id(1), id(2))
    );
    assert_eq!(DeepLink::parse(&text), Some(page));
    let annotation = DeepLink {
        paper: PaperId(id(1)),
        target: Target::Annotation(id(9)),
    };
    assert_eq!(DeepLink::parse(&annotation.to_string()), Some(annotation));
    for bad in [
        "paper://",
        "http://x",
        &format!("paper://{}/attachment/{}/page/0", id(1), id(2)),
        &format!("paper://{}/attachment/{}/page/-1", id(1), id(2)),
        &format!("paper://{}/bogus", id(1)),
        &format!("paper://{}", uuid::Uuid::nil()),
        &format!("paper://{}/attachment/{}/page/1?x=日本", id(1), id(2)),
    ] {
        assert_eq!(DeepLink::parse(bad), None, "{bad}");
    }
}

#[test]
fn stored_page_text_counts_match_the_character_layer_rules() {
    assert_eq!(
        count_in_text(
            "Machine\r\nlearning and MACHINE learning",
            "machine learning"
        ),
        2
    );
    assert_eq!(count_in_text("日本語の日本", "日本"), 2);
    assert_eq!(count_in_text("anything", "  "), 0);
}

#[test]
fn deep_links_accept_only_the_canonical_form_they_print() {
    use cm_desktop::deep_links::DeepLink;
    let paper = "0192f2a1-7b3c-7d4e-8f00-112233445566";
    let att = "0192f2a1-7b3c-7d4e-8f00-aabbccddeeff";
    let ok = format!("paper://{paper}/attachment/{att}/page/12");
    let link = DeepLink::parse(&ok).expect("canonical link parses");
    assert_eq!(link.to_string(), ok);
    for bad in [
        format!("paper://{paper}/attachment/{att}/page/+12"),
        format!("paper://{paper}/attachment/{att}/page/012"),
        format!("paper://{paper}/attachment/{att}/page/0"),
        format!("paper://{paper}/attachment/{att}/page/"),
        format!("paper://{}/attachment/{att}", paper.replace('-', "")),
        format!("paper://{{{paper}}}"),
        format!("paper://{}", paper.to_uppercase()),
    ] {
        assert!(DeepLink::parse(&bad).is_none(), "{bad} must be rejected");
    }
}

#[test]
fn find_from_a_hit_keeps_its_page_and_starts_at_the_next_match() {
    // Matches on pages 2, 5 and 9 (0-based), e.g. the hit's text also occurs earlier.
    let pages = [(2, 1), (5, 3), (9, 1)];
    // Plain find: cursor at the first match, and jump there.
    assert_eq!(find_start(&pages, None), Some((0, Some(2))));
    // Opened from a hit on page 5: stay there (no jump), cursor on page 5's match.
    assert_eq!(find_start(&pages, Some(5)), Some((1, None)));
    // Hit page without a stored-text match: cursor at the next match after it, no jump.
    assert_eq!(find_start(&pages, Some(6)), Some((2, None)));
    // Every match is earlier than the hit's page: wrap the cursor, still no jump.
    assert_eq!(find_start(&pages, Some(10)), Some((0, None)));
    assert_eq!(find_start(&pages, Some(0)), Some((0, None)));
    assert_eq!(find_start(&[], Some(3)), None);
    assert_eq!(find_start(&[], None), None);
}
