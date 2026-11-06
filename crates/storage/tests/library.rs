use cm_core::{
    ServiceError,
    papers::{NewPaper, PaperLibrary, PaperList, PaperService, PaperVisibility},
    ports::{AttachmentRepository, PaperRepository},
};
use cm_domain::{Bibliography, Contributor, ExternalId, PaperId};
use cm_storage::{LibraryDb, LibraryDirectory};
use rusqlite::{Connection, params};
use serde_json::json;
use uuid::Uuid;

fn library() -> (tempfile::TempDir, LibraryDirectory) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap().join("library");
    let directory = LibraryDirectory::prepare(path).unwrap();
    (temp, directory)
}
fn full_metadata() -> NewPaper {
    NewPaper {
        title: "固体電解質 — 保存試験".into(),
        external_ids: vec![
            ExternalId {
                namespace: "DOI".into(),
                value: "https://doi.org/10.1234/EXAMPLE".into(),
            },
            ExternalId {
                namespace: "arxiv".into(),
                value: "2601.12345v2".into(),
            },
            ExternalId {
                namespace: "custom provider".into(),
                value: "Some / opaque # value".into(),
            },
        ],
        bibliography: Bibliography {
            paper_type: Some("journal-article".into()),
            abstract_text: Some("日本語\nAbstract with 'quotes'".into()),
            year: Some(2026),
            publication_date: Some("2026-11-06".into()),
            journal: Some("Journal".into()),
            volume: Some("12".into()),
            issue: Some("3".into()),
            pages: Some("S1–S12".into()),
            doi: Some(" DOI:10.1234/Example ".into()),
            url: Some("https://example.org/paper".into()),
            language: Some("ja".into()),
            citation_key: Some("Kanno2026".into()),
            authors: vec![
                Contributor {
                    given_name: Some("Ryoji".into()),
                    family_name: Some("Kanno".into()),
                    literal_name: None,
                    orcid: Some("0000-0002-1825-0097".into()),
                    role: "author".into(),
                },
                Contributor {
                    given_name: None,
                    family_name: None,
                    literal_name: Some("研究コンソーシアム".into()),
                    orcid: None,
                    role: "editor".into(),
                },
                Contributor {
                    given_name: Some("A.".into()),
                    family_name: Some("Second".into()),
                    literal_name: None,
                    orcid: None,
                    role: "author".into(),
                },
            ],
            extra_json: Some(
                json!({"nested":{"x":[1,"二",null,true]}, "unknownField":"preserved", "large":18446744073709551615u64}),
            ),
        },
    }
}

#[test]
fn full_metadata_ordered_authors_and_external_ids_survive_real_reopen() {
    let (_temp, dir) = library();
    let saved = {
        let mut db = LibraryDb::open(&dir).unwrap();
        assert_eq!(db.journal_mode().unwrap(), "wal");
        assert!(db.foreign_keys_enabled().unwrap());
        let input = full_metadata();
        let mut service = PaperService::new(db.papers());
        let paper = service.create(input.clone()).unwrap();
        assert_eq!(paper.id.0.get_version_num(), 7);
        assert_eq!(paper.bibliography.doi.as_deref(), Some("10.1234/example"));
        assert_eq!(paper.external_ids, input.external_ids);
        assert_eq!(paper.bibliography.authors, input.bibliography.authors);
        assert!(paper.created_at.as_deref().unwrap().ends_with('Z'));
        assert_eq!(paper.created_at, paper.updated_at);
        assert_eq!(service.get(paper.id).unwrap(), paper);
        paper
    };
    let mut db = LibraryDb::open(&dir).unwrap();
    let mut second = LibraryDb::open(&dir).unwrap();
    assert_eq!(second.journal_mode().unwrap(), "wal");
    assert!(second.foreign_keys_enabled().unwrap());
    assert_eq!(db.papers().get(saved.id).unwrap(), Some(saved.clone()));
    assert_eq!(second.papers().get(saved.id).unwrap(), Some(saved));
}

