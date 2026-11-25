use cm_desktop::{
    import_dialogs::Review,
    library::{Library, papers_count},
};
use cm_protocol::domain::{Paper, PaperId};
use cm_protocol::{import::*, library_query::*};

fn page(id: &str, next: Option<LibraryCursor>) -> LibraryPage {
    LibraryPage {
        papers: vec![Paper {
            id: PaperId(id.parse().unwrap()),
            title: id.into(),
            external_ids: vec![],
            bibliography: Default::default(),
            created_at: None,
            updated_at: None,
            deleted_at: None,
        }],
        next,
    }
}

#[test]
fn page_summary_counts_papers_in_the_singular_and_plural() {
    assert_eq!(papers_count(0), "0 papers");
    assert_eq!(papers_count(1), "1 paper");
    assert_eq!(papers_count(2), "2 papers");
    let mut library = Library::default();
    library.reload();
    library.complete(
        library.generation,
        page("00000000-0000-0000-0000-000000000001", None),
    );
    assert_eq!(library.page_summary(false), "Page 1 · 1 paper");
    library.select_only(library.papers[0].id);
    assert_eq!(
        library.page_summary(true),
        "Page 1 · 1 paper · 1 selected · loading…"
    );
}

#[test]
fn failed_page_moves_keep_rows_cursor_and_history_and_allow_retry() {
    let mut library = Library::default();
    let first_id = "00000000-0000-0000-0000-000000000001";
    let second_id = "00000000-0000-0000-0000-000000000002";
    let cursor = LibraryCursor::from_position(
        &library.query,
        PaperId(first_id.parse().unwrap()),
        SortValue::Id,
    )
    .unwrap();
    library.reload();
    library.complete(library.generation, page(first_id, Some(cursor.clone())));
    let first_page = library.papers.clone();
    let next_request = library.next_page().unwrap();
    assert_eq!(next_request.after, Some(cursor.clone()));
    assert!(library.query.after.is_none());
    assert!(library.previous.is_empty());
    assert_eq!(library.papers, first_page);
    assert!(library.next_page().is_none());
    assert!(library.fail(library.generation));
    assert_eq!(library.next, Some(cursor.clone()));
    assert_eq!(library.next_page().unwrap(), next_request);
    library.complete(library.generation, page(second_id, None));
    assert_eq!(library.query.after, Some(cursor.clone()));
    assert_eq!(library.previous, vec![None]);

    let second_page = library.papers.clone();
    let back_request = library.previous_page().unwrap();
    assert!(back_request.after.is_none());
    assert_eq!(library.query.after, Some(cursor.clone()));
    assert_eq!(library.previous, vec![None]);
    assert_eq!(library.papers, second_page);
    assert!(library.fail(library.generation));
    assert_eq!(library.previous_page().unwrap(), back_request);
    library.complete(library.generation, page(first_id, Some(cursor)));
    assert!(library.query.after.is_none());
    assert!(library.previous.is_empty());
    assert_eq!(library.papers, first_page);
}

