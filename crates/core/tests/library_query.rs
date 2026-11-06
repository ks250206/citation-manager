use cm_core::{ServiceError, library_query::*};
use cm_domain::{PaperId, TagId};

fn invalid(result: Result<(), ServiceError>) {
    assert!(matches!(result, Err(ServiceError::InvalidInput(_))));
}
#[test]
fn filters_and_cursor_validate_at_the_common_boundary() {
    let q = LibraryQuery::default();
    q.validate().unwrap();
    for limit in [0, 501, u32::MAX] {
        invalid(LibraryQuery { limit, ..q.clone() }.validate());
    }
    for text in [
        "".into(),
        "\u{3000}\t".into(),
        "foo\0bar".into(),
        "x".repeat(1025),
    ] {
        for filter in [
            LibraryFilter {
                keyword: Some(text.clone()),
                ..Default::default()
            },
            LibraryFilter {
                author: Some(text.clone()),
                ..Default::default()
            },
            LibraryFilter {
                paper_type: Some(text.clone()),
                ..Default::default()
            },
        ] {
            invalid(
                LibraryQuery {
                    filter,
                    ..q.clone()
                }
                .validate(),
            );
        }
    }
    invalid(
        LibraryQuery {
            filter: LibraryFilter {
                keyword: Some(["a"; 17].join(" ")),
                ..Default::default()
            },
            ..q.clone()
        }
        .validate(),
    );
    invalid(
        LibraryQuery {
            filter: LibraryFilter {
                year_from: Some(2026),
                year_to: Some(2025),
                ..Default::default()
            },
            ..q.clone()
        }
        .validate(),
    );
    let id: PaperId = serde_json::from_str("\"00000000-0000-0000-0000-000000000001\"").unwrap();
    let nil: TagId = serde_json::from_str("\"00000000-0000-0000-0000-000000000000\"").unwrap();
    invalid(
        LibraryQuery {
            filter: LibraryFilter {
                tags: vec![nil],
                ..Default::default()
            },
            ..q.clone()
        }
        .validate(),
    );
    invalid(
        LibraryQuery {
            filter: LibraryFilter {
                tags: vec![TagId(id.0); 101],
                ..Default::default()
            },
            ..q.clone()
        }
        .validate(),
    );
    let cursor = LibraryCursor::from_position(&q, id, SortValue::Id).unwrap();
    assert_eq!(
        cursor,
        LibraryCursor::from_encoded(cursor.as_str().into()).unwrap()
    );
    LibraryQuery {
        limit: 1,
        after: Some(cursor.clone()),
        ..q.clone()
    }
    .validate()
    .unwrap();
    invalid(
        LibraryQuery {
            filter: LibraryFilter {
                has_pdf: Some(true),
                ..Default::default()
            },
            after: Some(cursor.clone()),
            ..q.clone()
        }
        .validate(),
    );
    invalid(
        LibraryQuery {
            sort: LibrarySort {
                field: SortField::Year,
                ..Default::default()
            },
            after: Some(cursor.clone()),
            ..q.clone()
        }
        .validate(),
    );
    assert!(LibraryCursor::from_position(&q, id, SortValue::Year(None)).is_err());
    let original: serde_json::Value = serde_json::from_str(cursor.as_str()).unwrap();
    for (key, value) in [
        ("version", serde_json::json!(2)),
        ("id", serde_json::json!("not-a-uuid")),
        (
            "id",
            serde_json::json!("00000000-0000-0000-0000-000000000000"),
        ),
        ("value", serde_json::json!({"Year":"bad"})),
        ("unknown", serde_json::json!(1)),
    ] {
        let mut tampered = original.clone();
        tampered[key] = value;
        assert!(LibraryCursor::from_encoded(tampered.to_string()).is_err());
    }
    for text in [
        "".into(),
        "{}".into(),
        "null".into(),
        "{".into(),
        "x".repeat(1_048_577),
    ] {
        assert!(LibraryCursor::from_encoded(text).is_err());
    }
}
