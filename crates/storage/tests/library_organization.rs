use cm_core::{
    ServiceError,
    collections::{CollectionInput, CollectionLibrary, CollectionService, DestinationLibrary},
    import::ImportDestination,
    library_query::{
        LibraryFilter, LibraryQuery, LibraryQueryRepository, LibraryQueryService, LibrarySort,
        SortDirection, SortField, TagMatch,
    },
    papers::{NewPaper, PaperLibrary, PaperService, PaperVisibility},
    ports::{AttachmentRepository, PaperRepository},
    tags::{TagInput, TagLibrary, TagService},
};
use cm_domain::{Bibliography, CollectionId, Contributor, ExternalId, Paper, PaperId, TagId};
use cm_storage::{LibraryDb, LibraryDirectory};
use rusqlite::{Connection, params};
use uuid::Uuid;

fn library() -> (tempfile::TempDir, LibraryDirectory, LibraryDb) {
    let temp = tempfile::tempdir().unwrap();
    let directory =
        LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("library")).unwrap();
    let db = LibraryDb::open(&directory).unwrap();
    (temp, directory, db)
}
fn invalid<T: std::fmt::Debug>(result: Result<T, ServiceError>) {
    assert!(
        matches!(result, Err(ServiceError::InvalidInput(_))),
        "{result:?}"
    );
}
fn child(name: &str, parent: CollectionId) -> CollectionInput {
    CollectionInput {
        name: name.into(),
        parent: Some(parent),
        sort_order: 0,
    }
}
fn ids(db: &LibraryDb, filter: LibraryFilter) -> Vec<PaperId> {
    LibraryQueryService::new(db.library_query())
        .query(&LibraryQuery {
            filter,
            ..Default::default()
        })
        .unwrap()
        .papers
        .into_iter()
        .map(|p| p.id)
        .collect()
}
fn complete() -> NewPaper {
    NewPaper {
        title: "固体電解質 Énergie 100%_\\'".into(),
        external_ids: vec![
            ExternalId {
                namespace: "custom".into(),
                value: "Ω-opaque".into(),
            },
            ExternalId {
                namespace: "DOI".into(),
                value: "10.1234/ABC".into(),
            },
        ],
        bibliography: Bibliography {
            paper_type: Some("journal-article".into()),
            abstract_text: Some("日本語の要旨 uniqueabstract".into()),
            year: Some(2026),
            publication_date: Some("2026-11".into()),
            journal: Some("Journal独自".into()),
            volume: Some("12".into()),
            issue: Some("3".into()),
            pages: Some("S1–S12".into()),
            doi: Some("10.1234/abc".into()),
            url: Some("https://example.org/uniqueurl".into()),
            language: Some("ja".into()),
            citation_key: Some("Kanno2026".into()),
            authors: vec![
                Contributor {
                    given_name: Some("Ryoji".into()),
                    family_name: Some("菅野".into()),
                    literal_name: None,
                    orcid: Some("0000-0002-1825-0097".into()),
                    role: "author".into(),
                },
                Contributor {
                    given_name: None,
                    family_name: None,
                    literal_name: Some("編集 Équipe".into()),
                    orcid: None,
                    role: "editor".into(),
                },
                Contributor {
                    given_name: Some("Second".into()),
                    family_name: Some("著者".into()),
                    literal_name: None,
                    orcid: None,
                    role: "author".into(),
                },
            ],
            extra_json: Some(
                serde_json::json!({"unknown":[1,"二",null,true],"large":18446744073709551615u64}),
            ),
        },
    }
}
/// Metadata-only schema fixtures; #9 file-store is deliberately not exercised.
fn attachment(raw: &Connection, paper: PaperId, kind: &str, mime: &str) {
    let id = Uuid::now_v7().to_string();
    raw.execute("INSERT INTO attachments(id,paper_id,kind,file_hash,relative_path,original_name,mime_type,byte_size,added_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![id, paper.0.to_string(), kind, vec![7u8;32], format!("fixture/{id}"), "fixture", mime, 123, "2026-11-06T00:00:00Z"]).unwrap();
}

#[test]
fn hierarchy_cycles_multiple_memberships_safe_delete_and_reopen() {
    let (_temp, dir, mut db) = library();
    let paper = db.papers().create(complete()).unwrap();
    let raw = Connection::open(dir.database_path()).unwrap();
    attachment(&raw, paper.id, "pdf", "application/pdf");
    let mut service = CollectionService::new(db.collections());
    let root = service
        .create(CollectionInput::named("\u{3000}研究\t資料 "))
        .unwrap();
    assert_eq!(root.name, "研究 資料");
    let branch = service.create(child("branch", root.id)).unwrap();
    let leaf = service.create(child("leaf", branch.id)).unwrap();
    let other = service.create(CollectionInput::named("other")).unwrap();
    assert_eq!(root.id.0.get_version_num(), 7);
    assert_eq!(service.list().unwrap().len(), 4);
    for parent in [root.id, branch.id, leaf.id] {
        invalid(service.update(root.id, child("cycle", parent)));
    }
    assert_eq!(service.get(root.id).unwrap(), root);
    for collection in [root.id, leaf.id, other.id] {
        service.add_paper(paper.id, collection).unwrap();
        service.add_paper(paper.id, collection).unwrap();
    }
    assert_eq!(service.for_paper(paper.id).unwrap().len(), 3);
    service.remove_paper(paper.id, other.id).unwrap();
    service.remove_paper(paper.id, other.id).unwrap();
    assert_eq!(service.for_paper(paper.id).unwrap().len(), 2);
    let moved = service
        .update(
            branch.id,
            CollectionInput {
                name: "moved".into(),
                parent: Some(other.id),
                sort_order: -5,
            },
        )
        .unwrap();
    assert_eq!(moved.parent, Some(other.id));
    assert_eq!(service.get(leaf.id).unwrap().parent, Some(branch.id));
    service.delete(branch.id).unwrap();
    assert_eq!(service.get(leaf.id).unwrap().parent, Some(other.id));
    service.delete(root.id).unwrap(); // its membership only
    assert_eq!(
        service
            .for_paper(paper.id)
            .unwrap()
            .iter()
            .map(|c| c.id)
            .collect::<Vec<_>>(),
        vec![leaf.id]
    );
    service.delete(other.id).unwrap(); // leaf is promoted to root
    assert_eq!(service.get(leaf.id).unwrap().parent, None);
    assert_eq!(service.delete(other.id), Err(ServiceError::NotFound));
    invalid(service.create(CollectionInput::named(" \u{3000}")));
    let missing = CollectionId(Uuid::now_v7());
    assert_eq!(
        service.create(child("missing parent", missing)),
        Err(ServiceError::NotFound)
    );
    invalid(service.get(CollectionId(Uuid::nil())));
    assert_eq!(db.papers().get(paper.id).unwrap(), Some(paper.clone()));
    assert_eq!(db.attachments().for_paper(paper.id).unwrap().len(), 1);
    drop(db);
    let mut db = LibraryDb::open(&dir).unwrap();
    assert_eq!(db.collections().for_paper(paper.id).unwrap()[0].id, leaf.id);
    assert_eq!(db.papers().get(paper.id).unwrap(), Some(paper));
    assert_eq!(
        db.attachments()
            .for_paper(
                db.library_query()
                    .query(&LibraryQuery::default())
                    .unwrap()
                    .papers[0]
                    .id
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn tag_collisions_rename_set_membership_delete_and_import_destination() {
    let (_temp, dir, mut db) = library();
    let paper = db.papers().create(complete()).unwrap();
    let original = db
        .tags()
        .create(TagInput {
            name: " ÉNERGIE\u{3000}電池 ".into(),
            color: Some("#aB12fF".into()),
        })
        .unwrap();
    assert_eq!(original.name, "ÉNERGIE 電池");
    assert_eq!(original.key.as_str(), "énergie 電池");
    assert_eq!(
        db.tags().create(TagInput::named("énergie\t電池")),
        Err(ServiceError::Conflict)
    );
    let other = db.tags().create(TagInput::named("other")).unwrap();
    assert_eq!(
        db.tags().update(other.id, TagInput::named("Énergie 電池")),
        Err(ServiceError::Conflict)
    );
    assert_eq!(db.tags().get(other.id).unwrap(), Some(other.clone()));
    let mut tags = TagService::new(db.tags());
    assert_eq!(tags.find("énergie\n電池").unwrap(), Some(original.clone()));
    assert_eq!(tags.list().unwrap().len(), 2);
    for raw in ["", "\u{3000}\n", "x\0y"] {
        invalid(tags.create(TagInput::named(raw)));
        invalid(tags.find(raw));
    }
    for color in ["red", "#fff", "#gggggg", "1234567", "#１２３４５６"] {
        invalid(tags.create(TagInput {
            name: "valid".into(),
            color: Some(color.into()),
        }));
    }
    tags.add_paper(paper.id, original.id).unwrap();
    tags.add_paper(paper.id, original.id).unwrap();
    tags.remove_paper(paper.id, other.id).unwrap();
    tags.remove_paper(paper.id, other.id).unwrap();
    let renamed = tags.update(original.id, TagInput::named("重要")).unwrap();
    assert_eq!(tags.for_paper(paper.id).unwrap(), vec![renamed.clone()]);
    assert!(tags.find("énergie 電池").unwrap().is_none());
    assert_eq!(tags.find("重要").unwrap(), Some(renamed.clone()));
    let collection = db
        .collections()
        .create(CollectionInput::named("dest"))
        .unwrap();
    let destination = ImportDestination {
        collection: Some(collection.id),
        tags: vec![original.id, other.id, other.id],
    };
    db.collections().validate_destination(&destination).unwrap();
    db.collections()
        .apply_destination(paper.id, &destination)
        .unwrap();
    db.collections()
        .apply_destination(paper.id, &destination)
        .unwrap();
    assert_eq!(db.tags().for_paper(paper.id).unwrap().len(), 2);
    db.tags().delete(other.id).unwrap();
    assert_eq!(db.tags().delete(other.id), Err(ServiceError::NotFound));
    // Preview destination went stale: reject before any link writes.
    let second = db.papers().create(NewPaper::titled("second")).unwrap();
    assert_eq!(
        db.collections().apply_destination(second.id, &destination),
        Err(ServiceError::NotFound)
    );
    assert!(db.collections().for_paper(second.id).unwrap().is_empty());
    assert!(db.tags().for_paper(second.id).unwrap().is_empty());
    db.papers().set_deleted(paper.id, true).unwrap();
    assert_eq!(db.tags().for_paper(paper.id), Err(ServiceError::NotFound));
    assert_eq!(
        db.collections().for_paper(paper.id),
        Err(ServiceError::NotFound)
    );
    assert_eq!(
        db.tags().set_membership(paper.id, original.id, false),
        Err(ServiceError::NotFound)
    );
    assert_eq!(
        db.collections()
            .set_membership(paper.id, collection.id, true),
        Err(ServiceError::NotFound)
    );
    assert_eq!(
        db.collections()
            .apply_destination(paper.id, &ImportDestination::default()),
        Err(ServiceError::NotFound)
    );
    db.papers().set_deleted(paper.id, false).unwrap();
    assert_eq!(
        db.tags().for_paper(paper.id).unwrap(),
        vec![renamed.clone()]
    );
    drop(db);
    let mut db = LibraryDb::open(&dir).unwrap();
    assert_eq!(db.tags().for_paper(paper.id).unwrap(), vec![renamed]);
    assert_eq!(
        db.collections().for_paper(paper.id).unwrap(),
        vec![collection]
    );
    db.papers().set_deleted(paper.id, true).unwrap();
    db.tags().delete(original.id).unwrap(); // remove links even in trash
    db.papers().set_deleted(paper.id, false).unwrap();
    assert!(db.tags().for_paper(paper.id).unwrap().is_empty());
    assert_eq!(
        db.papers().get(paper.id).unwrap().unwrap().bibliography,
        paper.bibliography
    );
}

#[test]
fn transaction_failures_roll_back_child_promotion_and_destination_memberships() {
    let (_temp, dir, mut db) = library();
    let root = db
        .collections()
        .create(CollectionInput::named("root"))
        .unwrap();
    let child = db.collections().create(child("child", root.id)).unwrap();
    let paper = db.papers().create(complete()).unwrap();
    let t1 = db.tags().create(TagInput::named("first")).unwrap();
    let t2 = db.tags().create(TagInput::named("second")).unwrap();
    db.collections()
        .set_membership(paper.id, root.id, true)
        .unwrap();
    let fault = Connection::open(dir.database_path()).unwrap();
    fault.execute_batch("CREATE TRIGGER fail_collection_delete BEFORE DELETE ON collections BEGIN SELECT RAISE(ABORT,'fixture'); END").unwrap();
    invalid(db.collections().delete(root.id));
    assert_eq!(
        db.collections().get(child.id).unwrap().unwrap().parent,
        Some(root.id)
    );
    assert_eq!(
        db.collections().for_paper(paper.id).unwrap(),
        vec![root.clone()]
    );
    // The rejected ID is bound as data via a fixture lookup table.
    fault.execute_batch("CREATE TABLE fixture_reject_tag(id TEXT); CREATE TRIGGER fail_tag_insert BEFORE INSERT ON paper_tags WHEN NEW.tag_id IN (SELECT id FROM fixture_reject_tag) BEGIN SELECT RAISE(ABORT,'fixture'); END").unwrap();
    fault
        .execute(
            "INSERT INTO fixture_reject_tag VALUES (?1)",
            [t2.id.0.to_string()],
        )
        .unwrap();
    let destination = ImportDestination {
        collection: Some(child.id),
        tags: vec![t1.id, t2.id],
    };
    invalid(db.collections().apply_destination(paper.id, &destination));
    assert_eq!(db.collections().for_paper(paper.id).unwrap(), vec![root]);
    assert!(db.tags().for_paper(paper.id).unwrap().is_empty());
    drop(db);
    let mut db = LibraryDb::open(&dir).unwrap();
    assert_eq!(db.collections().get(child.id).unwrap(), Some(child));
    assert!(db.tags().for_paper(paper.id).unwrap().is_empty());
    assert_eq!(db.papers().get(paper.id).unwrap(), Some(paper));
}

#[test]
fn common_query_combines_predicates_literal_unicode_keywords_and_trash() {
    let (_temp, dir, mut db) = library();
    let full = db.papers().create(complete()).unwrap();
    let mut second = NewPaper::titled("other electrolyte");
    second.bibliography.year = Some(2026);
    second.bibliography.authors = vec![Contributor {
        literal_name: Some("Ryoji 組織".into()),
        given_name: None,
        family_name: None,
        orcid: None,
        role: "author".into(),
    }];
    let second = db.papers().create(second).unwrap();
    let mut third = NewPaper::titled("book");
    third.bibliography.year = Some(2020);
    third.bibliography.paper_type = Some("book".into());
    let third = db.papers().create(third).unwrap();
    let fourth = db.papers().create(NewPaper::titled("no date")).unwrap();
    let deleted = db.papers().create(NewPaper::titled("trash")).unwrap();
    let c1 = db
        .collections()
        .create(CollectionInput::named("C1"))
        .unwrap();
    let c2 = db.collections().create(child("C2", c1.id)).unwrap();
    let a = db.tags().create(TagInput::named("shared")).unwrap();
    let b = db.tags().create(TagInput::named("日本語 タグ")).unwrap();
    for (p, cs, ts) in [
        (full.id, vec![c1.id, c2.id], vec![a.id, b.id]),
        (second.id, vec![c1.id], vec![a.id]),
        (third.id, vec![c2.id], vec![b.id]),
        (deleted.id, vec![c1.id], vec![a.id, b.id]),
    ] {
        for c in cs {
            db.collections().set_membership(p, c, true).unwrap();
        }
        for t in ts {
            db.tags().set_membership(p, t, true).unwrap();
        }
    }
    let raw = Connection::open(dir.database_path()).unwrap();
    attachment(&raw, full.id, "pdf", "application/pdf");
    attachment(&raw, full.id, "pdf", "application/pdf");
    attachment(&raw, second.id, "pdf", "application/octet-stream");
    attachment(&raw, third.id, "supplement", "application/pdf");
    attachment(&raw, deleted.id, "pdf", "application/pdf");
    db.papers().set_deleted(deleted.id, true).unwrap();
    let tags = |mode, tags| LibraryFilter {
        tag_match: mode,
        tags,
        ..Default::default()
    };
    assert_eq!(
        ids(&db, tags(TagMatch::All, vec![a.id, b.id])),
        vec![full.id]
    );
    assert_eq!(
        ids(&db, tags(TagMatch::Any, vec![a.id, b.id])),
        vec![full.id, second.id, third.id]
    );
    assert_eq!(
        ids(&db, tags(TagMatch::All, vec![a.id, a.id])),
        vec![full.id, second.id]
    );
    assert_eq!(
        ids(&db, tags(TagMatch::Any, vec![])),
        vec![full.id, second.id, third.id, fourth.id]
    );
    assert_eq!(
        ids(&db, tags(TagMatch::All, vec![TagId(Uuid::now_v7())])),
        vec![]
    );
    assert_eq!(
        ids(
            &db,
            LibraryFilter {
                collection: Some(c1.id),
                ..Default::default()
            }
        ),
        vec![full.id, second.id]
    );
    assert_eq!(
        ids(
            &db,
            LibraryFilter {
                collection: Some(c2.id),
                ..Default::default()
            }
        ),
        vec![full.id, third.id]
    );
    assert_eq!(
        ids(
            &db,
            LibraryFilter {
                has_pdf: Some(true),
                ..Default::default()
            }
        ),
        vec![full.id]
    );
    assert_eq!(
        ids(
            &db,
            LibraryFilter {
                has_pdf: Some(false),
                ..Default::default()
            }
        ),
        vec![second.id, third.id, fourth.id]
    );
    assert_eq!(
        ids(
            &db,
            LibraryFilter {
                year_from: Some(2020),
                year_to: Some(2026),
                ..Default::default()
            }
        ),
        vec![full.id, second.id, third.id]
    );
    assert_eq!(
        ids(
            &db,
            LibraryFilter {
                year_to: Some(2020),
                paper_type: Some("book".into()),
                ..Default::default()
            }
        ),
        vec![third.id]
    );
    assert!(
        ids(
            &db,
            LibraryFilter {
                paper_type: Some("BOOK".into()),
                ..Default::default()
            }
        )
        .is_empty()
    );
    assert_eq!(
        ids(
            &db,
            LibraryFilter {
                author: Some("RYOJI".into()),
                ..Default::default()
            }
        ),
        vec![full.id, second.id]
    );
    assert_eq!(
        ids(
            &db,
            LibraryFilter {
                author: Some("菅野 Ryoji".into()),
                ..Default::default()
            }
        ),
        vec![full.id]
    );
    assert!(
        ids(
            &db,
            LibraryFilter {
                author: Some("編集".into()),
                ..Default::default()
            }
        )
        .is_empty()
    );
    for word in [
        "電解",
        "uniqueabstract",
        "Journal独自",
        "10.1234/ABC",
        "uniqueurl",
        "Kanno2026",
        "Ω-opaque",
        "編集",
        "S1–S12",
        "100%_\\'",
        "Énergie",
        "2026-11",
    ] {
        assert_eq!(
            ids(
                &db,
                LibraryFilter {
                    keyword: Some(word.into()),
                    ..Default::default()
                }
            ),
            vec![full.id],
            "{word}"
        );
    }
    assert_eq!(
        ids(
            &db,
            LibraryFilter {
                keyword: Some("日本語 タグ".into()),
                ..Default::default()
            }
        ),
        vec![full.id, third.id]
    );
    // SQL metacharacters are data, ASCII case is folded; Unicode case/composition is literal.
    for word in [
        "' OR 1=1 --",
        "ω-opaque",
        "énergie",
        "E\u{301}nergie",
        "%_%",
    ] {
        assert!(
            ids(
                &db,
                LibraryFilter {
                    keyword: Some(word.into()),
                    ..Default::default()
                }
            )
            .is_empty(),
            "{word}"
        );
    }
    let filter = LibraryFilter {
        collection: Some(c1.id),
        tags: vec![a.id, b.id],
        tag_match: TagMatch::All,
        author: Some("菅野".into()),
        year_from: Some(2026),
        year_to: Some(2026),
        paper_type: Some("journal-article".into()),
        has_pdf: Some(true),
        keyword: Some("電解 uniqueabstract".into()),
        ..Default::default()
    };
    let page = db
        .library_query()
        .query(&LibraryQuery {
            filter: filter.clone(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.papers, vec![full.clone()]);
    assert!(page.next.is_none());
    db.papers().set_deleted(full.id, true).unwrap();
    assert!(ids(&db, filter.clone()).is_empty());
    let trash_filter = LibraryFilter {
        visibility: PaperVisibility::Deleted,
        ..filter.clone()
    };
    assert_eq!(ids(&db, trash_filter.clone()), vec![full.id]);
    let trash = db
        .library_query()
        .query(&LibraryQuery {
            filter: trash_filter,
            ..Default::default()
        })
        .unwrap()
        .papers
        .remove(0);
    assert!(trash.deleted_at.is_some());
    assert_eq!(trash.bibliography, full.bibliography);
    assert_eq!(trash.external_ids, full.external_ids);
    db.papers().set_deleted(full.id, false).unwrap();
    let restored = PaperService::new(db.papers()).get(full.id).unwrap();
    assert_eq!(restored.bibliography, full.bibliography);
    assert_eq!(ids(&db, filter.clone()), vec![full.id]);
    assert_eq!(
        ids(
            &db,
            LibraryFilter {
                visibility: PaperVisibility::All,
                has_pdf: Some(true),
                ..Default::default()
            }
        ),
        vec![full.id, deleted.id]
    );
    drop(db);
    let mut reopened = LibraryDb::open(&dir).unwrap();
    assert_eq!(
        reopened
            .library_query()
            .query(&LibraryQuery {
                filter,
                ..Default::default()
            })
            .unwrap()
            .papers,
        vec![restored]
    );
    assert_eq!(reopened.tags().for_paper(full.id).unwrap().len(), 2);
    assert_eq!(reopened.collections().for_paper(full.id).unwrap().len(), 2);
}

fn expected_order(papers: &[Paper], field: SortField, direction: SortDirection) -> Vec<PaperId> {
    let mut ordered = papers.to_vec();
    ordered.sort_by(|a, b| {
        // Null placement is independent of direction.
        if field == SortField::Year {
            let null = a
                .bibliography
                .year
                .is_none()
                .cmp(&b.bibliography.year.is_none());
            if !null.is_eq() {
                return null;
            }
        }
        let primary = match field {
            SortField::Id => a.id.0.cmp(&b.id.0),
            SortField::Title => a.title.cmp(&b.title),
            SortField::Year => a.bibliography.year.cmp(&b.bibliography.year),
            SortField::CreatedAt => a.created_at.cmp(&b.created_at),
            SortField::UpdatedAt => a.updated_at.cmp(&b.updated_at),
        };
        let comparison = primary.then(a.id.0.cmp(&b.id.0));
        if direction == SortDirection::Desc {
            comparison.reverse()
        } else {
            comparison
        }
    });
    ordered.iter().map(|p| p.id).collect()
}

#[test]
fn stable_keyset_pagination_handles_all_sorts_directions_nulls_and_ties() {
    let (_temp, dir, mut db) = library();
    let tag = db.tags().create(TagInput::named("tied")).unwrap();
    let collection = db
        .collections()
        .create(CollectionInput::named("ties"))
        .unwrap();
    for (i, year) in [
        Some(2026),
        None,
        Some(0),
        Some(2026),
        Some(-1),
        None,
        Some(2026),
        Some(2020),
        Some(0),
        None,
        Some(2026),
    ]
    .into_iter()
    .enumerate()
    {
        let mut input = NewPaper::titled(["same", "日本語", "Énergie", "same"][i % 4]);
        input.bibliography.year = year;
        let p = db.papers().create(input).unwrap();
        db.tags().set_membership(p.id, tag.id, true).unwrap();
        db.collections()
            .set_membership(p.id, collection.id, true)
            .unwrap();
    }
    let raw = Connection::open(dir.database_path()).unwrap();
    raw.execute(
        "UPDATE papers SET created_at=?1,updated_at=?2",
        params!["2026-11-06T00:00:00.000Z", "2026-11-06T01:00:00.000Z"],
    )
    .unwrap();
    let filter = LibraryFilter {
        tags: vec![tag.id, tag.id],
        collection: Some(collection.id),
        ..Default::default()
    };
    let papers = db
        .library_query()
        .query(&LibraryQuery {
            filter: filter.clone(),
            ..Default::default()
        })
        .unwrap()
        .papers;
    for field in [
        SortField::Id,
        SortField::Title,
        SortField::Year,
        SortField::CreatedAt,
        SortField::UpdatedAt,
    ] {
        for direction in [SortDirection::Asc, SortDirection::Desc] {
            for limit in [1, 2, 3, 5, 11, 12] {
                let mut query = LibraryQuery {
                    filter: filter.clone(),
                    sort: LibrarySort { field, direction },
                    limit,
                    after: None,
                };
                let mut seen = Vec::new();
                loop {
                    let page = db.library_query().query(&query).unwrap();
                    assert!(!page.papers.is_empty());
                    assert!(page.papers.len() <= limit as usize);
                    seen.extend(page.papers.iter().map(|p| p.id));
                    assert!(seen.len() <= papers.len(), "cursor failed to advance");
                    let Some(cursor) = page.next else {
                        break;
                    };
                    query.after = Some(
                        cm_core::library_query::LibraryCursor::from_encoded(cursor.as_str().into())
                            .unwrap(),
                    );
                }
                assert_eq!(
                    seen,
                    expected_order(&papers, field, direction),
                    "{field:?} {direction:?} limit={limit}"
                );
            }
        }
    }
    let initial = LibraryQuery {
        sort: LibrarySort {
            field: SortField::Title,
            direction: SortDirection::Asc,
        },
        limit: 2,
        ..Default::default()
    };
    let page = db.library_query().query(&initial).unwrap();
    let anchor = page.papers.last().unwrap().id;
    let after = page.next.unwrap();
    // An opaque cursor carries the key; deleted anchors need no lookup.
    db.papers().set_deleted(anchor, true).unwrap();
    let continued = db
        .library_query()
        .query(&LibraryQuery {
            after: Some(after.clone()),
            limit: 500,
            ..initial.clone()
        })
        .unwrap();
    let expected = expected_order(&papers, SortField::Title, SortDirection::Asc);
    assert_eq!(
        continued.papers.iter().map(|p| p.id).collect::<Vec<_>>(),
        expected[2..]
    );
    invalid(db.library_query().query(&LibraryQuery {
        filter: LibraryFilter {
            has_pdf: Some(true),
            ..Default::default()
        },
        after: Some(after),
        ..initial
    }));
    invalid(db.library_query().query(&LibraryQuery {
        limit: 0,
        ..Default::default()
    }));
    drop(db);
    let reopened = LibraryDb::open(&dir).unwrap();
    assert_eq!(
        reopened
            .library_query()
            .query(&LibraryQuery::default())
            .unwrap()
            .papers
            .len(),
        papers.len() - 1
    );
}

#[test]
fn empty_memory_library_and_invalid_destinations_use_the_same_contracts() {
    let mut db = LibraryDb::in_memory().unwrap();
    let page = db.library_query().query(&LibraryQuery::default()).unwrap();
    assert!(page.papers.is_empty());
    assert!(page.next.is_none());
    assert!(db.collections().list().unwrap().is_empty());
    assert!(db.tags().list().unwrap().is_empty());
    let paper = db.papers().create(NewPaper::titled("memory")).unwrap();
    let collection = db
        .collections()
        .create(CollectionInput::named("memory"))
        .unwrap();
    let tag = db.tags().create(TagInput::named("memory")).unwrap();
    let destination = |tags| ImportDestination {
        collection: Some(collection.id),
        tags,
    };
    invalid(
        db.collections()
            .apply_destination(paper.id, &destination(vec![tag.id; 101])),
    );
    invalid(
        db.collections()
            .validate_destination(&destination(vec![TagId(Uuid::nil())])),
    );
    invalid(
        db.collections()
            .set_membership(PaperId(Uuid::nil()), collection.id, true),
    );
    invalid(db.tags().set_membership(paper.id, TagId(Uuid::nil()), true));
    assert_eq!(
        db.collections()
            .apply_destination(PaperId(Uuid::now_v7()), &destination(vec![tag.id])),
        Err(ServiceError::NotFound)
    );
    assert!(db.collections().for_paper(paper.id).unwrap().is_empty());
    assert!(db.tags().for_paper(paper.id).unwrap().is_empty());
    db.collections()
        .apply_destination(paper.id, &destination(vec![tag.id; 100]))
        .unwrap();
    assert_eq!(
        db.collections().for_paper(paper.id).unwrap(),
        vec![collection]
    );
    assert_eq!(db.tags().for_paper(paper.id).unwrap(), vec![tag]);
}
