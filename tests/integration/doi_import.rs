//! Worker composition integration only. HTTP transport has separate real-loopback tests
//! in cm-metadata; this harness injects neutral provider replies and uses genuine #33 SQLite.
use cm_core::{
    ServiceError,
    import::*,
    metadata_resolve::*,
    papers::{NewPaper, PaperLibrary, PaperList},
    ports::{ImportService, PaperRepository},
};
use cm_storage::{LibraryDb, LibraryDirectory};

struct Provider;
impl MetadataProvider for Provider {
    fn lookup(
        &mut self,
        provider: cm_core::metadata_resolve::Provider,
        doi: &str,
        _: &Cancellation,
    ) -> ProviderReply {
        use cm_core::metadata_resolve::Provider as Kind;
        let mut draft = PaperDraft::titled("Fetched Ω title");
        draft.bibliography.doi = Some(doi.into());
        draft.bibliography.abstract_text = Some("Fetched abstract".into());
        draft.bibliography.year = Some(2024);
        draft.bibliography.journal = Some("Fetched journal".into());
        draft.source = ImportSource {
            adapter: provider.name().into(),
            locator: Some(doi.into()),
            ..Default::default()
        };
        ProviderReply {
            outcome: ProviderOutcome {
                provider,
                status: if provider == Kind::Crossref {
                    ProviderStatus::Found
                } else {
                    ProviderStatus::NotFound
                },
                attempts: 1,
                cached: false,
                retry_after_seconds: None,
            },
            record: (provider == Kind::Crossref).then_some(ProviderRecord {
                draft,
                pdf_candidates: vec![],
            }),
        }
    }
}
fn resolution(doi: &str) -> MetadataResolution {
    MetadataResolveService::new(Provider)
        .resolve(doi, &Cancellation::default())
        .unwrap()
}
fn commit(p: &ImportPreview, action: ImportAction) -> ImportCommit {
    ImportCommit {
        preview_id: p.id.clone(),
        decisions: vec![ImportDecision {
            candidate_index: 0,
            action,
        }],
    }
}
fn bytes(dir: &LibraryDirectory) -> (Vec<u8>, Vec<u8>) {
    (
        std::fs::read(dir.database_path()).unwrap(),
        std::fs::read(dir.database_path().with_extension("sqlite-wal")).unwrap(),
    )
}
#[test]
fn resolve_preview_cancel_is_byte_unchanged_and_only_approved_merge_fields_are_written() {
    let temp = tempfile::tempdir().unwrap();
    let dir =
        LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("library")).unwrap();
    let mut db = LibraryDb::open(&dir).unwrap();
    let mut input = NewPaper::titled("Original title");
    input.bibliography.doi = Some("10.99/test".into());
    input.bibliography.abstract_text = Some("Private original abstract".into());
    input.bibliography.journal = Some("Original journal".into());
    input.bibliography.citation_key = Some("Keep2024".into());
    let old = db.papers().create(input).unwrap();
    let before = bytes(&dir);
    let r = resolution("https://doi.org/10.99/TEST");
    assert_eq!(bytes(&dir), before);
    let mut imports = ImportCoordinator::new(db.imports());
    let p = r.preview(&mut imports, Default::default()).unwrap();
    assert_eq!(p.candidates[0].matches[0].kind, MatchKind::Doi);
    assert!(
        p.candidates[0].matches[0]
            .differences
            .iter()
            .any(|d| d.field == "abstract")
    );
    assert_eq!(
        imports.commit(commit(&p, ImportAction::Create)),
        Err(ServiceError::Conflict)
    );
    imports.cancel(&p.id).unwrap();
    drop(imports);
    assert_eq!(bytes(&dir), before);
    assert_eq!(db.papers().get(old.id).unwrap(), Some(old.clone()));
    let mut imports = ImportCoordinator::new(db.imports());
    let p = r.preview(&mut imports, Default::default()).unwrap();
    let out = imports
        .commit(commit(
            &p,
            ImportAction::Merge {
                match_index: 0,
                fields: vec!["title".into()],
            },
        ))
        .unwrap();
    assert_eq!(out.merged, vec![old.id]);
    drop(imports);
    let new = db.papers().get(old.id).unwrap().unwrap();
    let mut expected = old;
    expected.title = "Fetched Ω title".into();
    expected.updated_at = new.updated_at.clone();
    assert_eq!(new, expected);
    drop(db);
    let mut db = LibraryDb::open(&dir).unwrap();
    assert_eq!(db.papers().get(new.id).unwrap(), Some(new));
    assert_eq!(db.papers().list(&PaperList::default()).unwrap().len(), 1);
}
#[test]
fn resolved_draft_uses_shared_external_ids_bibliographic_suggestions_and_keys() {
    use cm_protocol::domain::{Contributor, ExternalId};
    let mut db = LibraryDb::in_memory().unwrap();
    let mut r = resolution("10.99/test");
    r.draft.bibliography.authors = vec![Contributor {
        given_name: Some("Zoë".into()),
        family_name: Some("Ångström".into()),
        literal_name: None,
        orcid: None,
        role: "author".into(),
    }];
    r.draft.external_ids.push(ExternalId {
        namespace: "PMID".into(),
        value: "00042".into(),
    });
    let mut id_match = NewPaper::titled("Another title");
    id_match.external_ids = vec![ExternalId {
        namespace: "pmid".into(),
        value: "42".into(),
    }];
    let external = db.papers().create(id_match).unwrap();
    let mut fuzzy = NewPaper::titled(&r.draft.title);
    fuzzy.bibliography.authors = r.draft.bibliography.authors.clone();
    fuzzy.bibliography.year = r.draft.bibliography.year;
    let similar = db.papers().create(fuzzy).unwrap();
    let mut imports = ImportCoordinator::new(db.imports());
    let p = r.preview(&mut imports, Default::default()).unwrap();
    let row = &p.candidates[0];
    assert!(
        row.matches
            .iter()
            .any(|m| m.existing.id == external.id && m.kind == MatchKind::ExternalId)
    );
    assert!(
        row.matches
            .iter()
            .any(|m| m.existing.id == similar.id && m.kind == MatchKind::TitleAuthorYear)
    );
    assert!(row.draft.bibliography.citation_key.is_some());
    imports.cancel(&p.id).unwrap();
}
#[test]
fn resolver_keeps_witnessed_legacy_values_and_reports_unwitnessed_ambiguity() {
    let temp = tempfile::tempdir().unwrap();
    let dir =
        LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("library")).unwrap();
    drop(LibraryDb::open(&dir).unwrap());
    // Load the existing actual old-API-produced synthetic fixture without adding a
    // forbidden metadata/storage driver dependency or changing production migrations.
    let sql = include_str!("../../crates/storage/tests/fixtures/legacy_doi/old_api.sql");
    let setup=std::process::Command::new("python3").args(["-c","import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.executescript(sys.argv[2]); c.close()"])
        .arg(dir.database_path()).arg(sql).output().unwrap();
    assert!(setup.status.success(), "fixture setup failed");
    let mut db = LibraryDb::open(&dir).unwrap();
    let old = db.papers().list(&PaperList::default()).unwrap().remove(0);
    let initial = bytes(&dir);
    let r = resolution(&old.external_ids[0].value);
    let mut imports = ImportCoordinator::new(db.imports());
    let p = r.preview(&mut imports, Default::default()).unwrap();
    assert!(
        p.candidates[0]
            .matches
            .iter()
            .any(|m| m.existing.id == old.id && m.kind == MatchKind::Doi)
    );
    imports.cancel(&p.id).unwrap();
    drop(imports);
    assert_eq!(bytes(&dir), initial);
    let mut imports = ImportCoordinator::new(db.imports());
    let p = r.preview(&mut imports, Default::default()).unwrap();
    let index = p.candidates[0]
        .matches
        .iter()
        .position(|m| m.existing.id == old.id)
        .unwrap();
    imports
        .commit(commit(
            &p,
            ImportAction::Merge {
                match_index: index,
                fields: vec!["title".into()],
            },
        ))
        .unwrap();
    drop(imports);
    let saved = db.papers().get(old.id).unwrap().unwrap();
    let mut expected = old;
    expected.title = "Fetched Ω title".into();
    expected.updated_at = saved.updated_at.clone();
    assert_eq!(saved, expected);
    let mut input = NewPaper::titled("Unwitnessed historical spelling");
    input.bibliography.doi = Some("10.99/abc%3c493%3e".into());
    let literal = db.papers().create(input).unwrap();
    let r = resolution("https://doi.org/10.99/abc%3C493%3E");
    let mut imports = ImportCoordinator::new(db.imports());
    let p = r.preview(&mut imports, Default::default()).unwrap();
    assert!(
        !p.candidates[0]
            .matches
            .iter()
            .any(|m| m.existing.id == literal.id && m.kind.exact())
    );
    imports.cancel(&p.id).unwrap();
}
