use cm_core::{
    ServiceError,
    import::{file_metadata::*, normalize::*, *},
    papers::normalize_doi,
    ports::ImportService,
};
use cm_domain::{Contributor, ExternalId};
use std::time::Duration;

#[test]
fn normalizer_preserves_full_unicode_bibliography_and_resolves_identifiers() {
    let mut d = PaperDraft::titled("　固体\t電解質 É e\u{301} Ω ");
    d.bibliography.authors = vec![
        Contributor {
            given_name: Some(" Ryoji　 K ".into()),
            family_name: Some(" 菅野 ".into()),
            literal_name: None,
            orcid: Some("original-orcid".into()),
            role: " AUTHOR ".into(),
        },
        Contributor {
            given_name: None,
            family_name: None,
            literal_name: Some(" 組織　名 ".into()),
            orcid: None,
            role: "editor".into(),
        },
    ];
    d.bibliography.publication_date = Some("2024/2/29".into());
    d.bibliography.pages = Some(" S1 -- S12 ".into());
    d.bibliography.abstract_text = Some("要旨\n二行".into());
    d.bibliography.extra_json = Some(serde_json::json!({"custom":["原文",1]}));
    d.external_ids = vec![
        ExternalId {
            namespace: "DOI".into(),
            value: "https://doi.org/10.12/a%2Fb?tracking=1#frag".into(),
        },
        ExternalId {
            namespace: "PMID".into(),
            value: "000123".into(),
        },
        ExternalId {
            namespace: "CustomΩ".into(),
            value: "X-Y".into(),
        },
    ];
    let n = normalize(&d).unwrap();
    assert_eq!(n.title, "固体 電解質 É e\u{301} Ω");
    assert_eq!(
        n.bibliography.authors[0].given_name.as_deref(),
        Some("Ryoji K")
    );
    assert_eq!(n.bibliography.authors[1].role, "editor");
    assert_eq!(n.bibliography.year, Some(2024));
    assert_eq!(
        n.bibliography.publication_date.as_deref(),
        Some("2024-02-29")
    );
    assert_eq!(n.bibliography.pages.as_deref(), Some("S1–S12"));
    assert_eq!(n.bibliography.doi.as_deref(), Some("10.12/a/b"));
    assert_eq!(n.external_ids[1].value, "123");
    assert_eq!(n.external_ids[2], d.external_ids[2]);
    assert_eq!(n.bibliography.extra_json, d.bibliography.extra_json);
    assert_eq!(normalize(&n).unwrap(), n);
}
#[test]
fn doi_one_shared_canonical_point_distinguishes_raw_and_resolver_escapes() {
    assert_eq!(normalize_doi("10.12/a%2Fb").unwrap(), "10.12/a%2fb");
    assert_eq!(
        normalize_doi("https://DOI.org/10.12/a%252Fb").unwrap(),
        "10.12/a%2fb"
    );
    assert_eq!(
        normalize_doi("https://doi.org/10.12/%CE%A9").unwrap(),
        "10.12/Ω"
    );
    for d in [
        "https://doi.org/10.12/a%ZZ",
        "https://doi.org/10.12/a%00",
        "https://doi.org/10.12/a%20",
        "https://doi.org/10.12/%FF",
        "https://doi.org/10.12%2Fa",
    ] {
        assert!(normalize_doi(d).is_err(), "{d}");
    }
    let mut d = PaperDraft::titled("Disagreement");
    d.bibliography.doi = Some("10.12/a".into());
    d.external_ids.push(ExternalId {
        namespace: "doi".into(),
        value: "10.12/b".into(),
    });
    assert!(normalize(&d).is_err());
    for date in [
        "2023-02-29",
        "2024-04-31",
        "2024-00",
        "2024-13",
        "2024-2-0",
        "2024-2-1-1",
    ] {
        d.external_ids.clear();
        d.bibliography.publication_date = Some(date.into());
        assert!(normalize(&d).is_err(), "{date}");
    }
}
#[test]
fn input_limits_are_explicit_and_never_truncate_fields() {
    let d = PaperDraft::titled("x".repeat(MAX_TITLE_BYTES));
    assert_eq!(normalize(&d).unwrap().title.len(), MAX_TITLE_BYTES);
    assert!(normalize(&PaperDraft::titled("x".repeat(MAX_TITLE_BYTES + 1))).is_err());
    let mut d = PaperDraft::titled("x");
    d.bibliography.abstract_text = Some("x".repeat(MAX_DRAFT_BYTES));
    assert!(normalize(&d).is_err());
    let mut used = std::collections::HashSet::new();
    let mut d = PaperDraft::titled("日本語");
    d.bibliography.citation_key = Some("Original".into());
    assert_eq!(citation_keys::allocate(&d, &mut used), "Original");
    assert_eq!(citation_keys::allocate(&d, &mut used), "Original_2");
}
struct Mock {
    applies: usize,
}
impl ImportBackend for Mock {
    fn snapshot(&mut self, _: &ImportDestination) -> Result<ImportSnapshot, ServiceError> {
        Ok(ImportSnapshot {
            revision: "1".into(),
            papers: vec![],
        })
    }
    fn apply(&mut self, p: &PreparedImport) -> Result<ImportOutcome, ServiceError> {
        p.writes(&ImportSnapshot {
            revision: "1".into(),
            papers: vec![],
        })?;
        self.applies += 1;
        Ok(ImportOutcome {
            skipped: p.preview().candidates.len(),
            ..Default::default()
        })
    }
}
fn request(candidates: Vec<PaperDraft>) -> ImportRequest {
    ImportRequest {
        candidates,
        destination: Default::default(),
    }
}
#[test]
fn future_adapters_use_real_coordinator_with_mock_port_and_bounded_handles() {
    let mut s = ImportCoordinator::new(Mock { applies: 0 });
    let bad = PaperDraft::titled(" ");
    let p = s
        .preview(request(vec![PaperDraft::titled("Good"), bad]))
        .unwrap();
    assert!(p.candidates[1].diagnostics.iter().any(|d| d.error));
    assert!(
        s.commit(ImportCommit {
            preview_id: p.id.clone(),
            decisions: vec![
                ImportDecision {
                    candidate_index: 0,
                    action: ImportAction::Skip
                };
                MAX_BATCH + 1
            ]
        })
        .is_err()
    );
    assert!(
        s.commit(ImportCommit {
            preview_id: p.id.clone(),
            decisions: vec![ImportDecision {
                candidate_index: 0,
                action: ImportAction::Merge {
                    match_index: 0,
                    fields: vec!["x".repeat(129)]
                }
            }]
        })
        .is_err()
    );
    assert!(
        s.commit(ImportCommit {
            preview_id: p.id.clone(),
            decisions: vec![]
        })
        .is_err()
    );
    assert!(
        s.commit(ImportCommit {
            preview_id: p.id.clone(),
            decisions: vec![
                ImportDecision {
                    candidate_index: 0,
                    action: ImportAction::Skip
                };
                2
            ]
        })
        .is_err()
    );
    let result = s
        .commit(ImportCommit {
            preview_id: p.id.clone(),
            decisions: (0..2)
                .map(|i| ImportDecision {
                    candidate_index: i,
                    action: ImportAction::Skip,
                })
                .collect(),
        })
        .unwrap();
    assert_eq!(result.skipped, 2);
    assert_eq!(
        s.commit(ImportCommit {
            preview_id: p.id,
            decisions: vec![]
        }),
        Err(ServiceError::NotFound)
    );
    let p = s
        .preview(request(vec![PaperDraft::titled("Cancel")]))
        .unwrap();
    s.cancel(&p.id).unwrap();
    assert_eq!(s.cancel(&p.id), Err(ServiceError::NotFound));
    let p = s
        .preview(request(vec![PaperDraft::titled("Local")]))
        .unwrap();
    let mut other = ImportCoordinator::new(Mock { applies: 0 });
    assert_eq!(
        other.commit(ImportCommit {
            preview_id: p.id,
            decisions: vec![]
        }),
        Err(ServiceError::NotFound)
    );
    for _ in 0..3 {
        s.preview(request(vec![PaperDraft::titled("Capacity")]))
            .unwrap();
    }
    assert!(
        s.preview(request(vec![PaperDraft::titled("Overflow")]))
            .is_err()
    );
    assert_eq!(s.into_backend().applies, 1);
    let mut expired = ImportCoordinator::with_policy(
        Mock { applies: 0 },
        ImportPolicy {
            lifetime: Duration::ZERO,
            max_previews: 1,
        },
    );
    let p = expired
        .preview(request(vec![PaperDraft::titled("Expire")]))
        .unwrap();
    assert_eq!(
        expired.commit(ImportCommit {
            preview_id: p.id,
            decisions: vec![]
        }),
        Err(ServiceError::NotFound)
    );
}
#[test]
fn pdf_policy_mock_metadata_and_folder_partial_success_cancel_retry_contract() {
    let id = serde_json::from_str("\"019a5665-18c0-7000-8000-000000000001\"").unwrap();
    let input = FileMetadataCandidate {
        intent: PdfIntent::AttachExisting(id),
        filename: "existing.pdf".into(),
        explicit: None,
        embedded: Some(PaperDraft::titled("Never overwrite")),
    };
    assert_eq!(propose(input.clone()).unwrap(), PdfProposal::AttachOnly(id));
    let mut input = input;
    input.intent = PdfIntent::RegisterNew;
    input.explicit = Some(PaperDraft::titled("Explicit"));
    assert!(
        matches!(propose(input.clone()).unwrap(),PdfProposal::Register{draft,..} if draft.title=="Explicit")
    );
    input.explicit = None;
    assert!(
        matches!(propose(input.clone()).unwrap(),PdfProposal::Register{draft,..} if draft.title=="Never overwrite")
    );
    input.embedded = Some(PaperDraft::titled(""));
    assert!(
        matches!(propose(input).unwrap(),PdfProposal::Register{draft,unresolved} if draft.title=="existing" && !unresolved.is_empty())
    );
    let file = SelectedFile {
        in_selected_scope: true,
        symlink_or_unsafe_ancestor: false,
        regular: true,
        filename: "p.PDF".into(),
        byte_size: 1,
        sha256: Some([1; 32]),
    };
    let mut files = vec![file.clone(); 7];
    files[1].in_selected_scope = false;
    files[2].symlink_or_unsafe_ancestor = true;
    files[3].filename = "p.txt".into();
    files[4].byte_size = 257 * 1024 * 1024;
    files[6].sha256 = Some([2; 32]);
    let mut report = plan_folder(&files, &FolderPolicy::default(), None);
    assert_eq!(
        report
            .files
            .iter()
            .map(|f| f.disposition.clone())
            .collect::<Vec<_>>(),
        vec![
            FileDisposition::Ready,
            FileDisposition::OutsideScope,
            FileDisposition::UnsafeLink,
            FileDisposition::NonPdf,
            FileDisposition::Oversized,
            FileDisposition::DuplicateHash,
            FileDisposition::Ready
        ]
    );
    // Future executor records isolated success/failure without losing the other files.
    report.files[0].disposition = FileDisposition::Imported(AttachmentIdRef(
        serde_json::from_str("\"019a5665-18c0-7000-8000-000000000002\"").unwrap(),
    ));
    report.files[6].disposition = FileDisposition::Failed("synthetic parser error".into());
    assert!(report.files[6].retryable);
    let cancelled = plan_folder(&files, &FolderPolicy::default(), Some(1));
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.completed, 1);
    assert!(cancelled.files[6].retryable);
    assert_eq!(
        plan_folder(&[files[6].clone()], &FolderPolicy::default(), None).files[0].disposition,
        FileDisposition::Ready
    );
}

