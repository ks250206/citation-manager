use cm_core::{
    ServiceError,
    attachments::{AttachmentService, ImportAttachment},
    collections::{CollectionInput, CollectionLibrary},
    export::*,
    import::*,
    papers::{NewPaper, PaperLibrary},
    ports::{AttachmentRepository, ImportService, PaperRepository},
    tags::{TagInput, TagLibrary},
};
use cm_metadata::{
    bibtex::{self, BibtexSerializer},
    exchange::*,
    ris::{self, RisSerializer},
};
use cm_storage::{LibraryDb, LibraryDirectory};
use sha2::{Digest, Sha256};

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
#[test]
fn malformed_recovery_line_numbers_no_macro_or_tex_execution() {
    let r=bibtex::parse(b"@article{bad, title={unclosed\n@book{good,title={Good},year=2026}\n@misc{macro,title=loop # loop}\n@misc{safe,title={Safe},note={\\write18{touch /tmp/no-execution}}}\n").unwrap();
    assert_eq!(r.candidates.len(), 2);
    assert_eq!(r.candidates[0].title, "Good");
    assert_eq!(r.candidates[0].source.row, Some(2));
    assert_eq!(r.candidates[1].source.row, Some(4));
    assert_eq!(
        r.diagnostics.iter().map(|v| v.line).collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert!(
        r.candidates[1].bibliography.extra_json.as_ref().unwrap()["bibtex"]
            .to_string()
            .contains("write18")
    );
    let r=ris::parse(b"TY  - JOUR\nTI  - unfinished\nTY  - BOOK\nTI  - Good\nER  -\nTY  - JOUR\nTI  - Damaged\nbroken line\nER  -\nTY  - GEN\nTI  - Safe\nER  -\n").unwrap();
    assert_eq!(r.candidates.len(), 3);
    assert_eq!(r.candidates[0].title, "Good");
    assert_eq!(r.candidates[0].source.row, Some(3));
    assert!(r.candidates[1].diagnostics.iter().any(|v| v.error));
    assert_eq!(r.candidates[2].title, "Safe");
    assert_eq!(
        r.diagnostics.iter().map(|v| v.line).collect::<Vec<_>>(),
        vec![1, 8]
    );
    let mut db = LibraryDb::in_memory().unwrap();
    let mut s = ImportCoordinator::new(db.imports());
    let p = s
        .preview(r.batch(0, 3, Default::default()).unwrap())
        .unwrap();
    assert!(s.commit(commit(&p, vec![ImportAction::Create; 3])).is_err());
    let o = s
        .commit(commit(
            &p,
            vec![
                ImportAction::Create,
                ImportAction::Skip,
                ImportAction::Create,
            ],
        ))
        .unwrap();
    assert_eq!(o.created.len(), 2);
    assert_eq!(o.skipped, 1);
}
#[test]
fn encodings_budgets_and_unsupported_syntax_are_explicit() {
    for parse in [bibtex::parse, ris::parse] {
        for input in [&b"\xff\xfeA\0"[..], &b"\0A"[..]] {
            assert!(parse(input).unwrap_err().to_string().contains("encoding"));
        }
        assert!(parse(&vec![b'a'; MAX_INPUT_BYTES + 1]).is_err());
    }
    let deep = format!(
        "@misc{{x,title={{{}x{}}}}}",
        "{".repeat(MAX_DEPTH + 1),
        "}".repeat(MAX_DEPTH + 1)
    );
    assert!(bibtex::parse(deep.as_bytes()).is_err());
    let huge = format!(
        "TY  - JOUR\nTI  - {}\nER  -\n",
        "a".repeat(cm_metadata::exchange::MAX_RECORD_BYTES)
    );
    assert!(ris::parse(huge.as_bytes()).is_err());
    let many = "TY  - JOUR\nTI  - x\nER  -\n".repeat(cm_metadata::exchange::MAX_RECORDS + 1);
    assert!(ris::parse(many.as_bytes()).is_err());
    let many = "@misc{x,title={x}}\n".repeat(cm_metadata::exchange::MAX_RECORDS + 1);
    assert!(bibtex::parse(many.as_bytes()).is_err());
    let b = bibtex::parse(
        b"@string{loop=loop}\n@misc{x,title={x},crossref={missing},note={\\LaTeX{}}}\n",
    )
    .unwrap();
    assert!(b.diagnostics[0].error);
    assert!(
        b.candidates[0]
            .diagnostics
            .iter()
            .any(|v| v.field == "crossref")
    );
    let duplicate = bibtex::parse(b"@misc{x,title={first},title={second}}\n").unwrap();
    assert!(duplicate.candidates[0].diagnostics.iter().any(|v| v.error));
    let duplicate = ris::parse(b"TY  - GEN\nTI  - first\nT1  - second\nER  -\n").unwrap();
    assert!(duplicate.candidates[0].diagnostics.iter().any(|v| v.error));
    for r in [
        bibtex::parse(b"@misc{x,title={x},cm_data={ffff}}\n").unwrap(),
        ris::parse(b"TY  - GEN\nTI  - x\nC9  - cm-v1:ffff\nER  -\n").unwrap(),
    ] {
        assert!(r.candidates[0].diagnostics.iter().any(|v| v.error));
    }
}
#[test]
fn names_escaping_unknown_fields_are_observable_without_extension() {
    let r=bibtex::parse("@BOOK{K,title={Protection {DNA}: \\{Ω\\} \\% \\& \\# \\_ \\$ \\textbackslash{}path},author={von Neumann, John and\n{ACME and Sons} AND 山田, 花子},publisher={Press Ω},custom={A \\& B}}".as_bytes()).unwrap();
    let d = &r.candidates[0];
    assert_eq!(d.title, "Protection DNA: {Ω} % & # _ $ \\path");
    assert_eq!(d.bibliography.authors.len(), 3);
    assert_eq!(
        d.bibliography.authors[0].family_name.as_deref(),
        Some("von Neumann")
    );
    assert_eq!(
        d.bibliography.authors[1].literal_name.as_deref(),
        Some("ACME and Sons")
    );
    let mut db = LibraryDb::in_memory().unwrap();
    let mut s = ImportCoordinator::new(db.imports());
    let p = s
        .preview(r.batch(0, 1, Default::default()).unwrap())
        .unwrap();
    let id = s
        .commit(commit(&p, vec![ImportAction::Create]))
        .unwrap()
        .created[0];
    drop(s);
    let p = db.papers().get(id).unwrap().unwrap();
    let encoded = BibtexSerializer.encode(&p).unwrap();
    assert!(encoded.text.contains("publisher = {Press Ω}"));
    assert!(encoded.text.contains("custom = {A \\& B}"));
    let again = bibtex::parse(encoded.text.as_bytes()).unwrap();
    assert_eq!(again.candidates[0].title, p.title);
    assert_eq!(again.candidates[0].bibliography, p.bibliography);
}
#[test]
fn adapter_merge_skip_cancel_keys_dryrun_preserve_real_attachments_and_relationships() {
    for is_bib in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let dir =
            LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("library")).unwrap();
        let mut db = LibraryDb::open(&dir).unwrap();
        let collection = db
            .collections()
            .create(CollectionInput::named("existing"))
            .unwrap();
        let added = db
            .collections()
            .create(CollectionInput::named("approved"))
            .unwrap();
        let tag = db.tags().create(TagInput::named("keep")).unwrap();
        let mut old = NewPaper::titled("Existing title");
        old.bibliography.doi = Some("10.9999/existing".into());
        old.bibliography.citation_key = Some("Collision".into());
        let old = db.papers().create(old).unwrap();
        db.collections()
            .set_membership(old.id, collection.id, true)
            .unwrap();
        db.tags().set_membership(old.id, tag.id, true).unwrap();
        let source = temp.path().join("paper.pdf");
        let bytes = b"%PDF-1.7\nsynthetic preservation check\n%%EOF";
        std::fs::write(&source, bytes).unwrap();
        let attachment = AttachmentService::new(db.managed_attachments(&dir).unwrap())
            .import(ImportAttachment {
                paper_id: old.id,
                source: source.canonicalize().unwrap(),
                original_name: "paper.pdf".into(),
                media_type: "application/pdf".into(),
                expected_sha256: Some(Sha256::digest(bytes).into()),
            })
            .unwrap();
        let input = if is_bib {
            b"@article{Collision,title={Updated title},doi={10.9999/existing}}\n@book{Collision,title={New distinct book},doi={10.9999/new}}\n@misc{,title={Missing key generated},doi={10.9999/missing}}\n".as_slice()
        } else {
            b"TY  - JOUR\nID  - Collision\nTI  - Updated title\nDO  - 10.9999/existing\nER  -\nTY  - BOOK\nID  - Collision\nTI  - New distinct book\nDO  - 10.9999/new\nER  -\nTY  - GEN\nTI  - Missing key generated\nDO  - 10.9999/missing\nER  -\n".as_slice()
        };
        let report = if is_bib {
            bibtex::parse(input)
        } else {
            ris::parse(input)
        }
        .unwrap();
        let database = std::fs::read(dir.database_path()).unwrap();
        let wal = dir.database_path().with_extension("sqlite-wal");
        let before = std::fs::read(&wal).unwrap();
        let request = || {
            report
                .batch(
                    0,
                    3,
                    ImportDestination {
                        collection: Some(added.id),
                        tags: vec![tag.id],
                    },
                )
                .unwrap()
        };
        let mut s = ImportCoordinator::new(db.imports());
        let p = s.preview(request()).unwrap();
        assert_eq!(p.candidates[0].existing_match, Some(old.id));
        assert_eq!(
            p.candidates[1].draft.bibliography.citation_key.as_deref(),
            Some("Collision_2")
        );
        assert!(p.candidates[2].draft.bibliography.citation_key.is_some());
        s.cancel(&p.id).unwrap();
        assert!(s.commit(commit(&p, vec![ImportAction::Create; 3])).is_err());
        assert_eq!(std::fs::read(dir.database_path()).unwrap(), database);
        assert_eq!(std::fs::read(&wal).unwrap(), before);
        let p = s.preview(request()).unwrap();
        let o = s
            .commit(commit(
                &p,
                vec![
                    ImportAction::Merge {
                        match_index: 0,
                        fields: vec!["title".into()],
                    },
                    ImportAction::Create,
                    ImportAction::Skip,
                ],
            ))
            .unwrap();
        assert_eq!(o.created.len(), 1);
        assert_eq!(o.merged, vec![old.id]);
        assert_eq!(o.skipped, 1);
        drop(s);
        assert_eq!(
            db.papers().get(old.id).unwrap().unwrap().title,
            "Updated title"
        );
        assert_eq!(
            db.attachments().get(attachment.id).unwrap(),
            Some(attachment.clone())
        );
        assert_eq!(
            std::fs::read(dir.path().join(attachment.storage_key.as_str())).unwrap(),
            bytes
        );
        assert_eq!(std::fs::read(source).unwrap(), bytes);
        assert_eq!(db.collections().for_paper(old.id).unwrap().len(), 2);
        assert_eq!(db.tags().for_paper(old.id).unwrap().len(), 1);
        let all = ExportService::new(db.library_query())
            .export(ExportSelection::All, &RisSerializer)
            .unwrap();
        assert_eq!(all.records, 2);
        assert!(all.migration_notice.contains("annotations"));
    }
}
#[test]
fn export_missing_trashed_duplicate_ids_and_unsafe_keys_fail_explicitly() {
    let mut db = LibraryDb::in_memory().unwrap();
    let p = db.papers().create(NewPaper::titled("Export")).unwrap();
    assert!(
        ExportService::new(db.library_query())
            .export(ExportSelection::Ids(vec![p.id, p.id]), &BibtexSerializer)
            .is_err()
    );
    db.papers().set_deleted(p.id, true).unwrap();
    assert_eq!(
        ExportService::new(db.library_query())
            .export(ExportSelection::Ids(vec![p.id]), &BibtexSerializer),
        Err(ServiceError::NotFound)
    );
    let r = ExportService::new(db.library_query())
        .export(ExportSelection::All, &BibtexSerializer)
        .unwrap();
    assert_eq!(r.records, 0);
    let mut p = p;
    p.bibliography.citation_key = Some("bad}, injected".into());
    assert!(BibtexSerializer.encode(&p).is_err());
    p.title = "x".repeat(cm_core::export::MAX_RECORD_BYTES + 1);
    assert!(RisSerializer.encode(&p).is_err());
}

