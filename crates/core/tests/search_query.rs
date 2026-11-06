use cm_core::search::query::{self, *};
use cm_domain::TagKey;

fn kinds(input: &str) -> Vec<ClauseKind> {
    parse(input)
        .unwrap()
        .clauses
        .into_iter()
        .map(|c| c.kind)
        .collect()
}

fn error(input: &str, code: DiagnosticCode) -> ParseErrors {
    let errors = parse(input).unwrap_err();
    assert_eq!(errors.diagnostics[0].code, code, "input: {input:?}");
    errors
}

#[test]
fn every_design_example_has_the_expected_typed_meaning() {
    assert_eq!(
        kinds("lithium magnesium interface"),
        vec![
            ClauseKind::Term("lithium".into()),
            ClauseKind::Term("magnesium".into()),
            ClauseKind::Term("interface".into()),
        ]
    );
    assert_eq!(
        kinds("\"solid-state battery\" tag:anode"),
        vec![
            ClauseKind::Phrase("solid-state battery".into()),
            ClauseKind::Tag(TagKey::new("anode").unwrap()),
        ]
    );
    assert_eq!(
        kinds("DOI:10.1234/example"),
        vec![ClauseKind::Doi("10.1234/example".into())]
    );
    assert_eq!(
        kinds("year:2020..2026 collection:reading"),
        vec![
            ClauseKind::YearRange {
                from: 2020,
                to: 2026
            },
            ClauseKind::Collection("reading".into()),
        ]
    );
    assert_eq!(
        kinds("has:pdf tag:important"),
        vec![
            ClauseKind::HasPdf,
            ClauseKind::Tag(TagKey::new("important").unwrap()),
        ]
    );
}

#[test]
fn unicode_whitespace_separates_clauses_and_spans_reference_original_text() {
    let input = "\u{3000}電池\t\"固体　電解質\"\r\nTaG:重要\u{a0}HAS:PDF  ";
    let parsed = parse(input).unwrap();
    assert_eq!(
        parsed
            .clauses
            .iter()
            .map(|c| &input[c.span.start..c.span.end])
            .collect::<Vec<_>>(),
        vec!["電池", "\"固体　電解質\"", "TaG:重要", "HAS:PDF"]
    );
    assert_eq!(
        kinds(input),
        vec![
            ClauseKind::Term("電池".into()),
            ClauseKind::Phrase("固体　電解質".into()),
            ClauseKind::Tag(TagKey::new("重要").unwrap()),
            ClauseKind::HasPdf,
        ]
    );
    assert_eq!(
        kinds("Battery battery"),
        vec![
            ClauseKind::Term("Battery".into()),
            ClauseKind::Term("battery".into())
        ]
    );
    assert_eq!(
        kinds("a\u{200b}b"),
        vec![ClauseKind::Term("a\u{200b}b".into())]
    );
}

#[test]
fn field_values_reuse_shared_semantics_without_collapsing_distinct_keys() {
    let parsed = kinds("tAg:\"  İ　ANODE\t重要  \" CoLlEcTiOn:\" Reading　List \" has:\"pDf\"");
    match &parsed[0] {
        ClauseKind::Tag(key) => assert_eq!(key.as_str(), "i\u{307} anode 重要"),
        _ => panic!("expected tag"),
    }
    assert_eq!(parsed[1], ClauseKind::Collection("Reading List".into()));
    assert_eq!(parsed[2], ClauseKind::HasPdf);
    for (left, right) in [("é", "e\u{301}"), ("Ａ", "A"), ("ß", "ss"), ("ς", "σ")] {
        assert_ne!(
            kinds(&format!("tag:{left}")),
            kinds(&format!("tag:{right}"))
        );
    }
    assert_eq!(
        kinds("collection:Reading collection:reading"),
        vec![
            ClauseKind::Collection("Reading".into()),
            ClauseKind::Collection("reading".into()),
        ]
    );
}

