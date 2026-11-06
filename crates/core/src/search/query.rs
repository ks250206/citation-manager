//! Bounded, flat AND-only DSL; see docs/ADR/search-dsl.md for the exact grammar.
//!
//! AST text is untrusted data, never executable SQL/FTS. Future adapters must resolve
//! names and compile typed clauses with their own validation and parameter binding.
use crate::papers::normalize_doi;
use cm_domain::{TagKey, organization_name};

pub const MAX_INPUT_BYTES: usize = 8192;
pub const MAX_TERMS: usize = 64;
/// Both the encoded value (including quotes/escapes) and decoded value are bounded.
pub const MAX_VALUE_BYTES: usize = 1024;
pub const MAX_DIAGNOSTICS: usize = 16;
pub const MAX_CORRECTIONS: usize = 3;
const FIELD_EXAMPLES: [(&str, &str); 5] = [
    ("doi", "DOI:10.1234/example"),
    ("tag", "tag:anode"),
    ("year", "year:2020..2026"),
    ("collection", "collection:reading"),
    ("has", "has:pdf"),
];

/// Half-open UTF-8 byte offsets into the original input, always at char boundaries.
/// `start` is the error position; zero-width spans can point at EOF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// Successful nonempty parse. All clauses are conjunctive; source order and
/// duplicates are preserved, including repeated field conditions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub clauses: Vec<Clause>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    /// Entire original clause, including its field key and quotes if any.
    pub span: Span,
    pub kind: ClauseKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClauseKind {
    Term(String),
    Phrase(String),
    /// Canonicalized by the existing papers::normalize_doi.
    Doi(String),
    Tag(TagKey),
    /// Inclusive, four-digit years in 0001..=9999, from <= to.
    YearRange {
        from: u16,
        to: u16,
    },
    /// Shared organization display-name rules; unresolved, case-preserving name.
    Collection(String),
    HasPdf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticCode {
    InputTooLong,
    EmptyInput,
    TooManyTerms,
    ValueTooLong,
    UnknownField,
    EmptyValue,
    InvalidQuote,
    UnclosedQuote,
    InvalidEscape,
    ControlCharacter,
    UnsupportedSyntax,
    InvalidDoi,
    InvalidTag,
    InvalidCollection,
    InvalidYearRange,
    InvalidHas,
}

impl DiagnosticCode {
    pub fn message(self) -> &'static str {
        match self {
            Self::InputTooLong => "query exceeds 8192 UTF-8 bytes",
            Self::EmptyInput => "enter at least one search condition",
            Self::TooManyTerms => "query exceeds 64 conditions",
            Self::ValueTooLong => "value exceeds 1024 encoded or decoded UTF-8 bytes",
            Self::UnknownField => "unknown field; use doi, tag, year, collection, or has",
            Self::EmptyValue => "value must contain a non-whitespace character",
            Self::InvalidQuote => "double quotes must enclose the entire value",
            Self::UnclosedQuote => "quoted value needs a closing double quote",
            Self::InvalidEscape => {
                "only escaped double quotes and backslashes inside quotes are supported"
            }
            Self::ControlCharacter => "non-whitespace control characters are not allowed",
            Self::UnsupportedSyntax => {
                "Uppercase Boolean operators, grouping, wildcards, and boosts are not supported; quote literal text"
            }
            Self::InvalidDoi => "invalid DOI; use the existing DOI format",
            Self::InvalidTag => "tag must satisfy the shared TagKey rules",
            Self::InvalidCollection => "collection must satisfy the shared display-name rules",
            Self::InvalidYearRange => "use inclusive YYYY..YYYY in 0001..9999 with start <= end",
            Self::InvalidHas => "the only supported has value is pdf",
        }
    }
}

/// An optional example edit, never an automatic repair or a claim of user intent.
/// Replaces `span` in the original input. Static replacements cannot echo input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    pub span: Span,
    pub replacement: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub span: Span,
    pub corrections: Vec<Correction>,
}

/// No partial AST is exposed. If truncated, remaining input was not examined:
/// the input-byte, term-count, or diagnostic-count budget stopped parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseErrors {
    pub diagnostics: Vec<Diagnostic>,
    pub truncated: bool,
}

