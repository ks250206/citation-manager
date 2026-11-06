use cm_core::{
    ServiceError,
    attachments::{AttachmentService, ImportAttachment},
    collections::{CollectionInput, CollectionLibrary},
    import::*,
    papers::{NewPaper, PaperLibrary, PaperList},
    ports::{AttachmentRepository, ImportService, PaperRepository},
    tags::{TagInput, TagLibrary},
};
use cm_domain::{Contributor, ExternalId};
use cm_storage::{LibraryDb, LibraryDirectory};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

fn disk() -> (tempfile::TempDir, LibraryDirectory, LibraryDb) {
    let temp = tempfile::tempdir().unwrap();
    let dir =
        LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("library")).unwrap();
    let db = LibraryDb::open(&dir).unwrap();
    (temp, dir, db)
}
fn req(candidates: Vec<PaperDraft>) -> ImportRequest {
    ImportRequest {
        candidates,
        destination: Default::default(),
    }
}
fn commit(p: &ImportPreview, actions: Vec<ImportAction>) -> ImportCommit {
    ImportCommit {
        preview_id: p.id.clone(),
        decisions: actions
            .into_iter()
            .enumerate()
            .map(|(candidate_index, action)| ImportDecision {
                candidate_index,
                action,
            })
            .collect(),
    }
}
fn doi(title: &str, value: &str) -> PaperDraft {
    let mut d = PaperDraft::titled(title);
    d.bibliography.doi = Some(value.into());
    d
}
fn count(raw: &Connection, table: &str) -> i64 {
    assert!(
        [
            "papers",
            "people",
            "paper_people",
            "paper_external_ids",
            "paper_tags",
            "paper_collections"
        ]
        .contains(&table)
    );
    raw.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn hash(path: &std::path::Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(path).unwrap()).to_vec()
}
#[test]
fn dry_run_cancel_are_byte_unchanged_and_handles_cannot_cross_libraries() {
    let (temp, dir, mut db) = disk();
    let input = temp.path().join("source.bib");
    std::fs::write(&input, b"synthetic source").unwrap();
    let initial = hash(&dir.database_path());
    let wal = dir.database_path().with_extension("sqlite-wal");
    let wal_initial = hash(&wal);
    let source = hash(&input);
    let mut d = doi("Dry run", "10.99/dry");
    d.source = ImportSource {
        adapter: "synthetic".into(),
        locator: Some("input-row".into()),
        row: Some(4),
        sha256: Some(Sha256::digest(std::fs::read(&input).unwrap()).into()),
    };
    let mut service = ImportCoordinator::new(db.imports());
    let mut p = service.preview(req(vec![d])).unwrap();
    p.candidates[0].draft.title = "Client tampering".into();
    let mut other = LibraryDb::in_memory().unwrap();
    let mut other = ImportCoordinator::new(other.imports());
    assert_eq!(
        other.commit(commit(&p, vec![ImportAction::Create])),
        Err(ServiceError::NotFound)
    );
    service.cancel(&p.id).unwrap();
    assert_eq!(
        service.commit(commit(&p, vec![ImportAction::Create])),
        Err(ServiceError::NotFound)
    );
    drop(service);
    assert_eq!(hash(&dir.database_path()), initial);
    assert_eq!(hash(&wal), wal_initial);
    assert_eq!(hash(&input), source);
    assert!(db.papers().list(&PaperList::default()).unwrap().is_empty());
}
#[test]
fn retained_payload_full_metadata_keys_unicode_and_replay() {
    let mut db = LibraryDb::in_memory().unwrap();
    let mut d = doi(" Unicode Ω 日本語 ", "10.99/create");
    d.bibliography.journal = Some("誌名".into());
    d.bibliography.extra_json = Some(serde_json::json!({"custom":"Ω"}));
    d.source.row = Some(9);
    let mut service = ImportCoordinator::new(db.imports());
    let mut p = service.preview(req(vec![d])).unwrap();
    p.candidates[0].draft.title = "Client override".into();
    let result = service
        .commit(commit(&p, vec![ImportAction::Create]))
        .unwrap();
    assert_eq!(result.rows[0].source.row, Some(9));
    assert_eq!(
        service.commit(commit(&p, vec![ImportAction::Create])),
        Err(ServiceError::NotFound)
    );
    drop(service);
    let saved = db.papers().get(result.created[0]).unwrap().unwrap();
    assert_eq!(saved.title, "Unicode Ω 日本語");
    assert_eq!(saved.bibliography.journal.as_deref(), Some("誌名"));
    assert_eq!(
        saved.bibliography.extra_json,
        Some(serde_json::json!({"custom":"Ω"}))
    );
    assert!(saved.bibliography.citation_key.is_some());
}
#[test]
fn exact_batch_duplicates_trash_and_similarity_are_reviewed_explicitly() {
    let mut db = LibraryDb::in_memory().unwrap();
    let d = doi("Same batch", "10.99/batch");
    let mut s = ImportCoordinator::new(db.imports());
    let p = s.preview(req(vec![d.clone(), d])).unwrap();
    assert_eq!(p.candidates[0].batch_matches, vec![1]);
    assert_eq!(
        s.commit(commit(&p, vec![ImportAction::Create, ImportAction::Create])),
        Err(ServiceError::Conflict)
    );
    let o = s
        .commit(commit(&p, vec![ImportAction::Create, ImportAction::Skip]))
        .unwrap();
    drop(s);
    db.papers().set_deleted(o.created[0], true).unwrap();
    let mut s = ImportCoordinator::new(db.imports());
    let p = s
        .preview(req(vec![doi(
            "Trashed duplicate",
            "https://doi.org/10.99/BATCH",
        )]))
        .unwrap();
    assert!(p.candidates[0].matches[0].existing.deleted_at.is_some());
    assert_eq!(
        s.commit(commit(&p, vec![ImportAction::Create])),
        Err(ServiceError::Conflict)
    );
    assert!(
        s.commit(commit(
            &p,
            vec![ImportAction::Merge {
                match_index: 0,
                fields: vec![]
            }]
        ))
        .is_err()
    );
    s.cancel(&p.id).unwrap();
    drop(s);
    db.papers()
        .create(NewPaper::titled("Solid electrolyte transport review"))
        .unwrap();
    let mut s = ImportCoordinator::new(db.imports());
    let p = s
        .preview(req(vec![PaperDraft::titled(
            "Solid electrolyte transport reviews",
        )]))
        .unwrap();
    assert_eq!(p.candidates[0].matches[0].kind, MatchKind::SimilarTitle);
    assert_eq!(p.candidates[0].existing_match, None);
    s.commit(commit(&p, vec![ImportAction::Create])).unwrap();
}
#[test]
fn normalized_external_ids_doi_disagreement_and_citation_collisions() {
    let mut db = LibraryDb::in_memory().unwrap();
    let mut old = NewPaper::titled("Existing");
    old.bibliography.doi = Some("10.99/old".into());
    old.bibliography.citation_key = Some("KeepKey".into());
    old.external_ids = vec![
        ExternalId {
            namespace: "PMID".into(),
            value: "00123".into(),
        },
        ExternalId {
            namespace: "arxiv".into(),
            value: "2601.12345v2".into(),
        },
    ];
    db.papers().create(old).unwrap();
    let mut bad = doi("Disagree", "10.99/different");
    bad.external_ids = vec![ExternalId {
        namespace: "pmid".into(),
        value: "123".into(),
    }];
    let mut s = ImportCoordinator::new(db.imports());
    let p = s.preview(req(vec![bad])).unwrap();
    assert_eq!(p.candidates[0].matches[0].kind, MatchKind::ExternalId);
    assert!(
        s.commit(commit(
            &p,
            vec![ImportAction::Merge {
                match_index: 0,
                fields: vec!["title".into()]
            }]
        ))
        .is_err()
    );
    s.cancel(&p.id).unwrap();
    let mut a = PaperDraft::titled("Independent Alpha");
    a.bibliography.citation_key = Some("KeepKey".into());
    let mut b = PaperDraft::titled("Independent Beta");
    b.bibliography.citation_key = Some("KeepKey".into());
    let p = s.preview(req(vec![a, b])).unwrap();
    assert_eq!(
        p.candidates[0].draft.bibliography.citation_key.as_deref(),
        Some("KeepKey_2")
    );
    assert_eq!(
        p.candidates[1].draft.bibliography.citation_key.as_deref(),
        Some("KeepKey_3")
    );
    assert!(!p.candidates[0].diagnostics.is_empty());
    s.commit(commit(&p, vec![ImportAction::Create, ImportAction::Create]))
        .unwrap();
}
#[test]
fn concurrent_changes_invalidate_preview_including_aba_and_destination_deletion() {
    for mutation in ["paper", "aba", "doi", "external", "key", "destination"] {
        let (_temp, dir, mut db) = disk();
        let existing = db.papers().create(NewPaper::titled("Original")).unwrap();
        let collection = db
            .collections()
            .create(CollectionInput::named("Target"))
            .unwrap();
        let mut s = ImportCoordinator::new(db.imports());
        let p = s
            .preview(ImportRequest {
                candidates: vec![doi("Incoming", "10.99/race")],
                destination: ImportDestination {
                    collection: Some(collection.id),
                    tags: vec![],
                },
            })
            .unwrap();
        let raw = Connection::open(dir.database_path()).unwrap();
        raw.pragma_update(None, "foreign_keys", true).unwrap();
        match mutation {
            "paper" => {
                raw.execute(
                    "UPDATE papers SET title='Changed' WHERE id=?1",
                    [existing.id.0.to_string()],
                )
                .unwrap();
            }
            "aba" => {
                for title in ["Changed", "Original"] {
                    raw.execute(
                        "UPDATE papers SET title=?1 WHERE id=?2",
                        params![title, existing.id.0.to_string()],
                    )
                    .unwrap();
                }
            }
            "doi" => {
                raw.execute(
                    "UPDATE papers SET doi_normalized='10.99/race' WHERE id=?1",
                    [existing.id.0.to_string()],
                )
                .unwrap();
            }
            "external" => {
                raw.execute(
                    "INSERT INTO paper_external_ids VALUES (?1,'pmid','44',0)",
                    [existing.id.0.to_string()],
                )
                .unwrap();
            }
            "key" => {
                raw.execute(
                    "UPDATE papers SET citation_key=?1 WHERE id=?2",
                    params![
                        p.candidates[0].draft.bibliography.citation_key,
                        existing.id.0.to_string()
                    ],
                )
                .unwrap();
            }
            _ => {
                raw.execute(
                    "DELETE FROM collections WHERE id=?1",
                    [collection.id.0.to_string()],
                )
                .unwrap();
            }
        }
        assert!(
            matches!(
                s.commit(commit(&p, vec![ImportAction::Create])),
                Err(ServiceError::Conflict | ServiceError::NotFound)
            ),
            "{mutation}"
        );
        assert_eq!(count(&raw, "papers"), 1);
    }
}
#[test]
fn midbatch_paper_author_and_membership_failures_roll_back_every_write_and_retry() {
    for failure in ["paper", "author", "membership"] {
        let (_temp, dir, mut db) = disk();
        let c = db
            .collections()
            .create(CollectionInput::named("Target"))
            .unwrap();
        let raw = Connection::open(dir.database_path()).unwrap();
        let trigger = match failure {
            "paper" => {
                "CREATE TRIGGER fail_import BEFORE INSERT ON papers WHEN NEW.title='fail' BEGIN SELECT RAISE(ABORT,'injected paper failure'); END"
            }
            "author" => {
                "CREATE TRIGGER fail_import BEFORE INSERT ON paper_people WHEN (SELECT title FROM papers WHERE id=NEW.paper_id)='fail' BEGIN SELECT RAISE(ABORT,'injected relationship failure'); END"
            }
            _ => {
                "CREATE TRIGGER fail_import BEFORE INSERT ON paper_collections WHEN (SELECT title FROM papers WHERE id=NEW.paper_id)='fail' BEGIN SELECT RAISE(ABORT,'injected membership failure'); END"
            }
        };
        raw.execute_batch(trigger).unwrap();
        let mut last = doi("fail", "10.99/fail");
        last.bibliography.authors = vec![Contributor {
            given_name: None,
            family_name: Some("Synthetic".into()),
            literal_name: None,
            orcid: None,
            role: "author".into(),
        }];
        let request = ImportRequest {
            candidates: vec![doi("First", "10.99/first"), last],
            destination: ImportDestination {
                collection: Some(c.id),
                tags: vec![],
            },
        };
        let mut s = ImportCoordinator::new(db.imports());
        let p = s.preview(request.clone()).unwrap();
        assert!(
            s.commit(commit(&p, vec![ImportAction::Create, ImportAction::Create]))
                .is_err()
        );
        for table in [
            "papers",
            "people",
            "paper_people",
            "paper_external_ids",
            "paper_collections",
        ] {
            assert_eq!(count(&raw, table), 0, "{failure}/{table}");
        }
        // A failed write conservatively invalidates local revisions; retry is fresh preview.
        assert_eq!(
            s.commit(commit(&p, vec![ImportAction::Create, ImportAction::Create])),
            Err(ServiceError::Conflict)
        );
        s.cancel(&p.id).unwrap();
        raw.execute_batch("DROP TRIGGER fail_import").unwrap();
        let p = s.preview(request).unwrap();
        s.commit(commit(&p, vec![ImportAction::Create, ImportAction::Create]))
            .unwrap();
        assert_eq!(count(&raw, "papers"), 2);
        assert_eq!(count(&raw, "paper_collections"), 2);
    }
}
#[test]
fn explicit_merge_preserves_ids_contributors_metadata_relationships_and_original_files() {
    let (temp, dir, mut db) = disk();
    let mut old = NewPaper::titled("Old title");
    old.bibliography.doi = Some("10.99/merge".into());
    old.bibliography.journal = Some("原誌名".into());
    old.bibliography.extra_json = Some(serde_json::json!({"custom":"keep"}));
    old.bibliography.authors = vec![Contributor {
        given_name: Some("A".into()),
        family_name: Some("菅野".into()),
        literal_name: None,
        orcid: Some("keep-orcid".into()),
        role: "editor".into(),
    }];
    old.external_ids = vec![ExternalId {
        namespace: "opaque".into(),
        value: "Original".into(),
    }];
    let old = db.papers().create(old).unwrap();
    let collection = db
        .collections()
        .create(CollectionInput::named("Original collection"))
        .unwrap();
    db.collections()
        .set_membership(old.id, collection.id, true)
        .unwrap();
    let tag = db.tags().create(TagInput::named(" New　TAG ")).unwrap();
    let source = temp.path().canonicalize().unwrap().join("original.pdf");
    std::fs::write(&source, b"synthetic PDF bytes, no parser claim").unwrap();
    let attachment = AttachmentService::new(db.managed_attachments(&dir).unwrap())
        .import(ImportAttachment {
            paper_id: old.id,
            source: source.clone(),
            original_name: "original.pdf".into(),
            media_type: "application/pdf".into(),
            expected_sha256: None,
        })
        .unwrap();
    let blob = dir.path().join(attachment.storage_key.as_str());
    let blob_hash = hash(&blob);
    let source_hash = hash(&source);
    let raw = Connection::open(dir.database_path()).unwrap();
    let person: String = raw
        .query_row("SELECT person_id FROM paper_people", [], |r| r.get(0))
        .unwrap();
    let mut incoming = doi("New title", "10.99/merge");
    incoming.bibliography.abstract_text = Some("Explicit abstract".into());
    let mut s = ImportCoordinator::new(db.imports());
    let p = s
        .preview(ImportRequest {
            candidates: vec![incoming],
            destination: ImportDestination {
                collection: None,
                tags: vec![tag.id, tag.id],
            },
        })
        .unwrap();
    assert!(
        s.commit(commit(
            &p,
            vec![ImportAction::Merge {
                match_index: 0,
                fields: vec!["not-reviewed".into()]
            }]
        ))
        .is_err()
    );
    assert!(
        s.commit(commit(
            &p,
            vec![ImportAction::Merge {
                match_index: 0,
                fields: vec!["title".into(), "title".into()]
            }]
        ))
        .is_err()
    );
    let result = s
        .commit(commit(
            &p,
            vec![ImportAction::Merge {
                match_index: 0,
                fields: vec!["title".into(), "abstract".into()],
            }],
        ))
        .unwrap();
    assert_eq!(result.merged, vec![old.id]);
    drop(s);
    let new = db.papers().get(old.id).unwrap().unwrap();
    let mut expected = old.clone();
    expected.title = "New title".into();
    expected.bibliography.abstract_text = Some("Explicit abstract".into());
    expected.updated_at = new.updated_at.clone();
    assert_eq!(new, expected);
    assert_eq!(
        raw.query_row("SELECT person_id FROM paper_people", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        person
    );
    assert_eq!(
        db.collections().for_paper(old.id).unwrap()[0].id,
        collection.id
    );
    assert_eq!(db.tags().for_paper(old.id).unwrap()[0].id, tag.id);
    assert_eq!(
        db.attachments().for_paper(old.id).unwrap(),
        vec![attachment]
    );
    assert_eq!(hash(&blob), blob_hash);
    assert_eq!(hash(&source), source_hash);
    drop(db);
    let mut reopened = LibraryDb::open(&dir).unwrap();
    assert_eq!(reopened.papers().get(old.id).unwrap(), Some(new));
}
#[test]
fn preview_candidate_budget_fails_explicitly_without_truncating_legacy_fields() {
    let mut db = LibraryDb::in_memory().unwrap();
    let mut legacy = Vec::new();
    for i in 0..21 {
        let mut d = NewPaper::titled(format!("Legacy {i}"));
        d.external_ids = vec![ExternalId {
            namespace: "pmid".into(),
            value: "42".into(),
        }];
        legacy.push(db.papers().create(d).unwrap());
    }
    let mut d = PaperDraft::titled("Duplicate inventory");
    d.external_ids = vec![ExternalId {
        namespace: "PMID".into(),
        value: "42".into(),
    }];
    let mut s = ImportCoordinator::new(db.imports());
    assert!(matches!(
        s.preview(req(vec![d.clone()])),
        Err(ServiceError::InvalidInput(_))
    ));
    drop(s);
    let mut last = legacy.pop().unwrap();
    last.external_ids[0].value = "43".into();
    db.papers().update(&last).unwrap();
    let mut s = ImportCoordinator::new(db.imports());
    assert_eq!(
        s.preview(req(vec![d])).unwrap().candidates[0].matches.len(),
        20
    );
}

#[test]
fn stale_merge_target_and_mixed_merge_create_rollback_preserve_reviewed_paper() {
    let (_temp, dir, mut db) = disk();
    let mut input = NewPaper::titled("Original merge target");
    input.bibliography.doi = Some("10.99/target".into());
    let old = db.papers().create(input).unwrap();
    let raw = Connection::open(dir.database_path()).unwrap();
    raw.execute_batch("CREATE TRIGGER fail_mixed BEFORE INSERT ON papers WHEN NEW.title='fail' BEGIN SELECT RAISE(ABORT,'injected failure'); END").unwrap();
    let mut s = ImportCoordinator::new(db.imports());
    let p = s
        .preview(req(vec![
            doi("Approved title", "10.99/target"),
            PaperDraft::titled("fail"),
        ]))
        .unwrap();
    assert!(
        s.commit(commit(
            &p,
            vec![
                ImportAction::Merge {
                    match_index: 0,
                    fields: vec!["title".into()]
                },
                ImportAction::Create
            ]
        ))
        .is_err()
    );
    assert_eq!(
        raw.query_row(
            "SELECT title FROM papers WHERE id=?1",
            [old.id.0.to_string()],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        old.title
    );
    assert_eq!(count(&raw, "papers"), 1);
    s.cancel(&p.id).unwrap();
    let p = s
        .preview(req(vec![doi("Approved title", "10.99/target")]))
        .unwrap();
    raw.execute(
        "UPDATE papers SET title='Concurrent edit' WHERE id=?1",
        [old.id.0.to_string()],
    )
    .unwrap();
    assert_eq!(
        s.commit(commit(
            &p,
            vec![ImportAction::Merge {
                match_index: 0,
                fields: vec!["title".into()]
            }]
        )),
        Err(ServiceError::Conflict)
    );
}
#[test]
fn duplicate_merge_targets_and_unreviewed_indices_are_rejected_before_writes() {
    let mut db = LibraryDb::in_memory().unwrap();
    let mut input = NewPaper::titled("Existing");
    input.bibliography.doi = Some("10.99/target".into());
    db.papers().create(input).unwrap();
    let mut s = ImportCoordinator::new(db.imports());
    let p = s
        .preview(req(vec![
            doi("A", "10.99/target"),
            doi("B", "10.99/target"),
        ]))
        .unwrap();
    for decisions in [
        vec![
            ImportDecision {
                candidate_index: 99,
                action: ImportAction::Skip,
            },
            ImportDecision {
                candidate_index: 1,
                action: ImportAction::Skip,
            },
        ],
        vec![
            ImportDecision {
                candidate_index: 0,
                action: ImportAction::Merge {
                    match_index: 99,
                    fields: vec![],
                },
            },
            ImportDecision {
                candidate_index: 1,
                action: ImportAction::Skip,
            },
        ],
        vec![
            ImportDecision {
                candidate_index: 0,
                action: ImportAction::Merge {
                    match_index: 0,
                    fields: vec![],
                },
            },
            ImportDecision {
                candidate_index: 1,
                action: ImportAction::Merge {
                    match_index: 0,
                    fields: vec![],
                },
            },
        ],
    ] {
        assert!(matches!(
            s.commit(ImportCommit {
                preview_id: p.id.clone(),
                decisions
            }),
            Err(ServiceError::InvalidInput(_))
        ));
    }
}