#[test]
fn key_and_batch_boundaries_and_bibliographic_suggestions_are_honest() {
    let mut d = PaperDraft::titled("界".repeat(40));
    d.bibliography.authors = vec![Contributor {
        given_name: None,
        family_name: Some("界".repeat(40)),
        literal_name: None,
        orcid: None,
        role: "author".into(),
    }];
    let mut used = std::collections::HashSet::new();
    let generated = citation_keys::allocate(&d, &mut used);
    assert!(generated.len() <= 256);
    d.bibliography.citation_key = Some("界".repeat(85));
    let retained = citation_keys::allocate(&d, &mut used);
    let resolved = citation_keys::allocate(&d, &mut used);
    assert_eq!(retained.len(), 255);
    assert!(resolved.len() <= 256);
    assert_ne!(retained, resolved);
    let mut other = d.clone();
    d.bibliography.year = Some(2026);
    other.bibliography.year = Some(2026);
    assert_eq!(
        dedup::classify(&d, &other),
        Some(MatchKind::TitleAuthorYear)
    );
    assert!(!MatchKind::TitleAuthorYear.exact());
    d.external_ids = vec![ExternalId {
        namespace: "arxiv".into(),
        value: "2601.12345v2".into(),
    }];
    other.external_ids = d.external_ids.clone();
    assert_eq!(dedup::classify(&d, &other), Some(MatchKind::ExternalId));
    d.bibliography.doi = Some("10.99/exact".into());
    other.bibliography.doi = d.bibliography.doi.clone();
    assert_eq!(dedup::classify(&d, &other), Some(MatchKind::Doi));
    let mut s = ImportCoordinator::new(Mock { applies: 0 });
    assert!(
        s.preview(request(vec![PaperDraft::titled("Batch"); MAX_BATCH]))
            .is_ok()
    );
    assert!(
        s.preview(request(vec![PaperDraft::titled("Batch"); MAX_BATCH + 1]))
            .is_err()
    );
}

