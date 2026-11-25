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
//!   scan (ASCII case-insensitive, other text exact) over one window of documents. A
//!   window with no match after `candidate_limit` documents extends until one matches
//!   (bounded, see `ScanBudget`), so a run of non-matching records does not end it empty.
//!   Within a scan window only documents in that window are returned, so successive
//!   continuation windows never repeat a hit. The paper-level AND still spans windows: a
//!   paper with a document in the window is also checked for the short clauses in all of
//!   its other documents (same sources, same deadline).
//! - No width/NFC/kana normalization is applied (measured constraint, docs/validation/ISSUE_53.md).
//! - `SearchRequest::timeout_ms` bounds the whole request: the port enforces it inside the
//!   database (`SearchIndex::set_deadline`). An interrupted index lookup yields an explicit
//!   timed-out page; the short-term scan stops at its window with a continuation.
use super::{
    fold::{fold, fold_needle},
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

/// How far one short-term scan window may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanBudget {
    /// Documents examined per window (`SearchRequest::candidate_limit`).
    pub rows: u32,
    /// A window that has found no match after `rows` documents keeps going, chunk by
    /// chunk, until a chunk matches, `empty_rows` documents (>= `rows`) are examined or
    /// `empty_until` passes. Without it a run of non-matching documents — typically a
    /// collection's bibliographic records, whose ids precede its extracted PDF pages —
    /// ends the window empty although the next documents match (#30). Stopping at
    /// `empty_rows` or `empty_until` ends a normal window (continuation, not `timed_out`).
    pub empty_rows: u32,
    pub empty_until: Instant,
}
impl ScanBudget {
    /// Exactly `rows` documents (no extension of an empty window).
    pub fn fixed(rows: u32) -> Self {
        Self {
            rows,
            empty_rows: rows,
            empty_until: Instant::now(),
        }
    }
    /// Base window of `rows`; an empty window extends up to `MAX_CANDIDATE_LIMIT`
    /// documents and at most half of the time left until `deadline`, so the rest of the
    /// request (paper-level AND checks, the result fetch) keeps its share of the budget.
    pub fn extending(rows: u32, deadline: Instant) -> Self {
        let now = Instant::now();
        Self {
            rows,
            empty_rows: rows.max(MAX_CANDIDATE_LIMIT),
            empty_until: now + deadline.saturating_duration_since(now) / 2,
        }
    }
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
    /// Examines documents in id order after `after` within `budget` (`budget.rows`, or
    /// more while nothing has matched yet), stopping early at `deadline` (`timed_out`).
    /// Literal substring per needle and field, ASCII case-insensitive.
    fn scan(
        &self,
        needles: &[String],
        filter: &ResolvedFilter,
        sources: &[DocumentSource],
        after: i64,
        budget: ScanBudget,
        deadline: Instant,
    ) -> Result<ScanWindow, ServiceError>;
    fn documents(&self, ids: &[i64]) -> Result<Vec<IndexedDocument>, ServiceError>;
    /// PDF attachments of papers in `filter` whose page text is not (yet) searchable.
    fn extraction_gaps(&self, filter: &ResolvedFilter) -> Result<ExtractionGaps, ServiceError>;
    /// Hard deadline for the following calls (`None` clears it). An implementation that can
    /// interrupt running statements stops them when it passes and then reports
    /// `interrupted()`; `scan` instead ends its window early with `timed_out`.
    fn set_deadline(&self, _deadline: Option<Instant>) {}
    /// The last failing call was stopped by the `set_deadline` deadline.
    fn interrupted(&self) -> bool {
        false
    }
}

/// Extra time after the request budget for fetching the hit documents (at most `MAX_LIMIT`,
/// by primary key) and the extraction coverage counts, so a scan that used up the budget
/// still returns its partial window.
pub const RESULT_GRACE: Duration = Duration::from_millis(500);