#[test]
fn stale_page_errors_cannot_cancel_a_new_navigation_after_reload() {
    let mut library = Library::default();
    let first_id = "00000000-0000-0000-0000-000000000001";
    let cursor = LibraryCursor::from_position(
        &library.query,
        PaperId(first_id.parse().unwrap()),
        SortValue::Id,
    )
    .unwrap();
    library.reload();
    library.complete(library.generation, page(first_id, Some(cursor.clone())));
    library.next_page().unwrap();
    let stale = library.generation;
    library.query.filter.keyword = Some("new query".into());
    let query = library.reload();
    assert!(library.papers.is_empty());
    assert!(library.query.after.is_none());
    assert!(library.previous.is_empty());
    let new_cursor =
        LibraryCursor::from_position(&query, PaperId(first_id.parse().unwrap()), SortValue::Id)
            .unwrap();
    library.complete(library.generation, page(first_id, Some(new_cursor.clone())));
    library.next_page().unwrap();
    assert!(!library.fail(stale));
    assert!(!library.complete(stale, page(first_id, Some(cursor))));
    library.complete(library.generation, page(first_id, None));
    assert_eq!(library.query.after, Some(new_cursor));
    assert_eq!(library.previous, vec![None]);
}
#[test]
fn stale_query_pages_cannot_replace_current_filters_and_sort() {
    let mut library = Library::default();
    let first = library.reload();
    let old = library.generation;
    library.query.filter.keyword = Some("new query".into());
    let second = library.reload();
    assert_ne!(first.filter, second.filter);
    assert!(!library.complete(
        old,
        LibraryPage {
            papers: vec![],
            next: None
        }
    ));
    assert!(library.complete(
        library.generation,
        LibraryPage {
            papers: vec![],
            next: None
        }
    ));
}
#[test]
fn all_rows_need_explicit_decisions_and_errors_can_only_be_skipped() {
    let valid = PreviewCandidate {
        draft: PaperDraft::titled("Valid"),
        existing_match: None,
        matches: vec![],
        batch_matches: vec![],
        diagnostics: vec![],
    };
    let mut invalid = valid.clone();
    invalid.diagnostics.push(RowDiagnostic {
        field: "year".into(),
        message: "Invalid".into(),
        error: true,
    });
    let mut review = Review::default();
    review.set_preview(ImportPreview {
        id: PreviewId("opaque-test".into()),
        candidates: vec![valid, invalid],
        warnings: vec![],
    });
    assert_eq!(
        review.total, 2,
        "manual progress must count its preview rows"
    );
    assert!(review.commit().is_none());
    review.choose_valid_creates();
    assert!(review.commit().is_none());
    assert!(!review.choose(1, ImportAction::Create));
    assert!(review.choose(1, ImportAction::Skip));
    assert_eq!(review.commit().unwrap().decisions.len(), 2);
}
#[test]
fn title_only_edit_preserves_abstract_absence_and_contributor_identity() {
    let mut draft = PaperDraft::titled("Original");
    draft
        .bibliography
        .authors
        .push(cm_protocol::domain::Contributor {
            given_name: Some("Given".into()),
            family_name: Some("Family".into()),
            literal_name: None,
            orcid: Some("identity".into()),
            role: "editor".into(),
        });
    let mut values = cm_desktop::details::values(&draft);
    values[0] = "Edited".into();
    let edited = cm_desktop::details::apply(&draft, &values).unwrap();
    assert!(edited.bibliography.abstract_text.is_none());
    assert_eq!(edited.bibliography.authors, draft.bibliography.authors);
}

fn ids(n: u8) -> Vec<PaperId> {
    (1..=n)
        .map(|i| {
            PaperId(
                format!("00000000-0000-0000-0000-0000000000{i:02}")
                    .parse()
                    .unwrap(),
            )
        })
        .collect()
}

fn loaded(ids: &[PaperId]) -> Library {
    let mut library = Library::default();
    library.reload();
    let mut page = page("00000000-0000-0000-0000-000000000001", None);
    let template = page.papers.remove(0);
    page.papers = ids
        .iter()
        .map(|id| Paper {
            id: *id,
            ..template.clone()
        })
        .collect();
    library.complete(library.generation, page);
    library
}

#[test]
fn keyboard_row_movement_starts_at_edges_and_clamps_to_loaded_page() {
    assert_eq!(Library::default().adjacent(1), None);
    let ids = ids(3);
    let mut library = loaded(&ids);
    assert_eq!(library.adjacent(1), Some(ids[0]));
    assert_eq!(library.adjacent(-1), Some(ids[2]));
    library.select(ids[1], false);
    assert_eq!(library.adjacent(1), Some(ids[2]));
    assert_eq!(library.adjacent(-1), Some(ids[0]));
    library.select(ids[2], false);
    assert_eq!(library.adjacent(1), Some(ids[2]));
    assert_eq!(library.adjacent(isize::MAX), Some(ids[2]));
    library.select(ids[0], false);
    assert_eq!(library.adjacent(-1), Some(ids[0]));
    assert_eq!(library.adjacent(isize::MIN), Some(ids[0]));
    assert_eq!(library.index_of(ids[1]), Some(1));
}

#[test]
fn keyboard_row_movement_follows_latest_visible_selection() {
    let ids = ids(4);
    let mut library = loaded(&ids);
    library.select(ids[3], false);
    library.select(ids[1], true);
    assert_eq!(library.adjacent(1), Some(ids[2]));
    // A selection retained from another page does not anchor movement.
    let mut other = loaded(&ids[..2]);
    other.selected = vec![ids[0], ids[3]];
    assert_eq!(other.adjacent(1), Some(ids[1]));
}

#[test]
fn opening_a_paper_from_a_hit_or_link_makes_it_the_whole_selection() {
    // Export "Selected" builds its ids from `selected`; after a search hit or `paper://`
    // link opens another paper, the export must target that paper, not the old rows.
    let all = ids(4);
    let (ids, outside) = (&all[..3], all[3]);
    let mut library = loaded(ids);
    library.select(ids[0], false);
    library.select(ids[1], true);
    library.select_only(ids[2]);
    assert_eq!(library.selected, vec![ids[2]]);
    // Opening the already selected paper again keeps it selected (no toggle).
    library.select_only(ids[2]);
    assert_eq!(library.selected, vec![ids[2]]);
    // A paper outside the loaded page (e.g. a hit from another collection) still counts.
    library.select_only(outside);
    assert_eq!(library.selected, vec![outside]);
}