#[test]
fn preview_handles_do_not_alias_after_a_real_process_restart() {
    if std::env::var_os("CM_IMPORT_ID_TEST_CHILD").is_some() {
        let mut service = ImportCoordinator::new(Mock { applies: 0 });
        let preview = service
            .preview(request(vec![PaperDraft::titled("Unapproved child row")]))
            .unwrap();
        println!("CM_IMPORT_PREVIEW_ID={}", preview.id.0);
        return;
    }
    let ids = (0..2)
        .map(|_| {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "preview_handles_do_not_alias_after_a_real_process_restart",
                    "--nocapture",
                ])
                .env("CM_IMPORT_ID_TEST_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            String::from_utf8(result.stdout)
                .unwrap()
                .lines()
                .find_map(|line| {
                    line.strip_prefix("CM_IMPORT_PREVIEW_ID=")
                        .map(str::to_owned)
                })
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_ne!(ids[0], ids[1]);
    let mut service = ImportCoordinator::new(Mock { applies: 0 });
    service
        .preview(request(vec![PaperDraft::titled("Current unapproved row")]))
        .unwrap();
    assert_eq!(
        service.commit(ImportCommit {
            preview_id: PreviewId(ids[0].clone()),
            decisions: vec![ImportDecision {
                candidate_index: 0,
                action: ImportAction::Create
            }]
        }),
        Err(ServiceError::NotFound)
    );
    assert_eq!(service.into_backend().applies, 0);
}

#[test]
fn arxiv_wrappers_and_page_ranges_preserve_literal_elocators() {
    for value in [
        "arXiv:2101.00001v2",
        "arxiv:2101.00001v2",
        "ARXIV:2101.00001v2",
        "http://arxiv.org/abs/2101.00001v2",
        "https://ARXIV.org/abs/2101.00001v2",
        "arxiv.org/abs/2101.00001v2",
    ] {
        assert_eq!(
            external_id(&ExternalId {
                namespace: "ARXIV".into(),
                value: value.into()
            })
            .unwrap()
            .value,
            "2101.00001v2"
        );
    }
    for (raw, expected) in [
        ("123-145", "123–145"),
        ("S1 -- S12", "S1–S12"),
        ("e2020-001", "e2020-001"),
        ("S12-3", "S12-3"),
    ] {
        let mut draft = PaperDraft::titled("Pages");
        draft.bibliography.pages = Some(raw.into());
        assert_eq!(
            normalize(&draft).unwrap().bibliography.pages.as_deref(),
            Some(expected)
        );
    }
}

#[test]
fn reimport_retains_a_key_owned_by_its_single_exact_match() {
    struct Existing(cm_domain::Paper);
    impl ImportBackend for Existing {
        fn snapshot(&mut self, _: &ImportDestination) -> Result<ImportSnapshot, ServiceError> {
            Ok(ImportSnapshot {
                revision: "1".into(),
                papers: vec![self.0.clone()],
            })
        }
        fn apply(&mut self, _: &PreparedImport) -> Result<ImportOutcome, ServiceError> {
            unreachable!()
        }
    }
    let mut draft = PaperDraft::titled("A retained reference");
    draft.bibliography.doi = Some("10.12/self".into());
    draft.bibliography.citation_key = Some("StableLaTeXKey".into());
    let normalized = normalize(&draft).unwrap();
    let paper = cm_domain::Paper {
        id: cm_domain::PaperId(uuid::Uuid::now_v7()),
        title: normalized.title.clone(),
        external_ids: normalized.external_ids.clone(),
        bibliography: normalized.bibliography.clone(),
        created_at: None,
        updated_at: None,
        deleted_at: None,
    };
    let mut service = ImportCoordinator::new(Existing(paper));
    let preview = service.preview(request(vec![draft])).unwrap();
    let row = &preview.candidates[0];
    assert_eq!(
        row.draft.bibliography.citation_key.as_deref(),
        Some("StableLaTeXKey")
    );
    assert!(!row.diagnostics.iter().any(|d| d.field == "citation_key"));
    assert!(
        !row.matches[0]
            .differences
            .iter()
            .any(|d| d.field == "citation_key")
    );
}

#[test]
fn doi_matching_collects_bibliography_and_every_external_doi() {
    let mut existing = PaperDraft::titled("Persisted representation");
    existing.bibliography.doi = Some("10.1002/old%3c493%3e".into());
    existing.external_ids = vec![
        cm_domain::ExternalId {
            namespace: "doi".into(),
            value: "10.1002/old%3c493%3e".into(),
        },
        cm_domain::ExternalId {
            namespace: "DOI".into(),
            value: "https://doi.org/10.1002/old%3C493%3E".into(),
        },
        cm_domain::ExternalId {
            namespace: "doi".into(),
            value: "10.1002/third".into(),
        },
    ];
    for value in ["10.1002/old%3c493%3e", "10.1002/old<493>", "10.1002/third"] {
        let mut incoming = PaperDraft::titled("Incoming representation");
        incoming.bibliography.doi = Some(value.into());
        assert_eq!(dedup::classify(&incoming, &existing), Some(MatchKind::Doi));
        assert_eq!(dedup::classify(&existing, &incoming), Some(MatchKind::Doi));
        assert!(!dedup::doi_disagreement(&incoming, &existing));
    }
    let mut unknown = PaperDraft::titled("Unknown");
    unknown.bibliography.doi = Some("10.1002/unrelated".into());
    assert!(dedup::doi_disagreement(&unknown, &existing));
    // Matching reports identifiers even on inconsistent stored records; it does not
    // grant permission to merge them or turn new inconsistent metadata into valid input.
    assert!(normalize::normalize(&existing).is_err());
}