#[test]
fn extensions_preserve_non_native_metadata_and_detect_edited_public_fields() {
    use cm_protocol::domain::{Contributor, ExternalId};
    let mut db = LibraryDb::in_memory().unwrap();
    let mut input = NewPaper::titled("Version Ω");
    input.bibliography.paper_type = Some("dataset / custom".into());
    input.bibliography.publication_date = Some("2026-11-07".into());
    input.bibliography.year = Some(2026);
    input.bibliography.authors = vec![
        Contributor {
            given_name: Some("A and B".into()),
            family_name: Some("姓".into()),
            literal_name: None,
            orcid: Some("0000-0001-2345-6789".into()),
            role: "author".into(),
        },
        Contributor {
            given_name: None,
            family_name: None,
            literal_name: Some("Translator Ω".into()),
            orcid: None,
            role: "translator".into(),
        },
    ];
    input.external_ids = vec![ExternalId {
        namespace: "archive".into(),
        value: "opaque / Ω".into(),
    }];
    // Use a non-object extra_json too: extension must preserve the actual value.
    input.bibliography.extra_json = Some(serde_json::json!(["opaque",{"nested":"Ω"}]));
    let p = db.papers().create(input).unwrap();
    for (serializer, parse) in [
        (
            &BibtexSerializer as &dyn BibliographySerializer,
            bibtex::parse as fn(&[u8]) -> Result<ParseReport, ServiceError>,
        ),
        (&RisSerializer, ris::parse),
    ] {
        let encoded = serializer.encode(&p).unwrap();
        let parsed = parse(encoded.text.as_bytes()).unwrap();
        assert_eq!(parsed.candidates[0].bibliography, p.bibliography);
        assert_eq!(parsed.candidates[0].external_ids, p.external_ids);
        let edited = encoded.text.replace("Version Ω", "Edited Ω");
        let changed = parse(edited.as_bytes()).unwrap();
        assert_eq!(changed.candidates[0].title, "Edited Ω");
        assert!(
            changed.candidates[0]
                .diagnostics
                .iter()
                .any(|d| d.error && d.field == "cm-v1")
        );
    }
    // Unknown accent commands are kept verbatim, with explicit unsupported warnings.
    let r =
        bibtex::parse(br#"@misc{x,title={Garc\'{i}a and \"{o}},author={Garc\'{i}a, Jos\'{e}}}"#)
            .unwrap();
    assert_eq!(r.candidates[0].title, r#"Garc\'{i}a and \"{o}"#);
    assert!(r.candidates[0].diagnostics.iter().any(|d| d.field == "tex"));
}

#[test]
fn multiline_record_markers_crlf_and_output_budgets_do_not_corrupt_records() {
    let mut db = LibraryDb::in_memory().unwrap();
    let mut input = NewPaper::titled("Markers");
    input.bibliography.abstract_text =
        Some("first\n@article{literal, not a record}\nTY  - JOUR\nER  -\n\nlast".into());
    let p = db.papers().create(input).unwrap();
    let bib = BibtexSerializer.encode(&p).unwrap();
    let bib = bibtex::parse(bib.text.as_bytes()).unwrap();
    assert_eq!(bib.candidates.len(), 1);
    assert_eq!(bib.candidates[0].bibliography, p.bibliography);
    let ris = RisSerializer.encode(&p).unwrap();
    let crlf = ris.text.replace('\n', "\r\n");
    let ris = ris::parse(crlf.as_bytes()).unwrap();
    assert_eq!(ris.candidates.len(), 1);
    assert_eq!(ris.candidates[0].bibliography, p.bibliography);
    // Direct serializers also enforce limits, independent of ExportService.
    let mut changed = p.clone();
    changed.bibliography.abstract_text = Some("a\rb".into());
    assert!(
        RisSerializer
            .encode(&changed)
            .unwrap_err()
            .to_string()
            .contains("carriage")
    );
    let mut nested = serde_json::Value::Null;
    for _ in 0..34 {
        nested = serde_json::json!([nested]);
    }
    changed.bibliography.extra_json = Some(nested);
    assert!(
        BibtexSerializer
            .encode(&changed)
            .unwrap_err()
            .to_string()
            .contains("nesting")
    );
    struct Large;
    impl BibliographySerializer for Large {
        fn encode(&self, _: &cm_protocol::domain::Paper) -> Result<EncodedRecord, ServiceError> {
            Ok(EncodedRecord {
                text: "x".repeat(cm_core::export::MAX_RECORD_BYTES),
                diagnostics: vec![],
            })
        }
    }
    // 129 * 256 KiB crosses the output cap. Error returns no partial report.
    for _ in 0..128 {
        db.papers()
            .create(NewPaper::titled("Budget probe"))
            .unwrap();
    }
    let err = ExportService::new(db.library_query())
        .export(ExportSelection::All, &Large)
        .unwrap_err();
    assert!(err.to_string().contains("output"));
    assert_eq!(
        ExportService::new(db.library_query())
            .export(ExportSelection::All, &RisSerializer)
            .unwrap()
            .records,
        129
    );
}
