//! Search orchestration (#18): DSL compilation, candidate merging across the unicode61 and
//! trigram indexes plus the bounded short-term literal scan, paper-level AND, weighted
//! ranking, pagination and snippets. All SQL lives behind the `SearchIndex` port (storage).
//!
//! Semantics:
//! - Every text clause (term or phrase) must match somewhere in the same paper: in its
//!   bibliographic record, one of its annotations or one of its extracted PDF pages
//!   (paper-level AND). Returned hits are the matching documents of those papers.
//! - A clause with >= 3 Unicode scalars is looked up in both FTS indexes: trigram
//!   (substring, case-insensitive) and unicode61 (token, diacritics folded). Candidates are
//!   merged per document; the clause score is the better weighted bm25 of the two.
//! - A shorter clause is looked up as a unicode61 token plus a bounded literal substring
//!   scan (ASCII case-insensitive, other text exact) over one window of documents.
//!   Within a scan window only documents in that window are returned, so successive
//!   continuation windows never repeat a hit.
//! - No width/NFC/kana normalization is applied (measured constraint, docs/validation/ISSUE_53.md).
use super::{
    query::{self, ClauseKind, Span},
    types::*,
};
use crate::{ServiceError, papers::PaperVisibility};
use cm_domain::{AttachmentId, CollectionId, PaperId, TagId, TagKey};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

