//! Public-format expectations authored independently of the production serializers.
use cm_core::{ServiceError, export::BibliographySerializer, import::*, ports::ImportService};
use cm_metadata::{
    bibtex::{self, BibtexSerializer},
    exchange::{MAX_DEPTH, MAX_RECORD_BYTES, ParseReport},
    ris::{self, RisSerializer},
};
use cm_protocol::domain::{Paper, PaperId};
use cm_storage::LibraryDb;
use serde_json::{Value, json};

type Parser = fn(&[u8]) -> Result<ParseReport, ServiceError>;
const BIB: &[u8] = include_bytes!("../../../fixtures/bibtex/external-public.bib");
const RIS: &[u8] = include_bytes!("../../../fixtures/ris/external-public.ris");
fn paper(d: &PaperDraft) -> Paper {
    Paper {
        id: PaperId(Default::default()),
        title: d.title.clone(),
        bibliography: d.bibliography.clone(),
        external_ids: d.external_ids.clone(),
        created_at: None,
        updated_at: None,
        deleted_at: None,
    }
}
fn public(text: &str) -> String {
    text.lines()
        .filter(|s| !s.trim_start().starts_with("cm_data = ") && !s.starts_with("C9  - cm-v1:"))
        .map(|s| format!("{s}\n"))
        .collect()
}
#[test]
fn standard_month_tokens_survive_public_bibtex_and_have_specific_ris_loss() {
    for month in [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ] {
        let input = format!(
            "@article{{month,title={{Month}},month={}}}",
            month.to_uppercase()
        );
        let parsed = bibtex::parse(input.as_bytes()).unwrap();
        assert_eq!(parsed.candidates.len(), 1);
        let d = &parsed.candidates[0];
        assert!(!d.diagnostics.iter().any(|d| d.error));
        let bib = BibtexSerializer.encode(&paper(d)).unwrap();
        let native = public(&bib.text);
        assert!(native.contains(&format!("month = {month},")), "{native}");
        let again = bibtex::parse(native.as_bytes()).unwrap();
        assert_eq!(
            again.candidates[0]
                .bibliography
                .extra_json
                .as_ref()
                .unwrap()["bibtex"][0],
            json!(["expression:month", month])
        );
        assert!(!again.candidates[0].diagnostics.iter().any(|d| d.error));
        let ris = RisSerializer.encode(&paper(d)).unwrap();
        assert!(
            ris.diagnostics
                .iter()
                .any(|d| d.field == "month" && d.message.contains(month))
        );
    }
}

