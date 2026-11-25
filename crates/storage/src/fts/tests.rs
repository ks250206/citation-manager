//! #18 storage evidence: trigger-maintained projection == rebuild from sources, measured
//! tokenizer behaviour, extraction persistence/resume, corruption recovery and the search
//! service over real bundled SQLite.
use crate::{LibraryDb, LibraryDirectory};
use cm_core::{
    collections::{CollectionInput, CollectionService},
    papers::{NewPaper, PaperLibrary, PaperService, PaperVisibility},
    search::{
        extraction::{ExtractScope, ExtractedPage, ExtractionState, FulltextStore, StoreBatch},
        search,
        service::{
            Candidate, ExtractionGaps, IndexedDocument, ResolvedFilter, ScanWindow, SearchIndex,
            Tokenizer,
        },
        types::*,
    },
    tags::{TagInput, TagService},
};
use cm_domain::{AttachmentId, Contributor, PaperId};
use rusqlite::params;
use uuid::Uuid;

fn author(name: &str) -> Contributor {
    Contributor {
        given_name: None,
        family_name: None,
        literal_name: Some(name.into()),
        orcid: None,
        role: "author".into(),
    }
}
fn paper(db: &mut LibraryDb, title: &str, year: i32, abstract_text: &str) -> PaperId {
    let mut input = NewPaper::titled(title);
    input.bibliography.year = Some(year);
    input.bibliography.abstract_text = Some(abstract_text.into());
    input.bibliography.authors = vec![author("Aiko Tanaka")];
    db.papers().create(input).unwrap().id
}
fn attachment(db: &LibraryDb, paper: PaperId, seed: u8) -> AttachmentId {
    let id = Uuid::now_v7();
    let hash = [seed; 32];
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    db.connection
        .execute(
            "INSERT INTO attachments(id,paper_id,kind,file_hash,relative_path,original_name,mime_type,byte_size,added_at)
             VALUES (?1,?2,'pdf',?3,?4,'x.pdf','application/pdf',1,'2026-11-24T00:00:00Z')",
            params![
                id.to_string(),
                paper.0.to_string(),
                hash.as_slice(),
                format!("attachments/{}/{}/{hex}.pdf", &hex[..2], &hex[2..4])
            ],
        )
        .unwrap();
    AttachmentId(id)
}
fn page(db: &LibraryDb, attachment: AttachmentId, index: u32, label: &str, text: &str) {
    db.connection
        .execute(
            "INSERT INTO fulltext_pages(attachment_id,page_index,page_label,text,extraction_status,updated_at)
             VALUES (?1,?2,?3,?4,'complete','now')",
            params![attachment.0.to_string(), index, label, text],
        )
        .unwrap();
}
fn annotate(db: &LibraryDb, attachment: AttachmentId, quote: &str, comment: &str) -> String {
    let id = Uuid::now_v7().to_string();
    db.connection
        .execute(
            "INSERT INTO annotations(id,attachment_id,page_index,page_label,type,source,anchor_state,quote_text,comment,quads_json,created_at,updated_at)
             VALUES (?1,?2,0,'iv','highlight','human','resolved',?3,?4,'[]','now','now')",
            params![id, attachment.0.to_string(), quote, comment],
        )
        .unwrap();
    id
}
type Row = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);
fn projection(db: &LibraryDb) -> Vec<Row> {
    db.connection
        .prepare("SELECT source,source_key,title,authors,tags,coalesce(notes,body) FROM search_documents ORDER BY source,source_key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
#[track_caller]
fn assert_synced(db: &LibraryDb) {
    let health = db.search_index().verify().unwrap();
    assert!(health.ok(), "{health:?}");
}
fn results(db: &LibraryDb, request: &SearchRequest) -> SearchPage {
    match search(&db.search_index(), request).unwrap() {
        SearchReply::Results(page) => *page,
        SearchReply::Invalid(e) => panic!("{e:?}"),
    }
}
fn query(db: &LibraryDb, q: &str) -> SearchPage {
    results(db, &SearchRequest::new(q))
}
fn papers_of(page: &SearchPage) -> Vec<PaperId> {
    let mut v: Vec<PaperId> = page.hits.iter().map(|h| h.paper_id).collect();
    v.dedup();
    v
}
fn fts_ids(db: &LibraryDb, tokenizer: Tokenizer, text: &str) -> Vec<i64> {
    let mut ids: Vec<i64> = db
        .search_index()
        .fts(
            tokenizer,
            text,
            &Default::default(),
            &[
                DocumentSource::Paper,
                DocumentSource::Annotation,
                DocumentSource::Page,
            ],
            &Weights::default(),
            100,
        )
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    ids.sort();
    ids
}
fn doc_id(db: &LibraryDb, source: &str, key: &str) -> i64 {
    db.connection
        .query_row(
            "SELECT id FROM search_documents WHERE source=?1 AND source_key=?2",
            [source, key],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn triggers_keep_projection_equal_to_a_rebuild_through_every_source_change() {
    let mut db = LibraryDb::in_memory().unwrap();
    assert_synced(&db);
    let a = paper(
        &mut db,
        "Solid-state battery interfaces",
        2024,
        "Lithium alloy anodes",
    );
    let b = paper(&mut db, "固体電解質の界面", 2023, "全固体電池");
    assert_synced(&db);
    // Paper update with contributor replacement.
    let mut p = PaperService::new(db.papers()).get(a).unwrap();
    p.title = "Solid-state battery interphases".into();
    p.bibliography.authors = vec![author("Grace Hopper"), author("佐藤 花子")];
    p.bibliography.doi = Some("10.1234/example".into());
    PaperService::new(db.papers()).update(&p).unwrap();
    assert_synced(&db);
    // Tags: add, rename, remove, delete.
    let tag = TagService::new(db.tags())
        .create(TagInput::named("Anode"))
        .unwrap();
    TagService::new(db.tags()).add_paper(a, tag.id).unwrap();
    TagService::new(db.tags()).add_paper(b, tag.id).unwrap();
    assert_synced(&db);
    TagService::new(db.tags())
        .update(tag.id, TagInput::named("Anodes"))
        .unwrap();
    assert_synced(&db);
    assert_eq!(papers_of(&query(&db, "anodes")).len(), 2);
    TagService::new(db.tags()).remove_paper(b, tag.id).unwrap();
    assert_synced(&db);
    // Annotations and extracted pages.
    let att = attachment(&db, a, 1);
    let note = annotate(&db, att, "interfacial stability", "check the alloy");
    page(
        &db,
        att,
        0,
        "iv",
        "Unicode 日本語 αβ café interfacial stability of the Li alloy",
    );
    page(&db, att, 1, "S12", "");
    assert_synced(&db);
    db.connection
        .execute(
            "UPDATE annotations SET comment='revised comment' WHERE id=?1",
            [&note],
        )
        .unwrap();
    db.connection
        .execute(
            "UPDATE fulltext_pages SET text='replaced page text' WHERE page_index=0",
            [],
        )
        .unwrap();
    assert_synced(&db);
    // Soft delete/restore are visibility changes only; projection is unchanged.
    let before = projection(&db);
    PaperService::new(db.papers()).soft_delete(a).unwrap();
    assert_eq!(projection(&db), before);
    assert!(query(&db, "interphases").hits.is_empty());
    let mut trash = SearchRequest::new("interphases");
    trash.visibility = PaperVisibility::Deleted;
    let hits = results(&db, &trash).hits;
    assert!(hits.iter().all(|h| h.deleted) && !hits.is_empty());
    PaperService::new(db.papers()).restore(a).unwrap();
    assert_eq!(papers_of(&query(&db, "interphases")), vec![a]);
    TagService::new(db.tags()).delete(tag.id).unwrap();
    assert_synced(&db);
    // Hard delete cascades remove every document of the paper (annotation/page too).
    db.connection
        .execute("DELETE FROM papers WHERE id=?1", [a.0.to_string()])
        .unwrap();
    assert_synced(&db);
    let left: i64 = db
        .connection
        .query_row(
            "SELECT count(*) FROM search_documents WHERE paper_id=?1",
            [a.0.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
    // Rebuild produces exactly the trigger-maintained projection.
    let maintained = projection(&db);
    db.search_index().rebuild().unwrap();
    assert_eq!(projection(&db), maintained);
    assert_synced(&db);
}

#[test]
fn measured_tokenizer_behaviour_and_short_term_fallback() {
    let mut db = LibraryDb::in_memory().unwrap();
    let a = paper(&mut db, "固体電解質の界面", 2023, "全固体電池の研究");
    let b = paper(&mut db, "電池", 2022, "standalone token");
    let c = paper(&mut db, "\u{30AC}\u{30E9}\u{30B9}", 2021, "composed kana");
    let d = paper(
        &mut db,
        "\u{30AB}\u{3099}\u{30E9}\u{30B9}",
        2021,
        "decomposed kana",
    );
    let pa = doc_id(&db, "paper", &a.0.to_string());
    let pb = doc_id(&db, "paper", &b.0.to_string());
    let pc = doc_id(&db, "paper", &c.0.to_string());
    let pd = doc_id(&db, "paper", &d.0.to_string());
    // unicode61 indexes CJK runs as whole tokens: 電池 only matches the standalone token.
    assert_eq!(fts_ids(&db, Tokenizer::Words, "電池"), vec![pb]);
    // trigram finds 3+ scalar substrings but nothing for shorter terms.
    assert_eq!(fts_ids(&db, Tokenizer::Trigram, "固体電"), vec![pa]);
    assert!(fts_ids(&db, Tokenizer::Trigram, "電池").is_empty());
    // No NFC folding in either index (decomposed dakuten stays distinct for trigram).
    assert_eq!(
        fts_ids(&db, Tokenizer::Trigram, "\u{30AC}\u{30E9}\u{30B9}"),
        vec![pc]
    );
    assert_eq!(
        fts_ids(&db, Tokenizer::Trigram, "\u{30AB}\u{3099}\u{30E9}\u{30B9}"),
        vec![pd]
    );
    // Service: 2-scalar term uses the bounded literal scan and finds both papers.
    let page = query(&db, "電池");
    assert_eq!(page.coverage.short_terms, vec!["電池".to_string()]);
    assert_eq!(page.coverage.scan, ScanStatus::Complete);
    let mut found = papers_of(&page);
    found.sort_by_key(|p| p.0);
    let mut expected = vec![a, b];
    expected.sort_by_key(|p| p.0);
    assert_eq!(found, expected);
    // Bounded scope without a narrowing filter: PDF pages are reported as unsearched.
    assert!(
        page.coverage
            .unsearched
            .contains(&UnsearchedScope::PdfPagesForShortTerms)
    );
    assert!(!page.coverage.complete);
    // 1 scalar, plus 3+ scalar AND in the same paper.
    assert_eq!(papers_of(&query(&db, "電 固体電解")), vec![a]);
}

#[test]
fn ranking_filters_snippets_pages_and_errors() {
    let mut db = LibraryDb::in_memory().unwrap();
    let title_hit = paper(&mut db, "Lithium metal anodes", 2020, "A review");
    let body_hit = paper(&mut db, "Interfaces in batteries", 2025, "No match here");
    let att = attachment(&db, body_hit, 7);
    page(
        &db,
        att,
        4,
        "5",
        "We study lithium plating at the interface.",
    );
    db.connection
        .execute(
            "INSERT INTO fulltext_extractions(attachment_id,file_hash,status,page_count,next_page,pages_with_text,updated_at) VALUES (?1,?2,'complete',5,5,1,'now')",
            params![att.0.to_string(), [7u8; 32].as_slice()],
        )
        .unwrap();
    let page = query(&db, "lithium");
    assert_eq!(page.hits.len(), 2);
    assert_eq!(
        page.hits[0].paper_id, title_hit,
        "title weight outranks body"
    );
    let body = &page.hits[1];
    assert_eq!(body.source, DocumentSource::Page);
    assert_eq!(body.attachment_id, Some(att));
    assert_eq!(
        (body.page_index, body.page_label.as_deref()),
        (Some(4), Some("5"))
    );
    assert_eq!(body.matched_fields, vec![SearchField::Body]);
    let h = body.highlights[0];
    assert_eq!(body.snippet[h.start..h.end].to_ascii_lowercase(), "lithium");
    assert!(page.coverage.complete, "{:?}", page.coverage);
    // Reweighting body above title reverses the order (no duplicate hits per document).
    let mut weighted = SearchRequest::new("lithium");
    weighted.weights = vec![
        FieldWeight {
            field: SearchField::Title,
            weight: 0.1,
        },
        FieldWeight {
            field: SearchField::Body,
            weight: 50.0,
        },
    ];
    let reordered = results(&db, &weighted);
    assert_eq!(reordered.hits[0].paper_id, body_hit);
    assert_eq!(reordered.hits.len(), 2);
    // Filters: year, tag, collection, doi, has:pdf, sources.
    assert_eq!(
        papers_of(&query(&db, "lithium year:2024..2026")),
        vec![body_hit]
    );
    assert_eq!(papers_of(&query(&db, "lithium has:pdf")), vec![body_hit]);
    let tag = TagService::new(db.tags())
        .create(TagInput::named("Reading"))
        .unwrap();
    TagService::new(db.tags())
        .add_paper(title_hit, tag.id)
        .unwrap();
    assert_eq!(papers_of(&query(&db, "tag:reading")), vec![title_hit]);
    let col = CollectionService::new(db.collections())
        .create(CollectionInput::named("Shelf"))
        .unwrap();
    CollectionService::new(db.collections())
        .add_paper(body_hit, col.id)
        .unwrap();
    assert_eq!(
        papers_of(&query(&db, "lithium collection:Shelf")),
        vec![body_hit]
    );
    let unknown = query(&db, "lithium tag:absent");
    assert!(unknown.hits.is_empty());
    assert_eq!(
        unknown.coverage.unresolved,
        vec![Unresolved::Tag("absent".into())]
    );
    CollectionService::new(db.collections())
        .create(CollectionInput::named("Shelf"))
        .unwrap();
    match search(&db.search_index(), &SearchRequest::new("collection:Shelf")).unwrap() {
        SearchReply::Invalid(e) => assert_eq!(e.diagnostics[0].code, "AmbiguousCollection"),
        other => panic!("{other:?}"),
    }
    // A collection-ID scope alone (blank DSL) lists that collection's papers, even when
    // its name is ambiguous; a blank DSL without any scope still fails.
    let mut by_id = SearchRequest::new("  ");
    by_id.collection = Some(col.id);
    assert_eq!(papers_of(&results(&db, &by_id)), vec![body_hit]);
    let mut by_papers = SearchRequest::new("");
    by_papers.papers = vec![title_hit];
    assert_eq!(papers_of(&results(&db, &by_papers)), vec![title_hit]);
    match search(&db.search_index(), &SearchRequest::new(" ")).unwrap() {
        SearchReply::Invalid(e) => assert_eq!(e.diagnostics[0].code, "EmptyInput"),
        other => panic!("{other:?}"),
    }
    let mut bib_only = SearchRequest::new("lithium");
    bib_only.sources.pages = false;
    assert_eq!(papers_of(&results(&db, &bib_only)), vec![title_hit]);
    // Syntax errors return structured diagnostics with correction examples.
    match search(&db.search_index(), &SearchRequest::new("lithium foo:bar")).unwrap() {
        SearchReply::Invalid(e) => {
            assert_eq!(e.diagnostics[0].code, "UnknownField");
            assert_eq!((e.diagnostics[0].start, e.diagnostics[0].end), (8, 11));
            assert!(
                e.diagnostics[0]
                    .corrections
                    .iter()
                    .any(|c| c.replacement == "tag:anode")
            );
        }
        other => panic!("{other:?}"),
    }
    // Phrase input is a single quoted FTS phrase; FTS operators are data.
    assert!(query(&db, "\"lithium OR plating\"").hits.is_empty());
    assert_eq!(
        papers_of(&query(&db, "\"lithium plating\"")),
        vec![body_hit]
    );
    // Pagination over the ranked list.
    let mut first = SearchRequest::new("lithium");
    first.limit = 1;
    let one = results(&db, &first);
    assert_eq!((one.hits.len(), one.total_hits), (1, 2));
    first.cursor = one.next_page;
    let two = results(&db, &first);
    assert_eq!(two.hits.len(), 1);
    assert_ne!(two.hits[0].paper_id, one.hits[0].paper_id);
    assert!(two.next_page.is_none());
}

#[test]
fn short_term_scan_stops_at_the_deadline_between_chunks() {
    let mut db = LibraryDb::in_memory().unwrap();
    for i in 0..300 {
        paper(&mut db, &format!("電池 {i}"), 2020, "x");
    }
    let index = db.search_index();
    let all = [DocumentSource::Paper];
    let expired = std::time::Instant::now();
    let window = index
        .scan(
            &["電池".into()],
            &Default::default(),
            &all,
            0,
            1000,
            expired,
        )
        .unwrap();
    assert!(window.timed_out && !window.exhausted);
    assert_eq!(
        window.examined, 256,
        "one chunk, then the deadline is checked"
    );
    let rest = index
        .scan(
            &["電池".into()],
            &Default::default(),
            &all,
            window.last_id.unwrap(),
            1000,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .unwrap();
    assert!(rest.exhausted && !rest.timed_out);
    assert_eq!(window.rows.len() + rest.rows.len(), 300);
    // The service reports a timed-out window as partial with a continuation.
    let mut request = SearchRequest::new("電池");
    request.timeout_ms = 1;
    request.candidate_limit = 20_000;
    let page = results(&db, &request);
    if page.coverage.timed_out {
        assert_eq!(page.coverage.scan, ScanStatus::Partial);
        assert!(page.coverage.continuation.is_some() && !page.coverage.complete);
    }
}

/// Two short clauses matching different documents of one paper that fall into different
/// scan windows still satisfy the paper-level AND: following every continuation returns
/// each matching document exactly once, in its own window.
#[test]
fn short_clauses_matched_in_different_windows_keep_their_paper() {
    let mut db = LibraryDb::in_memory().unwrap();
    let both = paper(&mut db, "zabz", 2020, "nothing here");
    let att = attachment(&db, both, 5);
    annotate(&db, att, "quoted zcdz", "a comment");
    // Only one of the two substrings: never a hit.
    let half = paper(&mut db, "qabq", 2020, "nothing here");
    let mut request = SearchRequest::new("ab cd");
    request.candidate_limit = 1;
    let mut seen = Vec::new();
    let mut windows = 0;
    loop {
        let page = results(&db, &request);
        windows += 1;
        assert!(windows < 20, "continuations must end");
        assert!(!page.coverage.timed_out);
        seen.extend(page.hits.iter().map(|h| (h.paper_id, h.source)));
        match page.coverage.continuation {
            Some(next) => request.cursor = Some(next),
            None => break,
        }
    }
    assert!(windows >= 2);
    assert_eq!(
        seen,
        vec![
            (both, DocumentSource::Paper),
            (both, DocumentSource::Annotation)
        ]
    );
    assert!(!seen.iter().any(|(p, _)| *p == half));
    // One window over everything gives the same documents.
    let page = results(&db, &SearchRequest::new("ab cd"));
    assert_eq!(page.hits.len(), 2);
    assert_eq!(page.coverage.scan, ScanStatus::Complete);
}

#[test]
fn short_term_scan_is_bounded_with_continuation_and_explicit_scope() {
    let mut db = LibraryDb::in_memory().unwrap();
    let mut papers = Vec::new();
    for i in 0..10 {
        let title = if i % 2 == 0 {
            format!("電池 {i}")
        } else {
            format!("other {i}")
        };
        papers.push(paper(&mut db, &title, 2020, "x"));
    }
    let owner = paper(&mut db, "PDF owner", 2020, "y");
    let att = attachment(&db, owner, 3);
    page(&db, att, 0, "1", "本文に電池が出る");
    let mut request = SearchRequest::new("電池");
    request.candidate_limit = 4;
    let mut seen = Vec::new();
    let mut windows = 0;
    loop {
        let page = results(&db, &request);
        windows += 1;
        seen.extend(page.hits.iter().map(|h| h.paper_id));
        match page.coverage.continuation {
            Some(next) => {
                assert_eq!(page.coverage.scan, ScanStatus::Partial);
                assert!(
                    page.coverage
                        .unsearched
                        .contains(&UnsearchedScope::DocumentsAfterWindow)
                );
                request.cursor = Some(next);
            }
            None => break,
        }
    }
    assert!(windows >= 3);
    assert_eq!(
        seen.len(),
        5,
        "every title hit once, page text not searched"
    );
    assert!(!seen.contains(&owner));
    // Explicit narrowing (paper scope) makes the page candidate set bounded and searched.
    let mut scoped = SearchRequest::new("電池");
    scoped.papers = vec![owner];
    let page = results(&db, &scoped);
    assert_eq!(page.hits.len(), 1);
    assert_eq!(page.hits[0].source, DocumentSource::Page);
    assert!(
        !page
            .coverage
            .unsearched
            .contains(&UnsearchedScope::PdfPagesForShortTerms)
    );
    // AllPages scans everything (still windowed).
    let mut all = SearchRequest::new("電池");
    all.short_term_scope = ShortTermScope::AllPages;
    assert_eq!(results(&db, &all).hits.len(), 6);
}

#[test]
fn corrupted_fts_index_is_detected_and_rebuilt_without_touching_sources() {
    let mut db = LibraryDb::in_memory().unwrap();
    let a = paper(&mut db, "Solid electrolyte interphase", 2024, "SEI growth");
    let att = attachment(&db, a, 9);
    page(&db, att, 0, "1", "電解質 page text about dendrites");
    annotate(&db, att, "dendrites", "important");
    let before = query(&db, "dendrites");
    let sources = |db: &LibraryDb| -> (i64, i64, i64, String) {
        db.connection
            .query_row(
                "SELECT (SELECT count(*) FROM papers),(SELECT count(*) FROM fulltext_pages),(SELECT count(*) FROM annotations),(SELECT group_concat(title) FROM papers)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
    };
    let original = sources(&db);
    db.connection
        .execute_batch("DELETE FROM search_fts_words_data WHERE id > 10; UPDATE search_fts_trigram_data SET block = x'00' WHERE id > 10;")
        .unwrap();
    let health = db.search_index().verify().unwrap();
    assert!(!health.fts_ok);
    let generation = db.search_index().generation().unwrap();
    db.search_index().rebuild().unwrap();
    assert!(db.search_index().verify().unwrap().ok());
    assert_eq!(db.search_index().generation().unwrap(), generation + 1);
    assert_eq!(sources(&db), original);
    let after = query(&db, "dendrites");
    assert_eq!(after.hits.len(), before.hits.len());
    assert_eq!(
        after
            .hits
            .iter()
            .map(|h| (h.paper_id, h.source))
            .collect::<Vec<_>>(),
        before
            .hits
            .iter()
            .map(|h| (h.paper_id, h.source))
            .collect::<Vec<_>>()
    );
    // A drifted projection (row removed behind the triggers' back) is also detected.
    db.connection
        .execute_batch("UPDATE search_index_state SET suspended=1; DELETE FROM search_documents WHERE source='annotation'; UPDATE search_index_state SET suspended=0;")
        .unwrap();
    assert!(!db.search_index().verify().unwrap().projection_ok);
    db.search_index().rebuild().unwrap();
    assert!(db.search_index().verify().unwrap().ok());
}

#[test]
fn extraction_store_commits_batches_resumes_and_records_failures() {
    let temp = tempfile::tempdir().unwrap();
    let directory =
        LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("library")).unwrap();
    let mut db = LibraryDb::open(&directory).unwrap();
    let owner = paper(&mut db, "Owner", 2020, "z");
    let good = attachment(&db, owner, 1);
    let broken = attachment(&db, owner, 2);
    let mut store = db.fulltext(&directory).unwrap();
    let targets = store.targets(ExtractScope::Pending, false, 10).unwrap();
    assert_eq!(targets.len(), 2);
    assert!(
        targets
            .iter()
            .all(|t| t.resume_from == 0 && !t.reset && t.path.is_absolute())
    );
    let target = targets
        .iter()
        .find(|t| t.attachment_id == good)
        .unwrap()
        .clone();
    let pages = |range: std::ops::Range<u32>| -> Vec<ExtractedPage> {
        range
            .map(|i| ExtractedPage {
                page_index: i,
                page_label: Some(format!("p{i}")),
                text: if i == 1 {
                    String::new()
                } else {
                    format!("page {i} lithium")
                },
                truncated: i == 2,
            })
            .collect()
    };
    let first = pages(0..2);
    let state = store
        .store(StoreBatch {
            target: &target,
            page_count: 4,
            pages: &first,
            next_page: 2,
            finish: None,
        })
        .unwrap();
    assert_eq!(state, ExtractionState::Extracting);
    // "Crash": the next run resumes at the first uncommitted page without a reset.
    let resumed = store
        .targets(ExtractScope::Attachment(good), false, 10)
        .unwrap();
    assert_eq!((resumed[0].resume_from, resumed[0].reset), (2, false));
    let second = pages(2..4);
    let state = store
        .store(StoreBatch {
            target: &target,
            page_count: 4,
            pages: &second,
            next_page: 4,
            finish: Some(false),
        })
        .unwrap();
    assert_eq!(state, ExtractionState::Partial, "one page was truncated");
    let broken_target = targets
        .iter()
        .find(|t| t.attachment_id == broken)
        .unwrap()
        .clone();
    store
        .fail(
            &broken_target,
            ExtractionState::Encrypted,
            "password-protected",
        )
        .unwrap();
    assert!(
        store
            .targets(ExtractScope::Pending, false, 10)
            .unwrap()
            .is_empty()
    );
    let forced = store.targets(ExtractScope::Paper(owner), true, 10).unwrap();
    assert!(forced.iter().all(|t| t.reset && t.resume_from == 0));
    drop(store);
    let reader = db.fulltext_reader();
    let statuses = reader.for_paper(owner).unwrap();
    let good_status = statuses.iter().find(|s| s.attachment_id == good).unwrap();
    assert_eq!(
        (
            good_status.state,
            good_status.page_count,
            good_status.pages_with_text,
            good_status.truncated_pages
        ),
        (ExtractionState::Partial, 4, 3, 1)
    );
    let broken_status = statuses.iter().find(|s| s.attachment_id == broken).unwrap();
    assert_eq!(broken_status.state, ExtractionState::Encrypted);
    assert_eq!(reader.unfinished().unwrap().count, 0);
    // Bounded page reads.
    let read = reader
        .pages(&PageTextRequest {
            attachment: good,
            from_page: 0,
            max_pages: 2,
            max_chars: 100,
        })
        .unwrap();
    assert_eq!(read.pages.len(), 2);
    assert_eq!(
        (read.pages[0].text.as_str(), read.pages[0].truncated),
        ("page 0 lithium", false)
    );
    assert_eq!(read.pages[1].page_label.as_deref(), Some("p1"));
    assert_eq!(read.next_page, Some(2));
    let cut = reader
        .pages(&PageTextRequest {
            attachment: good,
            from_page: 2,
            max_pages: 50,
            max_chars: 10,
        })
        .unwrap();
    assert_eq!(cut.pages.len(), 1);
    assert_eq!(
        (cut.pages[0].text.as_str(), cut.pages[0].truncated),
        ("page 2 lit", true)
    );
    assert_eq!(cut.next_page, Some(3));
    assert!(
        reader
            .pages(&PageTextRequest {
                attachment: good,
                from_page: 0,
                max_pages: 51,
                max_chars: 1
            })
            .is_err()
    );
    // Extracted pages are searchable with labels; coverage reports the encrypted PDF.
    let page = query(&db, "lithium");
    assert_eq!(page.hits.len(), 3, "pages 0, 2 and 3 have text");
    assert!(page.hits.iter().all(|h| h.attachment_id == Some(good)));
    assert!(
        page.coverage
            .unsearched
            .contains(&UnsearchedScope::NoExtractableText { attachments: 1 })
    );
    assert_synced(&db);
}

fn file_library() -> (tempfile::TempDir, LibraryDirectory, LibraryDb) {
    let temp = tempfile::tempdir().unwrap();
    let directory =
        LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("library")).unwrap();
    let db = LibraryDb::open(&directory).unwrap();
    (temp, directory, db)
}

/// P2-1 regression: lock contention is not index damage. While another connection holds the
/// write lock, searches on a reader still work (WAL) and `verify` (whose FTS integrity-check
/// needs the write lock) reports BUSY as an error without a verdict instead of
/// `fts_ok == false`, so the daemon answers "busy" and never schedules a rebuild.
#[test]
fn busy_writer_is_reported_as_contention_not_damage() {
    use crate::{SqlFault, sql_fault};
    let (_temp, directory, mut db) = file_library();
    paper(&mut db, "Solid electrolyte interphase", 2024, "SEI growth");
    let reader = super::SearchReader::open(&directory).unwrap();
    reader
        .connection
        .busy_timeout(std::time::Duration::from_millis(50))
        .unwrap();
    let generation = db.search_index().generation().unwrap();
    db.connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    {
        let snapshot = reader.snapshot().unwrap();
        match search(&*snapshot, &SearchRequest::new("electrolyte")).unwrap() {
            SearchReply::Results(page) => assert_eq!(page.hits.len(), 1),
            other => panic!("{other:?}"),
        }
    }
    let index = reader.search_index();
    assert!(
        index.verify().is_err(),
        "no verdict under a held write lock"
    );
    assert_eq!(index.fault(), Some(SqlFault::Busy));
    db.connection.execute_batch("ROLLBACK").unwrap();
    let health = index.verify().unwrap();
    assert!(health.ok(), "{health:?}");
    assert_eq!(db.search_index().generation().unwrap(), generation);
    // The classifier separates the codes the daemon routes differently.
    let failure =
        |code| rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), Some("x".into()));
    assert_eq!(
        sql_fault(&failure(rusqlite::ffi::SQLITE_BUSY)),
        SqlFault::Busy
    );
    assert_eq!(
        sql_fault(&failure(rusqlite::ffi::SQLITE_LOCKED)),
        SqlFault::Busy
    );
    assert_eq!(
        sql_fault(&failure(rusqlite::ffi::SQLITE_INTERRUPT)),
        SqlFault::Interrupted
    );
    assert_eq!(
        sql_fault(&failure(rusqlite::ffi::SQLITE_CORRUPT_VTAB)),
        SqlFault::Corrupt
    );
    assert_eq!(
        sql_fault(&failure(rusqlite::ffi::SQLITE_ERROR)),
        SqlFault::Other
    );
}

/// Delegates to a reader's port but replaces every deadline with one already passed, so
/// the deadline interrupts deterministically inside SQLite.
struct Expired<'a>(super::SqliteSearch<'a>);
impl SearchIndex for Expired<'_> {
    fn resolve_tag(
        &self,
        key: &cm_domain::TagKey,
    ) -> Result<Option<cm_domain::TagId>, cm_core::ServiceError> {
        self.0.resolve_tag(key)
    }
    fn resolve_collection(
        &self,
        name: &str,
    ) -> Result<Vec<cm_domain::CollectionId>, cm_core::ServiceError> {
        self.0.resolve_collection(name)
    }
    fn fts(
        &self,
        tokenizer: Tokenizer,
        text: &str,
        filter: &ResolvedFilter,
        sources: &[DocumentSource],
        weights: &Weights,
        max: u32,
    ) -> Result<Vec<Candidate>, cm_core::ServiceError> {
        self.0.fts(tokenizer, text, filter, sources, weights, max)
    }
    fn filtered(
        &self,
        filter: &ResolvedFilter,
        max: u32,
    ) -> Result<Vec<Candidate>, cm_core::ServiceError> {
        self.0.filtered(filter, max)
    }
    fn scan(
        &self,
        needles: &[String],
        filter: &ResolvedFilter,
        sources: &[DocumentSource],
        after: i64,
        max_rows: u32,
        deadline: std::time::Instant,
    ) -> Result<ScanWindow, cm_core::ServiceError> {
        self.0
            .scan(needles, filter, sources, after, max_rows, deadline)
    }
    fn documents(&self, ids: &[i64]) -> Result<Vec<IndexedDocument>, cm_core::ServiceError> {
        self.0.documents(ids)
    }
    fn extraction_gaps(
        &self,
        filter: &ResolvedFilter,
    ) -> Result<ExtractionGaps, cm_core::ServiceError> {
        self.0.extraction_gaps(filter)
    }
    fn set_deadline(&self, deadline: Option<std::time::Instant>) {
        let past = std::time::Instant::now() - std::time::Duration::from_secs(1);
        self.0.set_deadline(deadline.map(|_| past));
    }
    fn interrupted(&self) -> bool {
        self.0.interrupted()
    }
}

/// P2-2 regression: the request deadline is enforced inside SQLite. A statement that would
/// run forever is interrupted at the deadline, an interrupted FTS lookup becomes an
/// explicit timed-out page (not an error, not a rebuild trigger) and a long statement on one
/// reader does not hold up a search on another reader.
#[test]
fn deadline_interrupts_inside_sqlite_and_readers_do_not_serialize() {
    use std::time::{Duration, Instant};
    let (_temp, directory, mut db) = file_library();
    for i in 0..40 {
        paper(&mut db, &format!("電池 electrolyte {i}"), 2020, "x");
    }
    // An endless statement on reader A stops at its deadline.
    let slow = std::thread::spawn({
        let directory = directory.clone();
        move || {
            let reader = super::SearchReader::open(&directory).unwrap();
            let index = reader.search_index();
            index.set_deadline(Some(Instant::now() + Duration::from_millis(1500)));
            let started = Instant::now();
            let error = reader
                .connection
                .query_row(
                    "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c) SELECT count(*) FROM c",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_err();
            (crate::sql_fault(&error), started.elapsed())
        }
    });
    std::thread::sleep(Duration::from_millis(100));
    // Meanwhile reader B answers a search right away.
    let reader = super::SearchReader::open(&directory).unwrap();
    let started = Instant::now();
    {
        let snapshot = reader.snapshot().unwrap();
        match search(&*snapshot, &SearchRequest::new("electrolyte")).unwrap() {
            SearchReply::Results(page) => assert_eq!(page.total_hits, 40),
            other => panic!("{other:?}"),
        }
    }
    let concurrent = started.elapsed();
    let (fault, slow_elapsed) = slow.join().unwrap();
    assert_eq!(fault, crate::SqlFault::Interrupted);
    assert!(
        slow_elapsed >= Duration::from_millis(1400) && slow_elapsed < Duration::from_secs(10),
        "{slow_elapsed:?}"
    );
    assert!(
        concurrent < Duration::from_millis(1000),
        "search waited for the other reader: {concurrent:?}"
    );
    // Interrupted FTS lookup -> explicit timed-out page.
    {
        let snapshot = reader.snapshot().unwrap();
        let expired = Expired(super::SqliteSearch::new(
            snapshot.connection,
            snapshot.deadline,
        ));
        match search(&expired, &SearchRequest::new("electrolyte")).unwrap() {
            SearchReply::Results(page) => {
                assert!(page.hits.is_empty());
                assert!(page.coverage.timed_out && !page.coverage.complete);
            }
            other => panic!("{other:?}"),
        }
    }
    // An interrupted scan chunk ends the window early (partial, with a continuation).
    let index = reader.search_index();
    index.set_deadline(Some(Instant::now() - Duration::from_secs(1)));
    let window = index
        .scan(
            &["電池".into()],
            &Default::default(),
            &[DocumentSource::Paper],
            0,
            1000,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
    assert!(window.timed_out && !window.exhausted && window.examined == 0);
    index.set_deadline(None);
    // After the deadline is cleared the same reader works normally.
    let snapshot = reader.snapshot().unwrap();
    match search(&*snapshot, &SearchRequest::new("電池")).unwrap() {
        SearchReply::Results(page) => assert!(page.total_hits > 0),
        other => panic!("{other:?}"),
    }
}

/// P2-5 regression over real SQLite: a phrase spanning a PDF line break matches through
/// unicode61, and the hit carries `matched_fields`, a snippet around it and a highlight.
#[test]
fn phrase_across_pdf_line_break_gets_snippet_and_highlight() {
    let mut db = LibraryDb::in_memory().unwrap();
    let owner = paper(&mut db, "Owner", 2020, "z");
    let att = attachment(&db, owner, 4);
    let body = format!(
        "{} we study the solid\r\nelectrolyte interphase of Müller cells {}",
        "lorem ".repeat(100),
        "ipsum ".repeat(100)
    );
    page(&db, att, 0, "1", &body);
    for (q, expected) in [
        ("\"solid electrolyte\"", "solid  electrolyte"),
        ("muller", "Müller"),
    ] {
        let found = query(&db, q);
        assert_eq!(found.hits.len(), 1, "{q}");
        let hit = &found.hits[0];
        assert_eq!(hit.matched_fields, vec![SearchField::Body], "{q}");
        assert_eq!(hit.highlights.len(), 1, "{q}: {:?}", hit.snippet);
        let marked = &hit.snippet[hit.highlights[0].start..hit.highlights[0].end];
        assert_eq!(marked, expected, "{q}");
    }
}

/// P3: one attachment with an unsafe stored path is marked failed and skipped; the other
/// attachments are still returned as extraction targets.
#[test]
fn unsafe_attachment_path_fails_only_that_attachment() {
    let (_temp, directory, mut db) = file_library();
    let owner = paper(&mut db, "Owner", 2020, "z");
    let good = attachment(&db, owner, 5);
    let bad = attachment(&db, owner, 6);
    db.connection
        .execute(
            "UPDATE attachments SET relative_path='../outside.pdf' WHERE id=?1",
            [bad.0.to_string()],
        )
        .unwrap();
    let mut store = db.fulltext(&directory).unwrap();
    let targets = store.targets(ExtractScope::Pending, false, 10).unwrap();
    assert_eq!(
        targets.iter().map(|t| t.attachment_id).collect::<Vec<_>>(),
        vec![good]
    );
    drop(store);
    let status = db.fulltext_reader().for_attachment(bad).unwrap().unwrap();
    assert_eq!(status.state, ExtractionState::Failed);
    assert_eq!(db.fulltext_reader().unfinished().unwrap().count, 1);
}

/// P2-4 storage side: deferring keeps the attachment unfinished and resumable, counts
/// attempts, and a committed batch resets the count.
#[test]
fn deferred_attachment_stays_unfinished_with_attempt_count() {
    let (_temp, directory, mut db) = file_library();
    let owner = paper(&mut db, "Owner", 2020, "z");
    let att = attachment(&db, owner, 7);
    let mut store = db.fulltext(&directory).unwrap();
    let target = store.targets(ExtractScope::Pending, false, 10).unwrap()[0].clone();
    assert_eq!(store.defer(&target, "time budget").unwrap(), 1);
    assert_eq!(store.defer(&target, "time budget").unwrap(), 2);
    let resumed = store.targets(ExtractScope::Pending, false, 10).unwrap();
    assert_eq!((resumed[0].resume_from, resumed[0].reset), (0, false));
    drop(store);
    let before = db.fulltext_reader().unfinished().unwrap().key();
    let status = db.fulltext_reader().for_attachment(att).unwrap().unwrap();
    assert_eq!(status.state, ExtractionState::Extracting);
    let mut store = db.fulltext(&directory).unwrap();
    store
        .store(StoreBatch {
            target: &target,
            page_count: 2,
            pages: &[ExtractedPage {
                page_index: 0,
                page_label: None,
                text: "x".into(),
                truncated: false,
            }],
            next_page: 1,
            finish: None,
        })
        .unwrap();
    assert_eq!(
        store.defer(&target, "read error").unwrap(),
        1,
        "reset by progress"
    );
    let resumed = store.targets(ExtractScope::Pending, false, 10).unwrap();
    assert_eq!(resumed[0].resume_from, 1);
    drop(store);
    let after = db.fulltext_reader().unfinished().unwrap().key();
    assert_ne!(after, before, "progress changes the automatic job key");
}