/// Typed, resolved paper filter. Every list entry is a required (AND) condition.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedFilter {
    pub visibility: PaperVisibility,
    pub dois: Vec<String>,
    pub tags: Vec<TagId>,
    /// Direct membership in each listed collection.
    pub collections: Vec<CollectionId>,
    /// Inclusive year ranges.
    pub years: Vec<(u16, u16)>,
    pub has_pdf: bool,
    /// Empty = no paper scope.
    pub papers: Vec<PaperId>,
}
impl ResolvedFilter {
    /// An explicit narrowing filter bounds the PDF candidate set for short terms.
    pub fn narrows(&self) -> bool {
        !(self.dois.is_empty()
            && self.tags.is_empty()
            && self.collections.is_empty()
            && self.years.is_empty()
            && self.papers.is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tokenizer {
    /// `unicode61 remove_diacritics 1`, all fields.
    Words,
    /// `trigram`, all fields except identifiers. Needs >= 3 scalars.
    Trigram,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub id: i64,
    pub paper_id: PaperId,
    pub source: DocumentSource,
    /// Weighted bm25, negated so that higher is better (>= 0 for weights >= 0).
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRow {
    pub id: i64,
    pub paper_id: PaperId,
    pub source: DocumentSource,
    /// Per needle: bit set of `SearchField::bit` where the needle occurs.
    pub masks: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanWindow {
    /// Rows with at least one needle match.
    pub rows: Vec<ScanRow>,
    pub examined: u32,
    /// Last document id examined (continuation point).
    pub last_id: Option<i64>,
    /// No further document in scope after `last_id`.
    pub exhausted: bool,
    pub timed_out: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedDocument {
    pub id: i64,
    pub source: DocumentSource,
    pub source_key: String,
    pub paper_id: PaperId,
    pub attachment_id: Option<AttachmentId>,
    pub page_index: Option<u32>,
    pub page_label: Option<String>,
    pub paper_title: String,
    pub doi: Option<String>,
    pub deleted: bool,
    /// Indexed by `SearchField::index`.
    pub fields: [Option<String>; 8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExtractionGaps {
    pub pending: u32,
    pub without_text: u32,
}

/// Storage port. Implementations bind every value as a parameter and quote FTS input as
/// a single phrase; AST text is never executable syntax.
pub trait SearchIndex {
    fn resolve_tag(&self, key: &TagKey) -> Result<Option<TagId>, ServiceError>;
    /// Exact (case-preserving) collection name; several IDs mean the name is ambiguous.
    fn resolve_collection(&self, name: &str) -> Result<Vec<CollectionId>, ServiceError>;
    /// Up to `max` best documents matching `text` as one phrase. Returning more than
    /// `max` rows is not allowed; returning exactly `max` means "possibly truncated".
    fn fts(
        &self,
        tokenizer: Tokenizer,
        text: &str,
        filter: &ResolvedFilter,
        sources: &[DocumentSource],
        weights: &Weights,
        max: u32,
    ) -> Result<Vec<Candidate>, ServiceError>;
    /// Bibliographic documents of papers matching `filter` (filter-only queries).
    fn filtered(&self, filter: &ResolvedFilter, max: u32) -> Result<Vec<Candidate>, ServiceError>;
    /// Examines documents in id order after `after`, at most `max_rows`, stopping early at
    /// `deadline`. Literal substring per needle and field, ASCII case-insensitive.
    fn scan(
        &self,
        needles: &[String],
        filter: &ResolvedFilter,
        sources: &[DocumentSource],
        after: i64,
        max_rows: u32,
        deadline: Instant,
    ) -> Result<ScanWindow, ServiceError>;
    fn documents(&self, ids: &[i64]) -> Result<Vec<IndexedDocument>, ServiceError>;
    /// PDF attachments of papers in `filter` whose page text is not (yet) searchable.
    fn extraction_gaps(&self, filter: &ResolvedFilter) -> Result<ExtractionGaps, ServiceError>;
}

struct Plan {
    texts: Vec<String>,
    filter: ResolvedFilter,
    unresolved: Vec<Unresolved>,
}

fn invalid_reply(errors: query::ParseErrors) -> SearchReply {
    SearchReply::Invalid(QueryErrors {
        diagnostics: errors
            .diagnostics
            .into_iter()
            .map(|d| QueryDiagnostic {
                code: format!("{:?}", d.code),
                message: d.code.message().into(),
                start: d.span.start,
                end: d.span.end,
                corrections: d
                    .corrections
                    .into_iter()
                    .map(|c| QueryCorrection {
                        start: c.span.start,
                        end: c.span.end,
                        replacement: c.replacement.into(),
                    })
                    .collect(),
            })
            .collect(),
        truncated: errors.truncated,
    })
}

fn ambiguous(span: Span) -> QueryDiagnostic {
    QueryDiagnostic {
        code: "AmbiguousCollection".into(),
        message: "several collections have this name; filter by collection ID instead".into(),
        start: span.start,
        end: span.end,
        corrections: Vec::new(),
    }
}

fn plan(
    index: &dyn SearchIndex,
    request: &SearchRequest,
) -> Result<Result<Plan, SearchReply>, ServiceError> {
    let parsed = match query::parse(&request.query) {
        Ok(q) => q,
        Err(e) => return Ok(Err(invalid_reply(e))),
    };
    let mut plan = Plan {
        texts: Vec::new(),
        filter: ResolvedFilter {
            visibility: request.visibility,
            papers: request.papers.clone(),
            collections: request.collection.into_iter().collect(),
            ..Default::default()
        },
        unresolved: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    for clause in parsed.clauses {
        match clause.kind {
            ClauseKind::Term(t) | ClauseKind::Phrase(t) => {
                // Duplicate clauses are redundant AND conditions; execute each text once.
                if !plan.texts.contains(&t) {
                    plan.texts.push(t)
                }
            }
            ClauseKind::Doi(doi) => plan.filter.dois.push(doi),
            ClauseKind::Tag(key) => match index.resolve_tag(&key)? {
                Some(id) => plan.filter.tags.push(id),
                None => plan.unresolved.push(Unresolved::Tag(key.as_str().into())),
            },
            ClauseKind::YearRange { from, to } => plan.filter.years.push((from, to)),
            ClauseKind::Collection(name) => match index.resolve_collection(&name)?.as_slice() {
                [] => plan.unresolved.push(Unresolved::Collection(name)),
                [id] => plan.filter.collections.push(*id),
                _ => diagnostics.push(ambiguous(clause.span)),
            },
            ClauseKind::HasPdf => plan.filter.has_pdf = true,
        }
    }
    if !diagnostics.is_empty() {
        return Ok(Err(SearchReply::Invalid(QueryErrors {
            diagnostics,
            truncated: false,
        })));
    }
    Ok(Ok(plan))
}

#[derive(Clone, Copy)]
struct Meta {
    paper_id: PaperId,
}

/// Executes one search request. `started` anchors the scan time budget.
pub fn search(
    index: &dyn SearchIndex,
    request: &SearchRequest,
) -> Result<SearchReply, ServiceError> {
    let started = Instant::now();
    let weights = request.validate()?;
    let plan = match plan(index, request)? {
        Ok(plan) => plan,
        Err(reply) => return Ok(reply),
    };
    let cursor = request.cursor.unwrap_or_default();
    let mut coverage = SearchCoverage {
        complete: true,
        fts_truncated: false,
        timed_out: false,
        short_terms: Vec::new(),
        scan: ScanStatus::NotNeeded,
        scanned_documents: 0,
        unsearched: Vec::new(),
        continuation: None,
        unresolved: plan.unresolved.clone(),
    };
    if !plan.unresolved.is_empty() {
        // An unknown tag/collection is an unsatisfiable conjunction, reported explicitly.
        return Ok(SearchReply::Results(Box::new(SearchPage {
            hits: Vec::new(),
            next_page: None,
            total_hits: 0,
            coverage,
        })));
    }
    let sources = request.sources.sources();
    let filter = &plan.filter;
    let (long, short): (Vec<&String>, Vec<&String>) = plan
        .texts
        .iter()
        .partition(|t| t.chars().count() >= TRIGRAM_MIN_SCALARS);
    coverage.short_terms = short.iter().map(|s| s.to_string()).collect();

    let mut meta: HashMap<i64, Meta> = HashMap::new();
    // Per clause (plan.texts order): document id -> score.
    let mut clauses: Vec<HashMap<i64, f64>> = vec![HashMap::new(); plan.texts.len()];
    let mut add = |clause: &mut HashMap<i64, f64>, id: i64, paper_id: PaperId, score: f64| {
        meta.entry(id).or_insert(Meta { paper_id });
        let entry = clause.entry(id).or_insert(score);
        *entry = entry.max(score);
    };
    for (i, text) in plan.texts.iter().enumerate() {
        let tokenizers: &[Tokenizer] = if long.contains(&text) {
            &[Tokenizer::Words, Tokenizer::Trigram]
        } else {
            &[Tokenizer::Words]
        };
        for tokenizer in tokenizers {
            let found = index.fts(
                *tokenizer,
                text,
                filter,
                &sources,
                &weights,
                MAX_FTS_CANDIDATES,
            )?;
            coverage.fts_truncated |= found.len() as u32 >= MAX_FTS_CANDIDATES;
            for c in found {
                add(&mut clauses[i], c.id, c.paper_id, c.score.max(0.0));
            }
        }
    }

    // Window bounds for short-term queries: hits are limited to (scan_after, window_end].
    let mut window: Option<(i64, Option<i64>)> = None;
    if !short.is_empty() {
        let pages_allowed =
            request.short_term_scope == ShortTermScope::AllPages || filter.narrows();
        let scan_sources: Vec<DocumentSource> = sources
            .iter()
            .copied()
            .filter(|s| *s != DocumentSource::Page || pages_allowed)
            .collect();
        if request.sources.pages && !pages_allowed {
            coverage
                .unsearched
                .push(UnsearchedScope::PdfPagesForShortTerms);
        }
        let needles: Vec<String> = short.iter().map(|s| s.to_string()).collect();
        let deadline = started + Duration::from_millis(u64::from(request.timeout_ms));
        let scan = if scan_sources.is_empty() {
            ScanWindow {
                exhausted: true,
                ..Default::default()
            }
        } else {
            index.scan(
                &needles,
                filter,
                &scan_sources,
                cursor.scan_after,
                request.candidate_limit,
                deadline,
            )?
        };
        coverage.scanned_documents = scan.examined;
        coverage.timed_out = scan.timed_out;
        for row in &scan.rows {
            for (n, mask) in row.masks.iter().enumerate() {
                if *mask != 0
                    && let Some(i) = plan.texts.iter().position(|t| *t == needles[n])
                {
                    add(&mut clauses[i], row.id, row.paper_id, weights.best(*mask));
                }
            }
        }
        if scan.exhausted && !scan.timed_out {
            coverage.scan = ScanStatus::Complete;
            window = Some((cursor.scan_after, None));
        } else {
            coverage.scan = ScanStatus::Partial;
            let end = scan.last_id.unwrap_or(cursor.scan_after);
            window = Some((cursor.scan_after, Some(end)));
            coverage
                .unsearched
                .push(UnsearchedScope::DocumentsAfterWindow);
            coverage.continuation = Some(SearchCursor {
                scan_after: end,
                offset: 0,
            });
        }
    }

    // Filter-only query: bibliographic documents of matching papers.
    let mut ranked: Vec<(i64, f64)> = if plan.texts.is_empty() {
        let found = index.filtered(filter, MAX_FTS_CANDIDATES)?;
        coverage.fts_truncated |= found.len() as u32 >= MAX_FTS_CANDIDATES;
        found.into_iter().map(|c| (c.id, 0.0)).collect()
    } else {
        let mut papers: Option<HashSet<PaperId>> = None;
        for clause in &clauses {
            let set: HashSet<PaperId> = clause.keys().map(|id| meta[id].paper_id).collect();
            papers = Some(match papers {
                None => set,
                Some(p) => p.intersection(&set).copied().collect(),
            });
        }
        let papers = papers.unwrap_or_default();
        let mut docs: HashMap<i64, f64> = HashMap::new();
        for clause in &clauses {
            for (id, score) in clause {
                if papers.contains(&meta[id].paper_id) {
                    *docs.entry(*id).or_insert(0.0) += *score;
                }
            }
        }
        docs.into_iter().collect()
    };
    if let Some((after, end)) = window {
        ranked.retain(|(id, _)| *id > after && end.is_none_or(|end| *id <= end));
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let total = ranked.len();
    let offset = cursor.offset as usize;
    let page: Vec<(i64, f64)> = ranked
        .iter()
        .skip(offset)
        .take(request.limit as usize)
        .copied()
        .collect();
    let next_page = (offset + page.len() < total).then_some(SearchCursor {
        scan_after: cursor.scan_after,
        offset: (offset + page.len()) as u32,
    });

    let ids: Vec<i64> = page.iter().map(|(id, _)| *id).collect();
    let documents: HashMap<i64, IndexedDocument> = index
        .documents(&ids)?
        .into_iter()
        .map(|d| (d.id, d))
        .collect();
    let needles: Vec<String> = plan.texts.clone();
    let mut hits = Vec::with_capacity(page.len());
    for (id, score) in page {
        // A document deleted between the two reads is skipped rather than invented.
        if let Some(doc) = documents.get(&id) {
            hits.push(hit(
                doc,
                score,
                &needles,
                &weights,
                request.snippet_chars as usize,
            ));
        }
    }

    if request.sources.pages {
        let gaps = index.extraction_gaps(filter)?;
        if gaps.pending > 0 {
            coverage
                .unsearched
                .push(UnsearchedScope::PendingExtraction {
                    attachments: gaps.pending,
                });
        }
        if gaps.without_text > 0 {
            coverage
                .unsearched
                .push(UnsearchedScope::NoExtractableText {
                    attachments: gaps.without_text,
                });
        }
    }
    coverage.complete = !coverage.fts_truncated
        && !coverage.timed_out
        && coverage.scan != ScanStatus::Partial
        && !coverage.unsearched.iter().any(|u| {
            matches!(
                u,
                UnsearchedScope::PdfPagesForShortTerms
                    | UnsearchedScope::DocumentsAfterWindow
                    | UnsearchedScope::PendingExtraction { .. }
            )
        });
    Ok(SearchReply::Results(Box::new(SearchPage {
        hits,
        next_page,
        total_hits: total as u32,
        coverage,
    })))
}

fn hit(
    doc: &IndexedDocument,
    score: f64,
    needles: &[String],
    weights: &Weights,
    snippet_chars: usize,
) -> SearchHit {
    let lowered: Vec<String> = needles.iter().map(|n| n.to_ascii_lowercase()).collect();
    let matched: Vec<SearchField> = SearchField::ALL
        .into_iter()
        .filter(|f| {
            doc.fields[f.index()].as_deref().is_some_and(|text| {
                let text = text.to_ascii_lowercase();
                lowered.iter().any(|n| text.contains(n.as_str()))
            })
        })
        .collect();
    let fallback = match doc.source {
        DocumentSource::Paper => SearchField::Title,
        DocumentSource::Annotation => SearchField::Notes,
        DocumentSource::Page => SearchField::Body,
    };
    let field = matched
        .iter()
        .copied()
        .max_by(|a, b| {
            weights
                .get(*a)
                .total_cmp(&weights.get(*b))
                .then(b.index().cmp(&a.index()))
        })
        .unwrap_or(fallback);
    let text = doc.fields[field.index()].as_deref().unwrap_or("");
    let (snippet, highlights, truncated) = snippet(text, &lowered, snippet_chars);
    SearchHit {
        paper_id: doc.paper_id,
        paper_title: doc.paper_title.clone(),
        doi: doc.doi.clone(),
        deleted: doc.deleted,
        source: doc.source,
        attachment_id: doc.attachment_id,
        annotation_id: (doc.source == DocumentSource::Annotation).then(|| doc.source_key.clone()),
        page_index: doc.page_index,
        page_label: doc.page_label.clone(),
        matched_fields: matched,
        snippet_field: (!text.is_empty()).then_some(field),
        snippet,
        highlights,
        snippet_truncated: truncated,
        score,
    }
}

const MAX_HIGHLIGHTS: usize = 32;
const ELLIPSIS: char = '\u{2026}';

/// Bounded excerpt around the first literal (ASCII case-insensitive) occurrence of any
/// needle. Control characters become spaces. Highlights are byte ranges in the result.
pub fn snippet(
    text: &str,
    lowered_needles: &[String],
    max_chars: usize,
) -> (String, Vec<SnippetRange>, bool) {
    let haystack = text.to_ascii_lowercase();
    let first = lowered_needles
        .iter()
        .filter(|n| !n.is_empty())
        .filter_map(|n| haystack.find(n.as_str()))
        .min()
        .unwrap_or(0);
    let char_starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let total = char_starts.len();
    let first_char = char_starts.partition_point(|b| *b < first);
    let start_char = if total <= max_chars {
        0
    } else {
        first_char
            .saturating_sub(max_chars / 3)
            .min(total - max_chars)
    };
    let end_char = (start_char + max_chars).min(total);
    let byte = |c: usize| char_starts.get(c).copied().unwrap_or(text.len());
    let mut out = String::new();
    if start_char > 0 {
        out.push(ELLIPSIS);
    }
    out.extend(
        text[byte(start_char)..byte(end_char)]
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c }),
    );
    let body_start = if start_char > 0 {
        ELLIPSIS.len_utf8()
    } else {
        0
    };
    if end_char < total {
        out.push(ELLIPSIS);
    }
    let lowered = out.to_ascii_lowercase();
    let mut ranges: Vec<SnippetRange> = Vec::new();
    for needle in lowered_needles.iter().filter(|n| !n.is_empty()) {
        let mut from = body_start;
        while let Some(found) = lowered[from..].find(needle.as_str()) {
            let start = from + found;
            ranges.push(SnippetRange {
                start,
                end: start + needle.len(),
            });
            from = start + needle.len();
            if ranges.len() > 4 * MAX_HIGHLIGHTS {
                break;
            }
        }
    }
    ranges.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<SnippetRange> = Vec::new();
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged.truncate(MAX_HIGHLIGHTS);
    (out, merged, start_char > 0 || end_char < total)
}
