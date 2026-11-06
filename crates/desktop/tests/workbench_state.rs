use cm_desktop::{import_dialogs::Review, library::Library};
use cm_protocol::{import::*, library_query::*};
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