fn diagnostic(code: DiagnosticCode, span: Span, clause: Span) -> Diagnostic {
    use DiagnosticCode::*;
    let examples: &[&str] = match code {
        UnknownField => &["tag:anode", "year:2020..2026", "has:pdf"],
        EmptyInput | EmptyValue => &["lithium"],
        InvalidQuote | UnclosedQuote | InvalidEscape | UnsupportedSyntax => &["\"literal text\""],
        InvalidDoi => &["DOI:10.1234/example"],
        InvalidTag => &["tag:anode"],
        InvalidCollection => &["collection:reading"],
        InvalidYearRange => &["year:2020..2026"],
        InvalidHas => &["has:pdf"],
        InputTooLong | TooManyTerms | ValueTooLong | ControlCharacter => &[],
    };
    Diagnostic {
        code,
        span,
        corrections: examples
            .iter()
            .take(MAX_CORRECTIONS)
            .map(|replacement| Correction {
                span: clause,
                replacement,
            })
            .collect(),
    }
}

/// Parse without I/O or recursion. Counts invalid clauses toward the term budget.
/// Oversized input is rejected before scanning or copying any of its contents.
pub fn parse(input: &str) -> Result<Query, ParseErrors> {
    if input.len() > MAX_INPUT_BYTES {
        let mut end = MAX_INPUT_BYTES;
        while !input.is_char_boundary(end) {
            end -= 1;
        }
        let span = Span { start: end, end };
        return Err(ParseErrors {
            diagnostics: vec![diagnostic(DiagnosticCode::InputTooLong, span, span)],
            truncated: true,
        });
    }
    let mut clauses = Vec::new();
    let mut diagnostics = Vec::new();
    let mut position = 0;
    let mut count = 0;
    let mut truncated = false;
    loop {
        position += input[position..].len() - input[position..].trim_start().len();
        if position == input.len() {
            break;
        }
        if count == MAX_TERMS {
            let span = Span {
                start: position,
                end: position,
            };
            diagnostics.push(diagnostic(DiagnosticCode::TooManyTerms, span, span));
            truncated = true;
            break;
        }
        let end = clause_end(input, position);
        let span = Span {
            start: position,
            end,
        };
        match parse_clause(&input[position..end], span) {
            Ok(kind) => clauses.push(Clause { span, kind }),
            Err(error) => diagnostics.push(error),
        }
        position = end;
        count += 1;
        if diagnostics.len() == MAX_DIAGNOSTICS {
            truncated = !input[position..].trim().is_empty();
            break;
        }
    }
    if count == 0 {
        let span = Span {
            start: input.len(),
            end: input.len(),
        };
        diagnostics.push(diagnostic(DiagnosticCode::EmptyInput, span, span));
    }
    if diagnostics.is_empty() {
        Ok(Query { clauses })
    } else {
        Err(ParseErrors {
            diagnostics,
            truncated,
        })
    }
}

// Quotes keep whitespace inside one clause, including malformed embedded quotes.
// Recovery is at the next whitespace outside quotes; an open quote consumes to EOF.
fn clause_end(input: &str, start: usize) -> usize {
    let mut quoted = false;
    let mut escaped = false;
    for (offset, ch) in input[start..].char_indices() {
        if !quoted && ch.is_whitespace() {
            return start + offset;
        }
        if quoted && escaped {
            escaped = false;
        } else if quoted && ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        }
    }
    input.len()
}