#[test]
fn service_crud_soft_delete_restore_and_paginated_visibility() {
    let (_temp, dir) = library();
    let mut db = LibraryDb::open(&dir).unwrap();
    let (first, second, third) = {
        let mut service = PaperService::new(db.papers());
        let first = service.create(NewPaper::titled("first")).unwrap();
        let second = service.create(NewPaper::titled("second")).unwrap();
        let third = service.create(NewPaper::titled("third")).unwrap();
        (first, second, third)
    };
    let mut service = PaperService::new(db.papers());
    let mut edit = second.clone();
    edit.title = "edited".into();
    edit.created_at = Some("client-supplied".into());
    let edited = service.update(&edit).unwrap();
    assert_eq!(edited.created_at, second.created_at);
    assert_eq!(service.get(second.id).unwrap().title, "edited");
    let deleted = service.soft_delete(first.id).unwrap();
    assert!(deleted.deleted_at.is_some());
    assert_eq!(service.soft_delete(first.id).unwrap(), deleted); // idempotent
    assert_eq!(service.get(first.id), Err(ServiceError::NotFound));
    assert_eq!(service.update(&first), Err(ServiceError::NotFound));
    let first_page = service
        .list(&PaperList {
            limit: 1,
            ..PaperList::default()
        })
        .unwrap();
    assert_eq!(first_page[0].id, second.id);
    let next_page = service
        .list(&PaperList {
            after: Some(second.id),
            limit: 1,
            ..PaperList::default()
        })
        .unwrap();
    assert_eq!(next_page[0].id, third.id);
    // A newly-created record cannot shift or duplicate an already-consumed keyset page.
    let fourth = service.create(NewPaper::titled("fourth")).unwrap();
    let next = service
        .list(&PaperList {
            after: Some(third.id),
            ..PaperList::default()
        })
        .unwrap();
    assert_eq!(
        next.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![fourth.id]
    );
    assert_eq!(
        service
            .list(&PaperList {
                visibility: PaperVisibility::Deleted,
                ..PaperList::default()
            })
            .unwrap(),
        vec![deleted.clone()]
    );
    assert_eq!(
        service
            .list(&PaperList {
                visibility: PaperVisibility::All,
                ..PaperList::default()
            })
            .unwrap()
            .len(),
        4
    );
    let mut repo = service.into_repository();
    assert_eq!(repo.get_including_deleted(first.id).unwrap(), Some(deleted));
    assert_eq!(repo.save(&second), Err(ServiceError::Conflict)); // never an upsert
    let mut service = PaperService::new(repo);
    assert_eq!(service.restore(first.id).unwrap().deleted_at, None);
    assert_eq!(service.get(first.id).unwrap().title, "first");
    let missing = PaperId(Uuid::now_v7());
    assert_eq!(service.restore(missing), Err(ServiceError::NotFound));
    assert_eq!(service.soft_delete(missing), Err(ServiceError::NotFound));
    assert_eq!(service.get(missing), Err(ServiceError::NotFound));
    assert!(matches!(
        service.create(NewPaper::titled(" \t")),
        Err(ServiceError::InvalidInput(_))
    ));
    assert!(matches!(
        service.list(&PaperList {
            limit: 0,
            ..PaperList::default()
        }),
        Err(ServiceError::InvalidInput(_))
    ));
    assert!(matches!(
        service.list(&PaperList {
            limit: 501,
            ..PaperList::default()
        }),
        Err(ServiceError::InvalidInput(_))
    ));
    drop(db);
    let mut db = LibraryDb::open(&dir).unwrap();
    assert_eq!(db.papers().get(second.id).unwrap().unwrap().title, "edited");
    db.papers().set_deleted(first.id, true).unwrap();
    drop(db);
    let mut db = LibraryDb::open(&dir).unwrap();
    assert!(db.papers().get(first.id).unwrap().is_none());
    assert!(
        db.papers()
            .get_including_deleted(first.id)
            .unwrap()
            .unwrap()
            .deleted_at
            .is_some()
    );
    assert!(
        db.papers()
            .set_deleted(first.id, false)
            .unwrap()
            .deleted_at
            .is_none()
    );
}