#[test]
fn ambiguous_multiline_recovery_marks_all_later_candidates_non_creatable() {
    let input = b"@article{before,title={Before}}\n@article{real,title={Real},abstract={line one\n@article{fake,title={Injected},doi={10.9/evil}}\n@book{fake2,title={Also injected}}\nrest}\n@misc{after,title={After}}\n";
    let parsed = bibtex::parse(input).unwrap();
    assert_eq!(
        parsed
            .candidates
            .iter()
            .map(|d| d.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Before", "Injected", "Also injected", "After"]
    );
    assert!(!parsed.candidates[0].diagnostics.iter().any(|d| d.error));
    for (draft, line) in parsed.candidates[1..].iter().zip([3, 4, 6]) {
        assert_eq!(draft.source.row, Some(line));
        assert!(
            draft
                .diagnostics
                .iter()
                .any(|d| d.error && d.field == "syntax" && d.message.contains("line 2"))
        );
    }
    let mut db = LibraryDb::in_memory().unwrap();
    let mut service = ImportCoordinator::new(db.imports());
    let preview = service
        .preview(parsed.batch(0, 4, Default::default()).unwrap())
        .unwrap();
    let decisions = |skip: bool| ImportCommit {
        preview_id: preview.id.clone(),
        decisions: (0..4)
            .map(|i| ImportDecision {
                candidate_index: i,
                action: if skip && i > 0 {
                    ImportAction::Skip
                } else {
                    ImportAction::Create
                },
            })
            .collect(),
    };
    assert!(service.commit(decisions(false)).is_err());
    let outcome = service.commit(decisions(true)).unwrap();
    assert_eq!(outcome.created.len(), 1);
    assert_eq!(outcome.skipped, 3);
}

#[test]
fn public_text_caret_and_tilde_use_literal_tex_commands_and_decode_without_extension() {
    let mut d = PaperDraft::titled("Ca^2+ and 50~nm");
    d.bibliography.abstract_text = Some("literal ^ and ~".into());
    let encoded = BibtexSerializer.encode(&paper(&d)).unwrap();
    let native = public(&encoded.text);
    assert!(native.contains("title = {Ca\\textasciicircum{}2+ and 50\\textasciitilde{}nm}"));
    assert!(!native.contains('^') && !native.contains('~'));
    let parsed = bibtex::parse(native.as_bytes()).unwrap();
    assert_eq!(parsed.candidates[0].title, d.title);
    assert_eq!(
        parsed.candidates[0].bibliography.abstract_text,
        d.bibliography.abstract_text
    );
    assert!(
        !parsed.candidates[0]
            .diagnostics
            .iter()
            .any(|d| d.message.contains("Unknown") || d.error)
    );
}

#[test]
fn ris_protected_person_parts_and_whole_corporate_group_use_distinct_names() {
    let parsed = ris::parse(b"TY  - JOUR\nTI  - Names\nAU  - {Smith}, {John}\nAU  - {Barnes and Noble}\nED  - {Editor}, {Ann}\nER  -\n").unwrap();
    let names = &parsed.candidates[0].bibliography.authors;
    assert_eq!(names[0].family_name.as_deref(), Some("Smith"));
    assert_eq!(names[0].given_name.as_deref(), Some("John"));
    assert!(names[0].literal_name.is_none());
    assert_eq!(names[1].literal_name.as_deref(), Some("Barnes and Noble"));
    assert_eq!(names[2].family_name.as_deref(), Some("Editor"));
    assert_eq!(names[2].given_name.as_deref(), Some("Ann"));
    assert_eq!(names[2].role, "editor");
    for serializer in [
        &BibtexSerializer as &dyn BibliographySerializer,
        &RisSerializer,
    ] {
        let encoded = serializer.encode(&paper(&parsed.candidates[0])).unwrap();
        assert!(public(&encoded.text).contains("Smith, John"));
        assert!(public(&encoded.text).contains("{Barnes and Noble}"));
    }
}
fn projection(d: &PaperDraft) -> Value {
    let b = &d.bibliography;
    json!({"key": b.citation_key, "title": d.title, "type": b.paper_type, "year": b.year,
        "journal": b.journal, "doi": b.doi, "url": b.url,
        "authors": b.authors.iter().map(|a| json!([a.given_name,a.family_name,a.literal_name,a.role])).collect::<Vec<_>>()})
}
#[test]
fn independent_exporter_shaped_fixtures_and_public_same_cross_format_expectations() {
    let expected: Vec<Value> = serde_json::from_str(include_str!(
        "../../../fixtures/external-public.expected.json"
    ))
    .unwrap();
    for (source, parse) in [(BIB, bibtex::parse as Parser), (RIS, ris::parse as Parser)] {
        let parsed = parse(source).unwrap();
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(parsed.candidates.len(), 2);
        for (d, expected) in parsed.candidates.iter().zip(&expected) {
            assert_eq!(&projection(d), expected);
            assert!(!d.diagnostics.iter().any(|d| d.error));
            for (serializer, parse) in [
                (
                    &BibtexSerializer as &dyn BibliographySerializer,
                    bibtex::parse as Parser,
                ),
                (&RisSerializer, ris::parse as Parser),
            ] {
                let output = serializer.encode(&paper(d)).unwrap();
                let text = public(&output.text);
                assert!(!text.contains("cm-v1"));
                assert!(!text.contains("cm_data"));
                for name in expected["authors"].as_array().unwrap() {
                    let native = if let Some(organization) = name[2].as_str() {
                        format!("{{{organization}}}")
                    } else {
                        format!(
                            "{}, {}",
                            name[1].as_str().unwrap(),
                            name[0].as_str().unwrap()
                        )
                    };
                    assert!(text.contains(&native), "{text}");
                }

                let again = parse(text.as_bytes()).unwrap();
                assert_eq!(again.candidates.len(), 1, "{text}");
                assert_eq!(&projection(&again.candidates[0]), expected, "{text}");
                assert!(!again.candidates[0].diagnostics.iter().any(|d| d.error));
            }
        }
    }
}
#[test]
fn identifiers_and_concatenations_retain_usable_bibliography_without_evaluation() {
    let report = bibtex::parse(include_bytes!("../../../fixtures/bibtex/expressions.bib")).unwrap();
    assert_eq!(report.candidates.len(), 4);
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].line, 1);
    let month = &report.candidates[0];
    assert_eq!(month.title, "Usable month record");
    assert_eq!(
        month.bibliography.authors[0].given_name.as_deref(),
        Some("John")
    );
    assert_eq!(month.bibliography.doi.as_deref(), Some("10.9999/month"));
    assert!(!month.diagnostics.iter().any(|d| d.error));
    for (d, field, raw) in [
        (month, "month", "jan"),
        (&report.candidates[1], "journal", "nat"),
        (&report.candidates[2], "journal", "unknownjournal"),
        (
            &report.candidates[3],
            "title",
            "{One} # \" two\" # undefined",
        ),
    ] {
        assert!(
            d.bibliography.extra_json.as_ref().unwrap()["bibtex"]
                .as_array()
                .unwrap()
                .contains(&json!([format!("expression:{field}"), raw]))
        );
    }
    assert!(report.candidates[1].bibliography.journal.is_none());
    assert!(report.candidates[2].bibliography.journal.is_none());
    assert!(report.candidates[3].title.is_empty());
    assert_eq!(report.candidates[3].bibliography.authors.len(), 1);
    assert_eq!(
        report.candidates[3].bibliography.doi.as_deref(),
        Some("10.9999/concat")
    );
    let mut db = LibraryDb::in_memory().unwrap();
    let mut service = ImportCoordinator::new(db.imports());
    let preview = service
        .preview(report.batch(0, 4, Default::default()).unwrap())
        .unwrap();
    assert!(
        preview.candidates[1..]
            .iter()
            .all(|c| c.diagnostics.iter().any(|d| d.error))
    );
    let commit = |all_create| ImportCommit {
        preview_id: preview.id.clone(),
        decisions: (0..4)
            .map(|candidate_index| ImportDecision {
                candidate_index,
                action: if all_create || candidate_index == 0 {
                    ImportAction::Create
                } else {
                    ImportAction::Skip
                },
            })
            .collect(),
    };
    assert!(service.commit(commit(true)).is_err());
    let result = service.commit(commit(false)).unwrap();
    assert_eq!(result.created.len(), 1);
    assert_eq!(result.skipped, 3);
}
#[test]
fn finite_accents_protected_braced_unbraced_and_special_letters_are_unicode() {
    for (command, base, expected) in [
        ("\"", "o", "ö"),
        ("'", "e", "é"),
        ("`", "a", "à"),
        ("^", "o", "ô"),
        ("~", "n", "ñ"),
        ("c", "c", "ç"),
        ("v", "r", "ř"),
        ("u", "g", "ğ"),
        ("H", "o", "ő"),
        ("k", "a", "ą"),
        ("=", "a", "ā"),
        (".", "z", "ż"),
    ] {
        for raw in [
            format!("\\{command}{{{base}}}"),
            format!("{{\\{command}{{{base}}}}}"),
            if command.chars().all(|c| c.is_ascii_alphabetic()) {
                format!("\\{command} {base}")
            } else {
                format!("\\{command}{base}")
            },
        ] {
            let input = format!("@misc{{x,title={{{raw}}}}}");
            let report = bibtex::parse(input.as_bytes()).unwrap();
            assert_eq!(report.candidates[0].title, expected, "{raw}");
            assert!(
                !report.candidates[0]
                    .diagnostics
                    .iter()
                    .any(|d| d.field == "tex")
            );
        }
    }
    let input = br#"@misc{x,title={\ss{} {\o} \aa{} \l{} \O{} \AA{} \L{} \'{\i}},author={J{\"{o}}rg Schr{\"{o}}der and {Smith}, {John} and {Research and Development Council}}}"#;
    let report = bibtex::parse(input).unwrap();
    assert_eq!(report.candidates[0].title, "ß ø å ł Ø Å Ł í");
    let names = &report.candidates[0].bibliography.authors;
    assert_eq!(names[0].given_name.as_deref(), Some("Jörg"));
    assert_eq!(names[0].family_name.as_deref(), Some("Schröder"));
    assert_eq!(names[1].given_name.as_deref(), Some("John"));
    assert_eq!(names[1].family_name.as_deref(), Some("Smith"));
    assert!(names[1].literal_name.is_none());
    assert_eq!(
        names[2].literal_name.as_deref(),
        Some("Research and Development Council")
    );
}
#[test]
fn unknown_commands_raw_names_and_literal_braces_backslash_survive() {
    let report = bibtex::parse(br#"@misc{x,title={Literal \{brace\} \textbackslash{}path \unknown{arg}},author={\unknown{Smith}, John}}"#).unwrap();
    let d = &report.candidates[0];
    assert_eq!(d.title, r"Literal {brace} \path \unknown{arg}");
    assert_eq!(
        d.bibliography.authors[0].family_name.as_deref(),
        Some(r"\unknown{Smith}")
    );
    assert!(d.diagnostics.iter().any(|d| d.field == "tex"));
    let raw = d.bibliography.extra_json.as_ref().unwrap()["bibtex"]
        .as_array()
        .unwrap();
    assert!(raw.contains(&json!(["raw:author", r"\unknown{Smith}, John"])));
    assert!(raw.contains(&json!([
        "raw:title",
        r"Literal \{brace\} \textbackslash{}path \unknown{arg}"
    ])));
}
fn recovered(input: &str, parse: Parser, after_line: u64) {
    let report = parse(input.as_bytes()).unwrap();
    assert_eq!(
        report
            .candidates
            .iter()
            .map(|d| d.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Before", "After"]
    );
    assert_eq!(report.candidates[0].source.row, Some(1));
    assert_eq!(report.candidates[1].source.row, Some(after_line));
    assert_eq!(report.diagnostics.len(), 1, "{:?}", report.diagnostics);
    assert!(report.diagnostics[0].error);
    assert!(
        report.diagnostics[0].message.contains("budget")
            || report.diagnostics[0].message.contains("limit")
    );
}
#[test]
fn record_local_limits_skip_only_bad_record_in_both_formats() {
    let bib_bad = [
        format!(
            "@article{{bad,title={{Bad}},author={{{}}}}}",
            (0..300)
                .map(|n| format!("Family{n}, Given"))
                .collect::<Vec<_>>()
                .join(" and ")
        ),
        format!(
            "@article{{bad,title={{Bad}},abstract={{{}}}}}",
            "x".repeat(MAX_RECORD_BYTES)
        ),
        format!(
            "@article{{bad,title={{Bad}},{}}}",
            (0..256)
                .map(|n| format!("field{n}={{x}},"))
                .collect::<String>()
        ),
        format!(
            "@article{{bad,title={{{}x{}}}}}",
            "{".repeat(MAX_DEPTH + 1),
            "}".repeat(MAX_DEPTH + 1)
        ),
        // Input fits 64 KiB, but contributor DTO overhead exceeds the unchanged draft limit.
        format!(
            "@article{{bad,title={{Bad}},author={{{}}}}}",
            (0..256)
                .map(|n| format!("Family{n}, {}", "g".repeat(170)))
                .collect::<Vec<_>>()
                .join(" and ")
        ),
    ];
    for bad in bib_bad {
        recovered(
            &format!("@misc{{before,title={{Before}}}}\n{bad}\n@misc{{after,title={{After}}}}\n"),
            bibtex::parse,
            3,
        );
    }
    let ris_bad = [
        format!(
            "TY  - JOUR\nTI  - Bad\n{}ER  -\n",
            "AU  - Family, Given\n".repeat(300)
        ),
        format!(
            "TY  - JOUR\nTI  - Bad\nAB  - {}\nER  -\n",
            "x".repeat(MAX_RECORD_BYTES)
        ),
        format!("TY  - JOUR\nTI  - Bad\n{}ER  -\n", "ZZ  - x\n".repeat(256)),
        format!(
            "TY  - JOUR\nTI  - Bad\nAB  - start\n  {}\nER  -\n",
            "x".repeat(MAX_RECORD_BYTES)
        ),
    ];
    for bad in ris_bad {
        let input = format!("TY  - GEN\nTI  - Before\nER  -\n{bad}TY  - GEN\nTI  - After\nER  -\n");
        recovered(&input, ris::parse, 4 + bad.lines().count() as u64);
    }
}
#[test]
fn deeply_nested_extensions_skip_only_bad_record_without_recursive_json_work() {
    // Deliberately invalid extension data: nesting must be rejected before schema/hash work.
    for levels in [MAX_DEPTH + 1, 150] {
        let json = format!("{}0{}", "[".repeat(levels), "]".repeat(levels));
        let hex: String = json.bytes().map(|v| format!("{v:02x}")).collect();
        recovered(
            &format!(
                "@misc{{b,title={{Before}}}}\n@misc{{x,title={{Bad}},cm_data={{{hex}}}}}\n@misc{{a,title={{After}}}}\n"
            ),
            bibtex::parse,
            3,
        );
        recovered(
            &format!(
                "TY  - GEN\nTI  - Before\nER  -\nTY  - GEN\nTI  - Bad\nC9  - cm-v1:{hex}\nER  -\nTY  - GEN\nTI  - After\nER  -\n"
            ),
            ris::parse,
            8,
        );
    }
}
#[test]
fn public_verbatim_fields_and_literal_at_have_independent_native_expectations() {
    let mut d = PaperDraft::titled("Email @ example\n@ line boundary");
    d.bibliography.doi = Some("10.9999/abc_def%20a&b#c".into());
    d.bibliography.url = Some("https://example.invalid/a_b%20c?x=1&y=2#frag".into());
    let encoded = BibtexSerializer.encode(&paper(&d)).unwrap();
    let text = public(&encoded.text);
    assert!(text.contains("doi = {10.9999/abc_def%20a&b#c}"));
    assert!(text.contains("url = {https://example.invalid/a_b%20c?x=1&y=2#frag}"));
    assert!(text.contains("title = {Email {@} example\n{@} line boundary}"));
    assert!(!text.contains(r"\@"));
    assert!(!text.contains(r"\_"));
    assert!(!text.contains(r"\%"));
    assert_eq!(
        bibtex::parse(text.as_bytes()).unwrap().candidates[0].title,
        d.title
    );
    d.bibliography.url = Some("https://example.invalid/{balanced}".into());
    let text = public(&BibtexSerializer.encode(&paper(&d)).unwrap().text);
    assert!(text.contains("url = {https://example.invalid/{balanced}}"));
    assert_eq!(
        bibtex::parse(text.as_bytes()).unwrap().candidates[0]
            .bibliography
            .url,
        d.bibliography.url
    );
    for value in [
        "https://example.invalid/}",
        "https://example.invalid/{",
        "https://example.invalid/\\path",
        "https://example.invalid/\nnext",
        "https://example.invalid/\rnext",
    ] {
        for field in ["doi", "url"] {
            let mut d = PaperDraft::titled("Unsafe verbatim");
            if field == "doi" {
                d.bibliography.doi = Some(value.into());
            } else {
                d.bibliography.url = Some(value.into());
            }
            assert!(
                BibtexSerializer.encode(&paper(&d)).is_err(),
                "{field}: {value:?}"
            );
        }
    }
}
#[test]
fn date_year_conflicts_are_review_only_in_both_orders() {
    for fields in ["year=2020,date={2019-01-01}", "date={2019-01-01},year=2020"] {
        let report =
            bibtex::parse(format!("@article{{x,title={{Conflict}},{fields}}}").as_bytes()).unwrap();
        let d = &report.candidates[0];
        assert_eq!(d.bibliography.year, Some(2020));
        assert_eq!(
            d.bibliography.publication_date.as_deref(),
            Some("2019-01-01")
        );
        assert!(d.diagnostics.iter().any(|d| d.error && d.field == "year"));
        let mut db = LibraryDb::in_memory().unwrap();
        let mut service = ImportCoordinator::new(db.imports());
        let p = service
            .preview(report.batch(0, 1, Default::default()).unwrap())
            .unwrap();
        assert!(p.candidates[0].diagnostics.iter().any(|d| d.error));
        let commit = |action| ImportCommit {
            preview_id: p.id.clone(),
            decisions: vec![ImportDecision {
                candidate_index: 0,
                action,
            }],
        };
        assert!(service.commit(commit(ImportAction::Create)).is_err());
        assert_eq!(
            service.commit(commit(ImportAction::Skip)).unwrap().skipped,
            1
        );
    }
}
#[test]
fn public_ris_type_conversions_have_specific_loss_diagnostics() {
    for (original, native, result) in [
        ("mastersthesis", "THES", "phdthesis"),
        ("inbook", "CHAP", "incollection"),
        ("manual", "GEN", "misc"),
        ("phdthesis", "THES", "phdthesis"),
        ("incollection", "CHAP", "incollection"),
        ("misc", "GEN", "misc"),
    ] {
        let mut d = PaperDraft::titled("Types");
        d.bibliography.paper_type = Some(original.into());
        let encoded = RisSerializer.encode(&paper(&d)).unwrap();
        assert!(public(&encoded.text).contains(&format!("TY  - {native}\n")));
        let lossy = encoded.diagnostics.iter().find(|d| d.field == "paper_type");
        assert_eq!(lossy.is_some(), original != result);
        if let Some(lossy) = lossy {
            assert!(lossy.message.contains(original) && lossy.message.contains(result));
        }
        assert_eq!(
            ris::parse(public(&encoded.text).as_bytes())
                .unwrap()
                .candidates[0]
                .bibliography
                .paper_type
                .as_deref(),
            Some(result)
        );
        assert_eq!(
            ris::parse(encoded.text.as_bytes()).unwrap().candidates[0]
                .bibliography
                .paper_type
                .as_deref(),
            Some(original)
        );
    }
}
#[test]
fn protection_loss_is_per_field_on_import_and_both_public_exports() {
    let literal =
        bibtex::parse(br"@misc{x,title={Literal \textbackslash{}path \{brace\} {@}}}").unwrap();
    assert_eq!(literal.candidates[0].title, r"Literal \path {brace} @");
    assert!(
        !literal.candidates[0]
            .diagnostics
            .iter()
            .any(|d| d.message.contains("case protection"))
    );

    let report =
        bibtex::parse(br"@article{x,title={A {DNA} study},journal={The {ACM} Journal}}").unwrap();
    let d = &report.candidates[0];
    assert_eq!(d.title, "A DNA study");
    for field in ["title", "journal"] {
        assert!(
            d.diagnostics
                .iter()
                .any(|d| d.field == field && d.message.contains("case protection"))
        );
        assert!(
            d.bibliography.extra_json.as_ref().unwrap()["bibtex"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v[0] == format!("protection:{field}"))
        );
    }
    for serializer in [
        &BibtexSerializer as &dyn BibliographySerializer,
        &RisSerializer,
    ] {
        let out = serializer.encode(&paper(d)).unwrap();
        for field in ["title", "journal"] {
            assert!(
                out.diagnostics
                    .iter()
                    .any(|d| d.field == field && d.message.contains("case protection"))
            );
        }
    }
}
#[test]
fn total_draft_budget_remains_file_fatal_after_local_recovery() {
    let record = format!(
        "@misc{{x,title={{x}},abstract={{{} {{protected}} \\unknown{{arg}}}}}}\n",
        "x".repeat(15_000)
    );
    let input = record.repeat(750);
    assert!(input.len() < cm_metadata::exchange::MAX_INPUT_BYTES);
    let error = match bibtex::parse(input.as_bytes()) {
        Err(error) => error.to_string(),
        Ok(report) => panic!(
            "Expected file budget rejection, got {} candidates",
            report.candidates.len()
        ),
    };
    assert!(error.contains("file draft budget"), "{error}");
}

#[test]
fn fixed_long_author_and_malformed_neighbor_fixtures_recover_normal_records() {
    for (input, parse, starts, rejected) in [
        (
            include_bytes!("../../../fixtures/bibtex/recovery.bib").as_slice(),
            bibtex::parse as Parser,
            [1, 3, 5],
            [2, 4],
        ),
        (
            include_bytes!("../../../fixtures/ris/recovery.ris").as_slice(),
            ris::parse as Parser,
            [1, 307, 312],
            [4, 310],
        ),
    ] {
        let report = parse(input).unwrap();
        assert_eq!(
            report
                .candidates
                .iter()
                .map(|d| d.title.as_str())
                .collect::<Vec<_>>(),
            ["Before", "After", "Last"]
        );
        assert_eq!(
            report
                .candidates
                .iter()
                .map(|d| d.source.row.unwrap())
                .collect::<Vec<_>>(),
            starts
        );
        assert_eq!(
            report
                .diagnostics
                .iter()
                .map(|d| d.line)
                .collect::<Vec<_>>(),
            rejected
        );
        assert!(report.diagnostics.iter().all(|d| d.error));
    }
}