fn parse_clause(raw: &str, span: Span) -> Result<ClauseKind, Diagnostic> {
    let (key, raw_value, offset) = if raw.starts_with('"') {
        (None, raw, span.start)
    } else if let Some((key, value)) = raw.split_once(':') {
        if !FIELD_EXAMPLES
            .iter()
            .any(|(known, _)| key.eq_ignore_ascii_case(known))
        {
            return Err(diagnostic(
                DiagnosticCode::UnknownField,
                Span {
                    start: span.start,
                    end: span.start + key.len(),
                },
                span,
            ));
        }
        (Some(key), value, span.start + key.len() + 1)
    } else {
        (None, raw, span.start)
    };
    let value_span = Span {
        start: offset,
        end: span.end,
    };
    let (value, quoted) = decode_value(raw_value, value_span, span).map_err(|mut error| {
        if !error.corrections.is_empty()
            && let Some(replacement) = key.and_then(|key| {
                FIELD_EXAMPLES
                    .iter()
                    .find(|(known, _)| key.eq_ignore_ascii_case(known))
                    .map(|(_, example)| *example)
            })
        {
            error.corrections = vec![Correction { span, replacement }];
        }
        error
    })?;
    let invalid = |code| diagnostic(code, value_span, span);
    let Some(key) = key else {
        if !quoted
            && (matches!(value.as_str(), "OR" | "AND" | "NOT" | "NEAR")
                || value.starts_with(['+', '-'])
                || value.contains(['(', ')', '[', ']', '{', '}', '*', '^', '|', '&', '!']))
        {
            return Err(invalid(DiagnosticCode::UnsupportedSyntax));
        }
        return Ok(if quoted {
            ClauseKind::Phrase(value)
        } else {
            ClauseKind::Term(value)
        });
    };
    if key.eq_ignore_ascii_case("doi") {
        normalize_doi(&value)
            .map(ClauseKind::Doi)
            .map_err(|_| invalid(DiagnosticCode::InvalidDoi))
    } else if key.eq_ignore_ascii_case("tag") {
        TagKey::new(&value)
            .map(ClauseKind::Tag)
            .map_err(|_| invalid(DiagnosticCode::InvalidTag))
    } else if key.eq_ignore_ascii_case("collection") {
        organization_name(&value)
            .map(ClauseKind::Collection)
            .map_err(|_| invalid(DiagnosticCode::InvalidCollection))
    } else if key.eq_ignore_ascii_case("year") {
        year_range(&value).ok_or_else(|| invalid(DiagnosticCode::InvalidYearRange))
    } else if value.eq_ignore_ascii_case("pdf") {
        Ok(ClauseKind::HasPdf)
    } else {
        Err(invalid(DiagnosticCode::InvalidHas))
    }
}

fn decode_value(raw: &str, span: Span, clause: Span) -> Result<(String, bool), Diagnostic> {
    let invalid = |code, start, end| diagnostic(code, Span { start, end }, clause);
    if raw.len() > MAX_VALUE_BYTES {
        return Err(invalid(DiagnosticCode::ValueTooLong, span.start, span.end));
    }
    let quoted = raw.starts_with('"');
    let mut value = String::new();
    let mut chars = raw.char_indices();
    if quoted {
        chars.next();
    }
    let mut closed = !quoted;
    while let Some((offset, ch)) = chars.next() {
        let start = span.start + offset;
        if ch.is_control() && !ch.is_whitespace() {
            return Err(invalid(
                DiagnosticCode::ControlCharacter,
                start,
                start + ch.len_utf8(),
            ));
        }
        if ch == '"' {
            if !quoted || offset + 1 != raw.len() {
                return Err(invalid(DiagnosticCode::InvalidQuote, start, start + 1));
            }
            closed = true;
            break;
        }
        if ch == '\\' {
            if !quoted {
                return Err(invalid(DiagnosticCode::InvalidEscape, start, start + 1));
            }
            match chars.next() {
                Some((_, escaped @ ('"' | '\\'))) => value.push(escaped),
                Some((next, escaped)) => {
                    return Err(invalid(
                        DiagnosticCode::InvalidEscape,
                        start,
                        span.start + next + escaped.len_utf8(),
                    ));
                }
                None => return Err(invalid(DiagnosticCode::InvalidEscape, start, start + 1)),
            }
        } else {
            value.push(ch);
        }
    }
    if !closed {
        return Err(invalid(DiagnosticCode::UnclosedQuote, span.end, span.end));
    }
    if value.trim().is_empty() {
        return Err(invalid(DiagnosticCode::EmptyValue, span.start, span.end));
    }
    if value.len() > MAX_VALUE_BYTES {
        return Err(invalid(DiagnosticCode::ValueTooLong, span.start, span.end));
    }
    Ok((value, quoted))
}

fn year_range(value: &str) -> Option<ClauseKind> {
    let (from, to) = value.split_once("..")?;
    let year = |s: &str| {
        if s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse::<u16>().ok().filter(|year| *year != 0)
        } else {
            None
        }
    };
    let (from, to) = (year(from)?, year(to)?);
    (from <= to).then_some(ClauseKind::YearRange { from, to })
}
