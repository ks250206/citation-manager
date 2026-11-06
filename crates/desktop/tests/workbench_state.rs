use cm_desktop::{import_dialogs::Review, library::Library};
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