#[test]
fn unique_doi_citation_key_and_relation_failures_never_overwrite_or_partially_update() {
    let (_temp, dir) = library();
    let mut db = LibraryDb::open(&dir).unwrap();
    let mut repo = db.papers();
    let original = repo.create(full_metadata()).unwrap();
    for doi in [
        "10.1234/EXAMPLE",
        "https://dx.doi.org/10.1234/example",
        "doi:10.1234/Example",
    ] {
        let mut duplicate = NewPaper::titled("must not overwrite");
        duplicate.bibliography.doi = Some(doi.into());
        assert_eq!(repo.create(duplicate), Err(ServiceError::Conflict));
    }
    let mut duplicate_key = NewPaper::titled("different DOI, same key");
    duplicate_key.bibliography.citation_key = original.bibliography.citation_key.clone();
    assert_eq!(repo.create(duplicate_key), Err(ServiceError::Conflict));
    // Inject a real SQL failure after metadata and author writes. Duplicate input
    // is now rejected before a transaction, so it cannot exercise rollback here.
    let fault = Connection::open(dir.database_path()).unwrap();
    fault.execute_batch("CREATE TRIGGER reject_test_external_id BEFORE INSERT ON paper_external_ids WHEN NEW.namespace = 'test-reject' BEGIN SELECT RAISE(ABORT, 'test relation failure'); END").unwrap();
    let mut changed = original.clone();
    changed.title = "should roll back".into();
    changed.bibliography.authors.reverse();
    changed.external_ids.push(ExternalId {
        namespace: "test-reject".into(),
        value: "force SQL failure".into(),
    });
    assert!(matches!(
        repo.update(&changed),
        Err(ServiceError::InvalidInput(_))
    ));
    assert_eq!(repo.get(original.id).unwrap(), Some(original.clone()));
    assert_eq!(repo.list(&PaperList::default()).unwrap().len(), 1);
    let mut invalid = original.clone();
    invalid.external_ids[0].value = "10.9999/different".into();
    assert!(matches!(
        repo.update(&invalid),
        Err(ServiceError::InvalidInput(_))
    ));
    repo.set_deleted(original.id, true).unwrap();
    let mut duplicate = NewPaper::titled("deleted DOI stays reserved for restore");
    duplicate.bibliography.doi = Some("10.1234/example".into());
    assert_eq!(repo.create(duplicate), Err(ServiceError::Conflict));
    assert_eq!(
        repo.set_deleted(original.id, false).unwrap().bibliography,
        original.bibliography
    );
    drop(db);
    let mut db = LibraryDb::open(&dir).unwrap();
    assert_eq!(
        db.papers().get(original.id).unwrap().unwrap().bibliography,
        original.bibliography
    );
}

#[test]
fn duplicate_external_ids_are_invalid_input_and_leave_existing_records_unchanged() {
    let (_temp, dir) = library();
    let mut db = LibraryDb::open(&dir).unwrap();
    let mut service = PaperService::new(db.papers());
    let original = service.create(full_metadata()).unwrap();
    let mut duplicate = NewPaper::titled("duplicate input");
    let identifier = ExternalId {
        namespace: "custom".into(),
        value: "same".into(),
    };
    duplicate.external_ids = vec![identifier.clone(), identifier.clone()];
    assert!(matches!(
        service.create(duplicate),
        Err(ServiceError::InvalidInput(_))
    ));
    let mut edited = original.clone();
    edited.title = "must not be saved".into();
    edited.external_ids = vec![identifier.clone(), identifier];
    assert!(matches!(
        service.update(&edited),
        Err(ServiceError::InvalidInput(_))
    ));
    assert_eq!(service.get(original.id).unwrap(), original);
    assert_eq!(service.list(&PaperList::default()).unwrap().len(), 1);
}

