//! Old-API-produced data, not new-API reconstruction of the regression.
use cm_core::{
    ServiceError,
    attachments::{AttachmentService, ImportAttachment},
    import::*,
    papers::{NewPaper, PaperLibrary, PaperList, normalize_doi},
    ports::{AttachmentRepository, ImportService, PaperRepository},
};
use cm_domain::{ExternalId, Paper};
use cm_storage::{LibraryDb, LibraryDirectory};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

fn fixture() -> (tempfile::TempDir, LibraryDirectory, LibraryDb) {
    let temp = tempfile::tempdir().unwrap();
    let dir =
        LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("library")).unwrap();
    if let Some(source) = std::env::var_os("CM_TEST_OLD_API_DATABASE") {
        // Explicit test-only archived API fixture; never the user's library. Copy all bytes
        // after that generator exits (its WAL is checkpointed), then open through new API.
        std::fs::copy(source, dir.database_path()).unwrap();
    } else {
        drop(LibraryDb::open(&dir).unwrap());
        Connection::open(dir.database_path())
            .unwrap()
            .execute_batch(include_str!("fixtures/legacy_doi/old_api.sql"))
            .unwrap();
    }
    let db = LibraryDb::open(&dir).unwrap();
    (temp, dir, db)
}
fn all(db: &mut LibraryDb) -> Vec<Paper> {
    db.papers().list(&PaperList::default()).unwrap()
}
fn request(draft: PaperDraft) -> ImportRequest {
    ImportRequest {
        candidates: vec![draft],
        destination: Default::default(),
    }
}
fn decision(preview: &ImportPreview, action: ImportAction) -> ImportCommit {
    ImportCommit {
        preview_id: preview.id.clone(),
        decisions: vec![ImportDecision {
            candidate_index: 0,
            action,
        }],
    }
}
fn draft(value: &str) -> PaperDraft {
    let mut d = PaperDraft::titled("Reviewed replacement title");
    d.bibliography.doi = Some(value.into());
    d
}
fn hashes(dir: &LibraryDirectory) -> (Vec<u8>, Vec<u8>) {
    let hash = |p| Sha256::digest(std::fs::read(p).unwrap()).to_vec();
    (
        hash(dir.database_path()),
        hash(dir.database_path().with_extension("sqlite-wal")),
    )
}
#[test]
fn old_api_disk_reopen_title_edits_and_exact_preview_create_conflict_are_lossless() {
    let (_temp, dir, mut db) = fixture();
    let originals = all(&mut db);
    assert_eq!(originals.len(), 6);
    assert_eq!(
        originals[0].bibliography.doi.as_deref(),
        Some("10.1002/abc%3c493%3e")
    );
    for old in &originals {
        let mut edit = old.clone();
        edit.title = "Only title was edited".into();
        let saved = db.papers().update(&edit).unwrap();
        edit.updated_at = saved.updated_at.clone();
        assert_eq!(saved, edit);
    }
    let before = hashes(&dir);
    let mut service = ImportCoordinator::new(db.imports());
    for old in originals.iter().take(3) {
        let raw_url = &old.external_ids[0].value;
        let p = service.preview(request(draft(raw_url))).unwrap();
        assert_eq!(p.candidates[0].existing_match, Some(old.id));
        assert_eq!(p.candidates[0].matches[0].kind, MatchKind::Doi);
        assert_eq!(
            service.commit(decision(&p, ImportAction::Create)),
            Err(ServiceError::Conflict)
        );
        service.cancel(&p.id).unwrap();
    }
    drop(service);
    assert_eq!(hashes(&dir), before);
    assert_eq!(all(&mut db).len(), 6);
    drop(db);
    let mut reopened = LibraryDb::open(&dir).unwrap();
    for (old, saved) in originals.iter().zip(all(&mut reopened)) {
        assert_eq!(saved.bibliography, old.bibliography);
        assert_eq!(saved.external_ids, old.external_ids);
    }
}
#[test]
fn approved_merge_keeps_unselected_legacy_doi_raw_ids_metadata_and_attachment() {
    let (temp, dir, mut db) = fixture();
    let mut old = all(&mut db).remove(0);
    old.bibliography.citation_key = Some("LegacyOwnedKey".into());
    let old = db.papers().update(&old).unwrap();
    let source = temp.path().canonicalize().unwrap().join("synthetic.pdf");
    std::fs::write(&source, b"synthetic immutable attachment, no parser claim").unwrap();
    let attachment = AttachmentService::new(db.managed_attachments(&dir).unwrap())
        .import(ImportAttachment {
            paper_id: old.id,
            source: source.clone(),
            original_name: "synthetic.pdf".into(),
            media_type: "application/pdf".into(),
            expected_sha256: None,
        })
        .unwrap();
    let blob = dir.path().join(attachment.storage_key.as_str());
    let bytes = std::fs::read(&blob).unwrap();
    let mut incoming = draft(&old.external_ids[0].value);
    incoming.bibliography.citation_key = old.bibliography.citation_key.clone();
    incoming.bibliography.journal = Some("Unselected journal".into());
    incoming.external_ids.push(ExternalId {
        namespace: "opaque".into(),
        value: "Approved addition".into(),
    });
    let mut service = ImportCoordinator::new(db.imports());
    let p = service.preview(request(incoming.clone())).unwrap();
    assert_eq!(
        p.candidates[0].draft.bibliography.citation_key,
        old.bibliography.citation_key
    );
    assert!(
        !p.candidates[0].matches[0]
            .differences
            .iter()
            .any(|diff| diff.field == "citation_key")
    );
    service
        .commit(decision(
            &p,
            ImportAction::Merge {
                match_index: 0,
                fields: vec!["title".into()],
            },
        ))
        .unwrap();
    drop(service);
    let saved = db.papers().get(old.id).unwrap().unwrap();
    let mut expected = old.clone();
    expected.title = incoming.title.clone();
    expected.updated_at = saved.updated_at.clone();
    assert_eq!(saved, expected);
    let mut service = ImportCoordinator::new(db.imports());
    let p = service.preview(request(incoming)).unwrap();
    service
        .commit(decision(
            &p,
            ImportAction::Merge {
                match_index: 0,
                fields: vec!["external_ids".into()],
            },
        ))
        .unwrap();
    drop(service);
    let saved = db.papers().get(old.id).unwrap().unwrap();
    assert_eq!(saved.bibliography, old.bibliography);
    assert_eq!(saved.external_ids[0], old.external_ids[0]);
    assert_eq!(saved.external_ids.len(), 2);
    assert_eq!(
        db.attachments().for_paper(old.id).unwrap(),
        vec![attachment.clone()]
    );
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    assert_eq!(std::fs::read(&blob).unwrap(), bytes);
    drop(db);
    let mut reopened = LibraryDb::open(&dir).unwrap();
    assert_eq!(
        reopened.attachments().for_paper(old.id).unwrap(),
        vec![attachment]
    );
    assert_eq!(reopened.papers().get(old.id).unwrap(), Some(saved.clone()));
    // An explicitly approved DOI field can adopt the canonical spelling; the raw
    // resolver remains intact. That permission does not bypass the DOI coherence check.
    let mut service = ImportCoordinator::new(reopened.imports());
    let p = service
        .preview(request(draft(&old.external_ids[0].value)))
        .unwrap();
    service
        .commit(decision(
            &p,
            ImportAction::Merge {
                match_index: 0,
                fields: vec!["doi".into()],
            },
        ))
        .unwrap();
    drop(service);
    let canonical = reopened.papers().get(old.id).unwrap().unwrap();
    assert_eq!(
        canonical.bibliography.doi.as_deref(),
        Some("10.1002/abc<493>")
    );
    assert_eq!(canonical.external_ids, saved.external_ids);
    assert_eq!(canonical.title, saved.title);
    assert_eq!(
        canonical.bibliography.citation_key,
        saved.bibliography.citation_key
    );
}
#[test]
fn manual_create_save_update_and_trash_cannot_bypass_legacy_equivalence() {
    let (_temp, dir, mut db) = fixture();
    let old = all(&mut db).remove(0);
    let other = db
        .papers()
        .create(NewPaper::titled("Distinct record"))
        .unwrap();
    let before = hashes(&dir);
    for value in [
        &old.external_ids[0].value,
        "10.1002/abc<493>",
        "10.1002/abc%3c493%3e",
    ] {
        let mut new = NewPaper::titled("Unreviewed duplicate");
        new.bibliography.doi = Some(value.into());
        assert_eq!(db.papers().create(new), Err(ServiceError::Conflict));
        let mut edited = other.clone();
        edited.bibliography.doi = Some(value.into());
        assert_eq!(db.papers().update(&edited), Err(ServiceError::Conflict));
        edited.id = cm_domain::PaperId(uuid::Uuid::now_v7());
        assert_eq!(db.papers().save(&edited), Err(ServiceError::Conflict));
    }
    assert_eq!(hashes(&dir), before);
    db.papers().set_deleted(old.id, true).unwrap();
    let mut new = NewPaper::titled("Trash still reserves identifiers");
    new.external_ids = old.external_ids;
    assert_eq!(db.papers().create(new), Err(ServiceError::Conflict));
    assert_eq!(db.papers().get(other.id).unwrap(), Some(other));
}
#[test]
fn true_disagreement_and_new_input_cannot_claim_stored_legacy_exception() {
    let (_temp, dir, mut db) = fixture();
    let old = all(&mut db).remove(0);
    let before = hashes(&dir);
    let mut explicit = old.clone();
    explicit.bibliography.doi = Some("10.1002/unknown".into());
    assert!(matches!(
        db.papers().update(&explicit),
        Err(ServiceError::InvalidInput(_))
    ));
    explicit = old.clone();
    explicit.external_ids.push(ExternalId {
        namespace: "doi".into(),
        value: "10.1002/unknown".into(),
    });
    assert!(matches!(
        db.papers().update(&explicit),
        Err(ServiceError::InvalidInput(_))
    ));
    let new = NewPaper {
        title: "Fake legacy input".into(),
        bibliography: old.bibliography.clone(),
        external_ids: old.external_ids.clone(),
    };
    assert!(matches!(
        db.papers().create(new),
        Err(ServiceError::InvalidInput(_))
    ));
    let mut incoming = draft("10.1002/unknown");
    incoming.title = old.title;
    let mut service = ImportCoordinator::new(db.imports());
    let p = service.preview(request(incoming)).unwrap();
    assert_eq!(p.candidates[0].matches[0].kind, MatchKind::SimilarTitle);
    for fields in [vec![], vec!["title".into()], vec!["doi".into()]] {
        assert!(matches!(
            service.commit(decision(
                &p,
                ImportAction::Merge {
                    match_index: 0,
                    fields
                }
            )),
            Err(ServiceError::InvalidInput(_))
        ));
    }
    drop(service);
    assert_eq!(hashes(&dir), before);
}
#[test]
fn raw_percent_identifiers_without_resolver_evidence_remain_distinct() {
    let mut db = LibraryDb::in_memory().unwrap();
    for value in ["10.123/raw%3c493%3e", "10.123/raw<493>"] {
        let mut new = NewPaper::titled("Independent literal identifier");
        new.bibliography.doi = Some(value.into());
        db.papers().create(new).unwrap();
    }
    let papers = all(&mut db);
    assert_ne!(papers[0].bibliography.doi, papers[1].bibliography.doi);
    let mut service = ImportCoordinator::new(db.imports());
    for (value, expected) in [
        ("https://doi.org/10.123/raw%3C493%3E", papers[1].id),
        ("https://doi.org/10.123/raw%253C493%253E", papers[0].id),
    ] {
        let p = service.preview(request(draft(value))).unwrap();
        assert_eq!(p.candidates[0].existing_match, Some(expected));
        assert_eq!(
            p.candidates[0]
                .matches
                .iter()
                .filter(|m| m.kind.exact())
                .count(),
            1
        );
        service.cancel(&p.id).unwrap();
    }
}
#[test]
fn legacy_equivalent_existing_rows_are_all_reported_and_fail_closed() {
    let (_temp, dir, mut db) = fixture();
    let old = all(&mut db).remove(0);
    // Simulate an existing collision admitted by the broken intermediate API. No service
    // may choose which row to destroy or coalesce; expose both and reject writes.
    let raw = Connection::open(dir.database_path()).unwrap();
    let duplicate = uuid::Uuid::now_v7().to_string();
    raw.execute("INSERT INTO papers(id,title,doi_normalized,created_at,updated_at) VALUES (?1,'Existing collision',?2,'old','old')", [&duplicate, "10.1002/abc<493>"]).unwrap();
    let before = hashes(&dir);
    let mut service = ImportCoordinator::new(db.imports());
    let p = service
        .preview(request(draft(&old.external_ids[0].value)))
        .unwrap();
    assert_eq!(
        p.candidates[0]
            .matches
            .iter()
            .filter(|m| m.kind.exact())
            .count(),
        2
    );
    assert_eq!(
        service.commit(decision(&p, ImportAction::Create)),
        Err(ServiceError::Conflict)
    );
    for match_index in 0..2 {
        assert_eq!(
            service.commit(decision(
                &p,
                ImportAction::Merge {
                    match_index,
                    fields: vec![]
                }
            )),
            Err(ServiceError::Conflict)
        );
    }
    drop(service);
    let mut edit = old.clone();
    edit.title = "No silent collision repair".into();
    assert_eq!(db.papers().update(&edit), Err(ServiceError::Conflict));
    assert_eq!(hashes(&dir), before);
    assert_eq!(all(&mut db).len(), 7);
}
#[test]
fn malformed_legacy_urls_are_preserved_but_never_accepted_as_new_urls() {
    let (_temp, dir, mut db) = fixture();
    let papers = all(&mut db);
    for old in papers.iter().skip(3) {
        let raw_url = &old.external_ids[0].value;
        assert!(normalize_doi(raw_url).is_err());
        let mut service = ImportCoordinator::new(db.imports());
        let p = service.preview(request(draft(raw_url))).unwrap();
        assert!(p.candidates[0].diagnostics.iter().any(|d| d.error));
        assert!(matches!(
            service.commit(decision(&p, ImportAction::Create)),
            Err(ServiceError::InvalidInput(_))
        ));
        service.cancel(&p.id).unwrap();
        // The stored raw DOI is still a literal, so it can be reviewed and matched.
        let p = service
            .preview(request(draft(old.bibliography.doi.as_deref().unwrap())))
            .unwrap();
        assert_eq!(p.candidates[0].existing_match, Some(old.id));
        service
            .commit(decision(
                &p,
                ImportAction::Merge {
                    match_index: 0,
                    fields: vec!["title".into()],
                },
            ))
            .unwrap();
        drop(service);
        let saved = db.papers().get(old.id).unwrap().unwrap();
        assert_eq!(saved.external_ids, old.external_ids);
        assert_eq!(saved.bibliography, old.bibliography);
        let mut edited = saved.clone();
        edited.external_ids[0].value.push('x');
        assert!(matches!(
            db.papers().update(&edited),
            Err(ServiceError::InvalidInput(_))
        ));
    }
    drop(db);
    assert_eq!(
        LibraryDb::open(&dir)
            .unwrap()
            .papers()
            .list(&PaperList::default())
            .unwrap()
            .len(),
        6
    );
}