#[test]
fn doi_wrappers_decoding_and_percent_literals_match_existing_identity_policy() {
    for input in [
        "DOI:10.1234/ABC",
        "doi:doi:10.1234/ABC",
        "doi:https://doi.org/10.1234/%41BC?ignored=yes#fragment",
    ] {
        assert_eq!(kinds(input), vec![ClauseKind::Doi("10.1234/abc".into())]);
    }
    assert_eq!(
        kinds("doi:10.1234/%41BC"),
        vec![ClauseKind::Doi("10.1234/%41bc".into())]
    );
    assert_eq!(
        kinds("doi:https://doi.org/10.1234/%2541BC"),
        vec![ClauseKind::Doi("10.1234/%41bc".into())]
    );
    assert_eq!(
        kinds("doi:10.12.34/A(B)"),
        vec![ClauseKind::Doi("10.12.34/a(b)".into())]
    );
    for input in [
        "doi:abc",
        "doi:10.x/a",
        "doi:10.1234/",
        "doi:\"10.1234/a b\"",
        "doi:https://doi.org/10.1234/%",
        "doi:https://doi.org/10.1234/%FF",
        "doi:https://doi.org/10.1234%2Fa",
    ] {
        error(input, DiagnosticCode::InvalidDoi);
    }
}

#[test]
fn quotes_preserve_text_and_only_two_escapes_decode() {
    assert_eq!(
        kinds(r#""say \"battery\" at C:\\papers""#),
        vec![ClauseKind::Phrase("say \"battery\" at C:\\papers".into())]
    );
    assert_eq!(
        kinds("\" a\t\nb \""),
        vec![ClauseKind::Phrase(" a\t\nb ".into())]
    );
    assert_eq!(
        kinds(r#"tag:"a\"b\\c""#),
        vec![ClauseKind::Tag(TagKey::new("a\"b\\c").unwrap())]
    );
    for input in [
        r#""a\nb""#,
        r#""a\tb""#,
        r#""a\u0041""#,
        r#""a\日""#,
        r"papers\file",
        "\"abc\\",
    ] {
        error(input, DiagnosticCode::InvalidEscape);
    }
    for input in ["\"", "\"電池", "tag:\"電池", "\"a\\\""] {
        let errors = error(input, DiagnosticCode::UnclosedQuote);
        assert_eq!(
            errors.diagnostics[0].span,
            Span {
                start: input.len(),
                end: input.len()
            }
        );
        assert!(!errors.truncated);
    }
    for input in ["ab\"cd\"", "\"a\"b", "tag:\"a\"b", "\"a\"\"b\""] {
        error(input, DiagnosticCode::InvalidQuote);
    }
}

#[test]
fn repeated_conditions_remain_ordered_conjunctions_without_last_value_wins() {
    let parsed = kinds(
        "tag:A tag:a year:2020..2021 year:2025..2026 collection:X collection:Y has:pdf has:pdf doi:10.1/a doi:10.1/b a a",
    );
    assert_eq!(parsed.len(), 12);
    assert_eq!(parsed[0], parsed[1]);
    assert_eq!(
        parsed[2],
        ClauseKind::YearRange {
            from: 2020,
            to: 2021
        }
    );
    assert_eq!(
        parsed[3],
        ClauseKind::YearRange {
            from: 2025,
            to: 2026
        }
    );
    assert_eq!(parsed[4], ClauseKind::Collection("X".into()));
    assert_eq!(parsed[5], ClauseKind::Collection("Y".into()));
    assert_eq!(parsed[6], parsed[7]);
    assert_ne!(parsed[8], parsed[9]);
    assert_eq!(parsed[10], parsed[11]);
}

#[test]
fn years_are_closed_inclusive_four_digit_positive_ranges() {
    assert_eq!(
        kinds("year:0001..9999 year:\"2026..2026\""),
        vec![
            ClauseKind::YearRange { from: 1, to: 9999 },
            ClauseKind::YearRange {
                from: 2026,
                to: 2026
            },
        ]
    );
    for value in [
        "2026",
        "2026..2020",
        "..2026",
        "2020..",
        "0000..2026",
        "2020..10000",
        "20..26",
        "+020..2026",
        "２０２０..２０２６",
        "2020...2026",
        "2020..2026..2027",
        "2020-2026",
        "2020 .. 2026",
        " 2020..2026 ",
    ] {
        error(
            &format!("year:\"{value}\""),
            DiagnosticCode::InvalidYearRange,
        );
    }
}

#[test]
fn empty_values_unknown_keys_and_unsupported_has_are_explicit_errors() {
    for input in ["", " \t\r\n\u{a0}\u{3000}"] {
        let errors = error(input, DiagnosticCode::EmptyInput);
        assert_eq!(
            errors.diagnostics[0].span,
            Span {
                start: input.len(),
                end: input.len()
            }
        );
    }
    for input in [
        "\"\"",
        "\" \t　\"",
        "tag:",
        "doi:",
        "year:",
        "has:",
        "collection:\"　\"",
    ] {
        error(input, DiagnosticCode::EmptyValue);
    }
    for input in [
        "tags:anode",
        "author:Jones",
        "ＤＯＩ:10.1/a",
        ":anode",
        "tag :anode",
        "https://example.com",
        r"C:\papers",
    ] {
        let errors = parse(input).unwrap_err();
        assert!(
            errors
                .diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::UnknownField)
        );
    }
    error("tag: anode", DiagnosticCode::EmptyValue);
    for input in ["has:false", "has:epub", "has:\" pdf \""] {
        error(input, DiagnosticCode::InvalidHas);
    }
}

#[test]
fn boolean_fts_and_group_syntax_are_rejected_and_quoted_literals_are_data() {
    for input in [
        "a OR b",
        "a or b",
        "a AND b",
        "NOT a",
        "NEAR",
        "NEAR(a,b)",
        "(a)",
        "[a]",
        "{a}",
        "a*",
        "a^2",
        "a|b",
        "a&b",
        "!a",
        "+a",
        "-a",
    ] {
        error(input, DiagnosticCode::UnsupportedSyntax);
    }
    assert_eq!(
        kinds("\"OR\" \"a*\" \"(a)\""),
        vec![
            ClauseKind::Phrase("OR".into()),
            ClauseKind::Phrase("a*".into()),
            ClauseKind::Phrase("(a)".into()),
        ]
    );
    // These are plain AST text, with no SQL/FTS compilation or database access.
    assert_eq!(
        kinds("\"'; DROP TABLE papers; --\""),
        vec![ClauseKind::Phrase("'; DROP TABLE papers; --".into())]
    );
    assert_eq!(
        kinds("tag:a* collection:OR"),
        vec![
            ClauseKind::Tag(TagKey::new("a*").unwrap()),
            ClauseKind::Collection("OR".into())
        ]
    );
    assert_eq!(
        kinds("solid-state O'Brien 10%"),
        vec![
            ClauseKind::Term("solid-state".into()),
            ClauseKind::Term("O'Brien".into()),
            ClauseKind::Term("10%".into())
        ]
    );
}

#[test]
fn diagnostics_point_to_safe_original_unicode_positions_and_recover_at_clauses() {
    let input = "電池 未知:値 year:2026..2020 \"a\\日\" has:no";
    let errors = parse(input).unwrap_err();
    assert_eq!(
        errors
            .diagnostics
            .iter()
            .map(|d| d.code)
            .collect::<Vec<_>>(),
        vec![
            DiagnosticCode::UnknownField,
            DiagnosticCode::InvalidYearRange,
            DiagnosticCode::InvalidEscape,
            DiagnosticCode::InvalidHas,
        ]
    );
    assert_eq!(
        errors
            .diagnostics
            .iter()
            .map(|d| &input[d.span.start..d.span.end])
            .collect::<Vec<_>>(),
        vec!["未知", "2026..2020", "\\日", "no"]
    );
    assert_eq!(errors.diagnostics[0].span.start, 7); // UTF-8 bytes, not scalar index 3.
    assert!(!errors.truncated);
    for input in ["a\0b", "tag:\"a\u{7f}b\"", "\"a\u{1b}b\""] {
        error(input, DiagnosticCode::ControlCharacter);
    }
}

#[test]
fn correction_examples_are_bounded_valid_edits_of_the_entire_offending_clause() {
    for bad in [
        "tagg:電池",
        "year:2026..2020",
        "has:epub",
        "tag:",
        "tag:\"\"",
        "doi:no",
        "\"unfinished",
        "\"bad\\x\"",
        "ab\"cd\"",
        "a*",
    ] {
        let input = format!("電池 {bad}");
        let errors = parse(&input).unwrap_err();
        for diagnostic in errors.diagnostics {
            assert!(!diagnostic.code.message().is_empty());
            assert!(!diagnostic.corrections.is_empty());
            assert!(diagnostic.corrections.len() <= MAX_CORRECTIONS);
            for correction in diagnostic.corrections {
                assert_eq!(&input[correction.span.start..correction.span.end], bad);
                let mut corrected = input.clone();
                corrected.replace_range(
                    correction.span.start..correction.span.end,
                    correction.replacement,
                );
                assert!(parse(&corrected).is_ok(), "{corrected:?}");
            }
        }
    }
    let errors = error("", DiagnosticCode::EmptyInput);
    assert!(parse(errors.diagnostics[0].corrections[0].replacement).is_ok());
}

#[test]
fn raw_value_and_unicode_name_boundaries_are_enforced_without_truncation() {
    assert_eq!(
        kinds(&"a".repeat(1024)),
        vec![ClauseKind::Term("a".repeat(1024))]
    );
    error(&"a".repeat(1025), DiagnosticCode::ValueTooLong);
    assert_eq!(
        kinds(&"🧪".repeat(256)),
        vec![ClauseKind::Term("🧪".repeat(256))]
    );
    error(&"🧪".repeat(257), DiagnosticCode::ValueTooLong);
    assert!(parse(&format!("\"{}\"", "a".repeat(1022))).is_ok());
    error(
        &format!("\"{}\"", "a".repeat(1023)),
        DiagnosticCode::ValueTooLong,
    );
    assert!(parse(&format!("tag:{}", "a".repeat(256))).is_ok());
    error(
        &format!("tag:{}", "a".repeat(257)),
        DiagnosticCode::InvalidTag,
    );
    assert!(parse(&format!("collection:{}", "a".repeat(256))).is_ok());
    error(
        &format!("collection:{}", "a".repeat(257)),
        DiagnosticCode::InvalidCollection,
    );
    assert!(parse(&format!("tag:{}", "İ".repeat(128))).is_ok());
    error(
        &format!("tag:{}", "İ".repeat(129)),
        DiagnosticCode::InvalidTag,
    );
}

#[test]
fn whole_input_limit_rejects_before_parsing_and_never_splits_unicode() {
    let mut values = vec!["a".repeat(1024); 7];
    values.push("b".repeat(1017));
    let input = values.join(" ");
    assert_eq!(input.len(), 8192);
    assert_eq!(parse(&input).unwrap().clauses.len(), 8);
    let errors = error(&(input + "x"), DiagnosticCode::InputTooLong);
    assert!(errors.truncated);
    assert_eq!(errors.diagnostics.len(), 1);
    assert!(errors.diagnostics[0].corrections.is_empty());
    let errors = error(&("a".repeat(8191) + "日"), DiagnosticCode::InputTooLong);
    assert_eq!(
        errors.diagnostics[0].span,
        Span {
            start: 8191,
            end: 8191
        }
    );
    error(&"🧪".repeat(262_144), DiagnosticCode::InputTooLong);
}

#[test]
fn term_and_diagnostic_budgets_include_invalid_clauses_and_report_unexamined_input() {
    let input = vec!["a"; 64].join(" ");
    assert_eq!(parse(&input).unwrap().clauses.len(), 64);
    let errors = error(&(input + " b"), DiagnosticCode::TooManyTerms);
    assert!(errors.truncated);
    assert_eq!(
        errors.diagnostics[0].span,
        Span {
            start: 128,
            end: 128
        }
    );
    let input = format!("has:no {}", vec!["a"; 63].join(" "));
    let errors = parse(&(input + " b")).unwrap_err();
    assert_eq!(
        errors
            .diagnostics
            .iter()
            .map(|d| d.code)
            .collect::<Vec<_>>(),
        vec![DiagnosticCode::InvalidHas, DiagnosticCode::TooManyTerms]
    );
    assert!(errors.truncated);
    for (count, truncated) in [(15, false), (16, false), (17, true), (64, true)] {
        let errors = parse(&vec!["has:no"; count].join(" ")).unwrap_err();
        assert_eq!(errors.diagnostics.len(), count.min(16));
        assert_eq!(errors.truncated, truncated);
    }
    // No invalid clause or suffix can be silently dropped into a successful AST.
    assert!(parse("lithium has:no magnesium").is_err());
}

fn assert_span(input: &str, span: Span) {
    assert!(span.start <= span.end && span.end <= input.len());
    assert!(input.is_char_boundary(span.start) && input.is_char_boundary(span.end));
}

fn assert_bounded(input: &str) {
    let result = parse(input);
    assert_eq!(result, parse(input), "determinism for {input:?}");
    match result {
        Ok(query) => {
            assert!(!query.clauses.is_empty() && query.clauses.len() <= MAX_TERMS);
            for clause in query.clauses {
                assert_span(input, clause.span);
                match clause.kind {
                    ClauseKind::Term(v)
                    | ClauseKind::Phrase(v)
                    | ClauseKind::Doi(v)
                    | ClauseKind::Collection(v) => {
                        assert!(!v.trim().is_empty() && v.len() <= MAX_VALUE_BYTES);
                    }
                    ClauseKind::Tag(key) => assert!(key.as_str().len() <= MAX_VALUE_BYTES),
                    ClauseKind::YearRange { from, to } => {
                        assert!(from >= 1 && from <= to && to <= 9999)
                    }
                    ClauseKind::HasPdf => (),
                }
            }
        }
        Err(errors) => {
            assert!(!errors.diagnostics.is_empty() && errors.diagnostics.len() <= MAX_DIAGNOSTICS);
            for diagnostic in errors.diagnostics {
                assert_span(input, diagnostic.span);
                assert!(diagnostic.corrections.len() <= MAX_CORRECTIONS);
                for correction in diagnostic.corrections {
                    assert_span(input, correction.span);
                    assert!(correction.replacement.len() <= MAX_VALUE_BYTES);
                    assert!(query::parse(correction.replacement).is_ok());
                }
            }
        }
    }
}

#[test]
fn bounded_malformed_corpus_is_reproducible_and_utf8_safe() {
    // Exhaustive lengths 0..=4 over 12 scalars: 22,621 cases, streamed without recursion.
    let alphabet = [
        'a', '日', '🧪', ' ', '\u{a0}', ':', '"', '\\', '.', '-', '\0', '*',
    ];
    for length in 0..=4_u32 {
        for mut index in 0..alphabet.len().pow(length) {
            let mut input = String::new();
            for _ in 0..length {
                input.push(alphabet[index % alphabet.len()]);
                index /= alphabet.len();
            }
            assert_bounded(&input);
        }
    }
    // Fixed xorshift64 seed, 2,048 longer combinations with 0..511 scalars each.
    let mut state = 0x0004_6d51_2026_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for index in 0..2048 {
        let length = next() % 512;
        let body: String = (0..length)
            .map(|_| alphabet[(next() % 12) as usize])
            .collect();
        let prefix = ["", "tag:", "year:", "doi:", "collection:", "has:", "\""][index % 7];
        let input = format!("{prefix}{body}");
        assert_bounded(&input);
    }
}