#[test]
fn metadata_edits_and_soft_delete_preserve_people_attachment_and_related_data() {
    let (_temp, dir) = library();
    let mut db = LibraryDb::open(&dir).unwrap();
    let mut paper = db.papers().create(full_metadata()).unwrap();
    // Test-only raw SQL populates future-feature tables; production does not expose arbitrary SQL.
    let sql = Connection::open(dir.database_path()).unwrap();
    sql.pragma_update(None, "foreign_keys", true).unwrap();
    let attachment_id = Uuid::now_v7();
    sql.execute("INSERT INTO attachments(id,paper_id,file_hash,relative_path,original_name,mime_type,byte_size,added_at)
        VALUES (?1,?2,?3,'ab/cd/file.pdf','original.pdf','application/pdf',123,'2026-11-06T00:00:00Z')",
        params![attachment_id.to_string(), paper.id.0.to_string(), vec![7u8;32]]).unwrap();
    sql.execute("INSERT INTO fulltext_pages(attachment_id,page_index,page_label,text,extraction_status,updated_at)
        VALUES (?1,0,'iv','extracted text','complete','now')", [attachment_id.to_string()]).unwrap();
    sql.execute("INSERT INTO annotations(id,attachment_id,type,source,anchor_state,unresolved_reason,quote_text,created_at,updated_at)
        VALUES ('ai-note',?1,'note','ai','unresolved','quote not found','unverified quote','now','now')", [attachment_id.to_string()]).unwrap();
    let before: Vec<String> = sql
        .prepare("SELECT person_id FROM paper_people WHERE paper_id=?1 ORDER BY sort_order")
        .unwrap()
        .query_map([paper.id.0.to_string()], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    paper.title = "metadata only".into();
    let updated = db.papers().update(&paper).unwrap();
    assert_eq!(updated.bibliography.authors, paper.bibliography.authors);
    let after: Vec<String> = sql
        .prepare("SELECT person_id FROM paper_people WHERE paper_id=?1 ORDER BY sort_order")
        .unwrap()
        .query_map([paper.id.0.to_string()], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(db.attachments().for_paper(paper.id).unwrap().len(), 1);
    db.papers().set_deleted(paper.id, true).unwrap();
    assert!(db.attachments().for_paper(paper.id).unwrap().is_empty());
    assert_eq!(
        sql.query_row("SELECT count(*) FROM attachments", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        1
    );
    db.papers().set_deleted(paper.id, false).unwrap();
    let attachment = db
        .attachments()
        .get(cm_domain::AttachmentId(attachment_id))
        .unwrap()
        .unwrap();
    assert_eq!(attachment.sha256, [7u8; 32]);
    assert_eq!(attachment.storage_key.as_str(), "ab/cd/file.pdf");
    assert_eq!(attachment.byte_size, 123);
    let state: (String, String, String, Option<i64>) = sql
        .query_row(
            "SELECT source,anchor_state,unresolved_reason,page_index FROM annotations",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        state,
        (
            "ai".into(),
            "unresolved".into(),
            "quote not found".into(),
            None
        )
    );
    assert!(
        sql.execute("UPDATE annotations SET page_index=0 WHERE id='ai-note'", [])
            .is_err()
    );
    assert!(
        sql.execute(
            "UPDATE annotations SET source='unknown' WHERE id='ai-note'",
            []
        )
        .is_err()
    );
    assert!(
        sql.execute("INSERT INTO paper_tags VALUES ('absent','absent')", [])
            .is_err()
    );
    // Resolve explicitly, retaining provenance and physical index vs label.
    sql.execute("UPDATE annotations SET anchor_state='resolved',unresolved_reason=NULL,page_index=0,page_label='iv',quads_json='[]'", []).unwrap();
    assert!(
        sql.execute("UPDATE annotations SET page_index=4294967295", [])
            .is_err()
    );
    let mut changed_authors = updated.clone();
    changed_authors.bibliography.authors.reverse();
    assert_eq!(
        db.papers()
            .update(&changed_authors)
            .unwrap()
            .bibliography
            .authors,
        changed_authors.bibliography.authors
    );
    assert_eq!(db.attachments().for_paper(paper.id).unwrap().len(), 1);
}

#[test]
fn in_memory_has_same_schema_and_foreign_keys_but_uses_memory_journal() {
    let mut db = LibraryDb::in_memory().unwrap();
    assert_eq!(db.journal_mode().unwrap(), "memory");
    assert!(db.foreign_keys_enabled().unwrap());
    let p = db.papers().create(full_metadata()).unwrap();
    assert_eq!(db.papers().get(p.id).unwrap(), Some(p));
}

#[test]
fn opening_a_file_database_refuses_modified_history_or_a_future_schema() {
    for sql in [
        "UPDATE schema_migrations SET checksum='modified' WHERE version LIKE '%001_%'",
        "INSERT INTO schema_migrations VALUES ('20991231T235959Z_issue-8_001_future.sql','future','now')",
    ] {
        let (_temp, dir) = library();
        let mut db = LibraryDb::open(&dir).unwrap();
        let paper = db.papers().create(full_metadata()).unwrap();
        drop(db);
        let raw = Connection::open(dir.database_path()).unwrap();
        raw.execute_batch(sql).unwrap();
        drop(raw);
        assert!(matches!(
            LibraryDb::open(&dir),
            Err(cm_storage::StorageError::Migration(_))
        ));
        let raw = Connection::open(dir.database_path()).unwrap();
        assert_eq!(
            raw.query_row("SELECT title FROM papers", [], |r| r.get::<_, String>(0))
                .unwrap(),
            paper.title
        );
        assert_eq!(
            raw.query_row("SELECT count(*) FROM paper_people", [], |r| r
                .get::<_, u32>(0))
                .unwrap(),
            3
        );
    }
}
