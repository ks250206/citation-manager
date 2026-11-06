use cm_core::{
    collections::{CollectionInput, CollectionLibrary},
    export::*,
    import::*,
    library_query::{LibraryQuery, LibraryQueryRepository},
    ports::{ImportService, PaperRepository},
};
use cm_domain::PaperId;
use cm_metadata::{
    bibtex::{self, BibtexSerializer},
    exchange::ParseReport,
    ris::{self, RisSerializer},
};
use cm_protocol::domain as cm_domain;
use cm_storage::{LibraryDb, LibraryDirectory};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const BIB: &[u8] = include_bytes!("../../../fixtures/bibtex/thousand.bib");
const RIS: &[u8] = include_bytes!("../../../fixtures/ris/thousand.ris");
fn decisions(p: &ImportPreview, actions: Vec<ImportAction>) -> ImportCommit {
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
fn values(d: &PaperDraft) -> (String, cm_domain::Bibliography, Vec<cm_domain::ExternalId>) {
    let d = normalize::normalize(d).unwrap();
    (d.title, d.bibliography, d.external_ids)
}
fn compare(report: ParseReport, expected: &BTreeMap<String, PaperDraft>) {
    assert_eq!(report.candidates.len(), expected.len());
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    for d in report.candidates {
        let key = d.bibliography.doi.as_ref().unwrap();
        assert!(
            !d.diagnostics.iter().any(|v| v.error),
            "{:?}",
            d.diagnostics
        );
        assert_eq!(values(&d), values(&expected[key]), "DOI {key}");
    }
}
fn run(
    input: &[u8],
    parse: fn(&[u8]) -> Result<ParseReport, cm_core::ServiceError>,
    serializer: &impl BibliographySerializer,
) {
    let temp = tempfile::tempdir().unwrap();
    let dir =
        LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("library")).unwrap();
    let source = temp.path().join("source");
    std::fs::write(&source, input).unwrap();
    let mut db = LibraryDb::open(&dir).unwrap();
    let collection = db
        .collections()
        .create(CollectionInput::named("Reviewed batch destination"))
        .unwrap();
    let parsed = parse(input).unwrap();
    assert_eq!(parsed.candidates.len(), 1000);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.sha256, <[u8; 32]>::from(Sha256::digest(input)));
    assert!(parsed.batch(0, 1000, Default::default()).is_err());
    let mut expected = BTreeMap::new();
    let mut ids = vec![];
    // The only writes are explicit commits after reviewing each fresh retained preview.
    for start in (0..1000).step_by(100) {
        let dest = ImportDestination {
            collection: if start < 300 {
                Some(collection.id)
            } else {
                None
            },
            tags: vec![],
        };
        let before = std::fs::read(dir.database_path()).unwrap();
        let wal = dir.database_path().with_extension("sqlite-wal");
        let wal_before = std::fs::read(&wal).unwrap();
        let mut s = ImportCoordinator::new(db.imports());
        let p = s.preview(parsed.batch(start, 100, dest).unwrap()).unwrap();
        assert_eq!(std::fs::read(dir.database_path()).unwrap(), before);
        assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
        for c in &p.candidates {
            assert!(
                !c.diagnostics.iter().any(|v| v.error),
                "{:?}",
                c.diagnostics
            );
            assert!(c.matches.is_empty(), "unintended fixture similarity");
            assert_eq!(c.draft.source.sha256, Some(parsed.sha256));
            assert!(c.draft.source.row.unwrap() > 0);
            expected.insert(c.draft.bibliography.doi.clone().unwrap(), c.draft.clone());
        }
        let o = s
            .commit(decisions(&p, vec![ImportAction::Create; 100]))
            .unwrap();
        assert_eq!(o.created.len(), 100);
        assert_eq!(o.rows.len(), 100);
        for r in &o.rows {
            assert_eq!(r.source.sha256, Some(parsed.sha256));
        }
        ids.extend(o.created);
    }
    // Reopen real SQLite so comparisons cannot be satisfied by retained DTOs alone.
    drop(db);
    let mut db = LibraryDb::open(&dir).unwrap();
    for id in &ids {
        let p = db.papers().get(*id).unwrap().unwrap();
        let expected = &expected[p.bibliography.doi.as_ref().unwrap()];
        assert_eq!((p.title, p.bibliography, p.external_ids), values(expected));
    }
    let service = ExportService::new(db.library_query());
    let all = service.export(ExportSelection::All, serializer).unwrap();
    assert_eq!(all.records, 1000);
    assert_eq!(all.diagnostics.len(), 1000);
    assert!(all.migration_notice.contains("PDFs"));
    compare(parse(all.text.as_bytes()).unwrap(), &expected);
    let chosen: Vec<PaperId> = vec![ids[0], ids[250], ids[999]];
    let selected = service
        .export(ExportSelection::Ids(chosen.clone()), serializer)
        .unwrap();
    assert_eq!(selected.records, 3);
    let mut expected_selection = BTreeMap::new();
    for id in chosen {
        let p = db.papers().get(id).unwrap().unwrap();
        let key = p.bibliography.doi.unwrap();
        expected_selection.insert(key.clone(), expected[&key].clone());
    }
    compare(
        parse(selected.text.as_bytes()).unwrap(),
        &expected_selection,
    );
    let members = ExportService::new(db.library_query())
        .export(ExportSelection::Collection(collection.id), serializer)
        .unwrap();
    assert_eq!(members.records, 300);
    let mut expected_members = BTreeMap::new();
    for id in &ids[..300] {
        let p = db.papers().get(*id).unwrap().unwrap();
        let key = p.bibliography.doi.unwrap();
        expected_members.insert(key.clone(), expected[&key].clone());
    }
    compare(parse(members.text.as_bytes()).unwrap(), &expected_members);
    assert_eq!(std::fs::read(source).unwrap(), input);
    assert_eq!(
        db.library_query()
            .query(&LibraryQuery {
                limit: 500,
                ..Default::default()
            })
            .unwrap()
            .papers
            .len(),
        500
    );
    // Both native serializers also preserve the exact common model across formats.
    let cross = ExportService::new(db.library_query())
        .export(ExportSelection::All, &BibtexSerializer)
        .unwrap();
    compare(bibtex::parse(cross.text.as_bytes()).unwrap(), &expected);
    let cross = ExportService::new(db.library_query())
        .export(ExportSelection::All, &RisSerializer)
        .unwrap();
    compare(ris::parse(cross.text.as_bytes()).unwrap(), &expected);
}
#[test]
fn thousand_bibtex_real_sqlite_all_selected_collection_roundtrip() {
    run(BIB, bibtex::parse, &BibtexSerializer);
}
#[test]
fn thousand_ris_real_sqlite_all_selected_collection_roundtrip() {
    run(RIS, ris::parse, &RisSerializer);
}

#[test]
fn original_fixtures_have_observable_unicode_ordered_authors_and_fields() {
    for report in [bibtex::parse(BIB).unwrap(), ris::parse(RIS).unwrap()] {
        let p = &report.candidates[0];
        let b = &p.bibliography;
        assert_eq!(b.authors.len(), 4);
        assert_eq!(b.authors[0].given_name.as_deref(), Some("José"));
        assert_eq!(
            b.authors[1].literal_name.as_deref(),
            Some("研究 and Development Consortium")
        );
        assert_eq!(b.authors[2].given_name.as_deref(), Some("花子"));
        assert_eq!(b.authors[3].role, "editor");
        assert_eq!(b.citation_key.as_deref(), Some("Fixture0"));
        assert_eq!(b.doi.as_deref(), Some("https://doi.org/10.9999/FIXTURE.0"));
        assert_eq!(b.extra_json.as_ref().unwrap()["accessed"], "2026-11-07");
        assert_eq!(b.volume.as_deref(), Some("1"));
        assert_eq!(b.issue.as_deref(), Some("1"));
        assert!(p.title.ends_with("A & B {literal}"));
    }
}