/// Reply for a request whose deadline interrupted an index lookup before any result could
/// be ranked: no hits, `timed_out`, not complete. Nothing is claimed as matched or unmatched.
fn timed_out_reply() -> SearchReply {
    SearchReply::Results(Box::new(SearchPage {
        hits: Vec::new(),
        next_page: None,
        total_hits: 0,
        coverage: SearchCoverage {
            complete: false,
            fts_truncated: false,
            timed_out: true,
            short_terms: Vec::new(),
            scan: ScanStatus::NotNeeded,
            scanned_documents: 0,
            unsearched: Vec::new(),
            continuation: None,
            unresolved: Vec::new(),
        },
    }))
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
    // An explicit ID scope (collection or papers) is itself a bounded condition: with a
    // blank DSL it lists that scope's papers. A blank DSL without a scope still fails
    // (EmptyInput) rather than selecting every record.
    let scoped = request.collection.is_some() || !request.papers.is_empty();
    let parsed = if scoped
        && request.query.len() <= query::MAX_INPUT_BYTES
        && request.query.trim().is_empty()
    {
        query::Query {
            clauses: Vec::new(),
        }
    } else {
        match query::parse(&request.query) {
            Ok(q) => q,
            Err(e) => return Ok(Err(invalid_reply(e))),
        }
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

/// Executes one search request within `request.timeout_ms` (plus `RESULT_GRACE` for the
/// final document fetch).
pub fn search(
    index: &dyn SearchIndex,
    request: &SearchRequest,
) -> Result<SearchReply, ServiceError> {
    let started = Instant::now();
    let weights = request.validate()?;
    let deadline = started + Duration::from_millis(u64::from(request.timeout_ms));
    index.set_deadline(Some(deadline));
    let result = execute(index, request, weights, deadline);
    index.set_deadline(None);
    match result {
        Err(_) if index.interrupted() => Ok(timed_out_reply()),
        other => other,
    }
}

fn execute(
    index: &dyn SearchIndex,
    request: &SearchRequest,
    weights: Weights,
    deadline: Instant,
) -> Result<SearchReply, ServiceError> {
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
    fn add(
        meta: &mut HashMap<i64, Meta>,
        clause: &mut HashMap<i64, f64>,
        id: i64,
        paper_id: PaperId,
        score: f64,
    ) {
        meta.entry(id).or_insert(Meta { paper_id });
        let entry = clause.entry(id).or_insert(score);
        *entry = entry.max(score);
    }
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
                add(
                    &mut meta,
                    &mut clauses[i],
                    c.id,
                    c.paper_id,
                    c.score.max(0.0),
                );
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
                ScanBudget::extending(request.candidate_limit, deadline),
                deadline,
            )?
        };
        coverage.scanned_documents = scan.examined;
        coverage.timed_out = scan.timed_out;
        let record =
            |meta: &mut HashMap<i64, Meta>, clauses: &mut [HashMap<i64, f64>], rows: &[ScanRow]| {
                for row in rows {
                    for (n, mask) in row.masks.iter().enumerate() {
                        if *mask != 0
                            && let Some(i) = plan.texts.iter().position(|t| *t == needles[n])
                        {
                            add(
                                meta,
                                &mut clauses[i],
                                row.id,
                                row.paper_id,
                                weights.best(*mask),
                            );
                        }
                    }
                }
            };
        record(&mut meta, &mut clauses, &scan.rows);
        let full = cursor.scan_after == 0 && scan.exhausted && !scan.timed_out;
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
        // Paper-level AND across windows: when this window did not cover every document, a
        // paper with a document in it may match a short clause only in a document of another
        // window. Those clauses are looked up in all documents of such papers, so the paper's
        // in-window documents are still returned here (and only here).
        if !full && !scan_sources.is_empty() {
            let (after, end) = window.expect("set above");
            let in_window = |id: i64| id > after && end.is_none_or(|end| id <= end);
            let is_short: Vec<bool> = plan.texts.iter().map(|t| short.contains(&t)).collect();
            let clause_papers: Vec<HashSet<PaperId>> = clauses
                .iter()
                .map(|c| c.keys().map(|id| meta[id].paper_id).collect())
                .collect();
            let mut candidates: Vec<PaperId> = clauses
                .iter()
                .flat_map(|c| c.keys())
                .filter(|id| in_window(**id))
                .map(|id| meta[id].paper_id)
                .collect::<HashSet<_>>()
                .into_iter()
                .filter(|p| {
                    let has = |i: usize| clause_papers[i].contains(p);
                    (0..is_short.len()).all(|i| is_short[i] || has(i))
                        && (0..is_short.len()).any(|i| is_short[i] && !has(i))
                })
                .collect();
            candidates.sort_unstable_by_key(|p| p.0);
            'papers: for chunk in candidates.chunks(MAX_SCOPE_PAPERS) {
                let scoped = ResolvedFilter {
                    papers: chunk.to_vec(),
                    ..filter.clone()
                };
                let mut from = 0;
                loop {
                    let found = index.scan(
                        &needles,
                        &scoped,
                        &scan_sources,
                        from,
                        ScanBudget::fixed(MAX_CANDIDATE_LIMIT),
                        deadline,
                    )?;
                    record(&mut meta, &mut clauses, &found.rows);
                    if found.timed_out {
                        // Papers not checked in time are not returned; the page says so.
                        coverage.timed_out = true;
                        break 'papers;
                    }
                    match found.last_id {
                        Some(last) if !found.exhausted => from = last,
                        _ => break,
                    }
                }
            }
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
    index.set_deadline(Some(Instant::now().max(deadline) + RESULT_GRACE));
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
    let folded: Vec<String> = needles.iter().map(|n| fold_needle(n)).collect();
    let matched: Vec<SearchField> = SearchField::ALL
        .into_iter()
        .filter(|f| {
            doc.fields[f.index()].as_deref().is_some_and(|text| {
                // Fast literal check first; then the unicode61-like folded comparison
                // (phrases across line breaks, case and Latin diacritics).
                let ascii = text.to_ascii_lowercase();
                lowered.iter().any(|n| ascii.contains(n.as_str())) || {
                    let text = fold(text).text;
                    folded
                        .iter()
                        .any(|n| !n.is_empty() && text.contains(n.as_str()))
                }
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
    let (snippet, highlights, truncated) = snippet(text, needles, snippet_chars);
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

/// Bounded excerpt around the first occurrence of any needle (query term or phrase),
/// located like `matched_fields`: case-insensitive, Latin diacritics folded and any run of
/// whitespace/line breaks matching one space. Control characters become spaces. Highlights
/// are byte ranges in the result covering the original matched text.
pub fn snippet(
    text: &str,
    needles: &[String],
    max_chars: usize,
) -> (String, Vec<SnippetRange>, bool) {
    let folded: Vec<String> = needles.iter().map(|n| fold_needle(n)).collect();
    let mut matches = fold(text).find_all(&folded, 4 * MAX_HIGHLIGHTS);
    matches.sort_unstable();
    let first = matches.first().map_or(0, |m| m.0);
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
    let (from, to) = (byte(start_char), byte(end_char));
    let mut out = String::new();
    if start_char > 0 {
        out.push(ELLIPSIS);
    }
    // (original byte offset, output byte offset) of every excerpt character, plus its end.
    let mut positions: Vec<(usize, usize)> = Vec::with_capacity(end_char - start_char + 1);
    for (at, c) in text[from..to].char_indices() {
        positions.push((from + at, out.len()));
        out.push(if c.is_control() { ' ' } else { c });
    }
    positions.push((to, out.len()));
    if end_char < total {
        out.push(ELLIPSIS);
    }
    let to_out = |original: usize| {
        let i = positions.partition_point(|p| p.0 < original);
        positions[i.min(positions.len() - 1)].1
    };
    let mut ranges: Vec<SnippetRange> = matches
        .iter()
        .filter(|(start, end)| *end > from && *start < to)
        .map(|(start, end)| SnippetRange {
            start: to_out((*start).max(from)),
            end: to_out((*end).min(to)),
        })
        .filter(|r| r.start < r.end)
        .collect();
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
