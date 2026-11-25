//! Transport-neutral full-text search DTOs (#18). The GUI (#21), REST (#27), CLI (#28) and
//! MCP (#29) send `SearchRequest` and read `SearchReply`; all limits are enforced here.
use crate::{ServiceError, papers::PaperVisibility};
use cm_domain::{AttachmentId, CollectionId, PaperId};
use serde::{Deserialize, Serialize};

pub const DEFAULT_LIMIT: u32 = 20;
pub const MAX_LIMIT: u32 = 100;
pub const MAX_SCOPE_PAPERS: usize = 500;
/// Rows the short-term literal scan examines per call (one continuation window).
pub const DEFAULT_CANDIDATE_LIMIT: u32 = 2_000;
pub const MAX_CANDIDATE_LIMIT: u32 = 20_000;
/// FTS rows kept per clause and tokenizer, best bm25 first. More are reported as truncated.
pub const MAX_FTS_CANDIDATES: u32 = 5_000;
pub const DEFAULT_SNIPPET_CHARS: u32 = 160;
pub const MIN_SNIPPET_CHARS: u32 = 32;
pub const MAX_SNIPPET_CHARS: u32 = 1_000;
pub const DEFAULT_TIMEOUT_MS: u32 = 2_000;
pub const MAX_TIMEOUT_MS: u32 = 10_000;
/// Terms with fewer Unicode scalar values than this cannot use the trigram index
/// (measured in #53) and use the bounded literal fallback instead.
pub const TRIGRAM_MIN_SCALARS: usize = 3;
pub const MAX_WEIGHT: f64 = 100.0;

/// Indexed fields. `Identifiers` (DOI, URL, citation key) is unicode61-only; every other
/// field is in both the unicode61 and the trigram index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchField {
    Title,
    Authors,
    Abstract,
    Journal,
    Tags,
    Identifiers,
    /// Annotation quote text and comment (highlights and notes).
    Notes,
    /// Extracted PDF page text.
    Body,
}
impl SearchField {
    pub const ALL: [SearchField; 8] = [
        Self::Title,
        Self::Authors,
        Self::Abstract,
        Self::Journal,
        Self::Tags,
        Self::Identifiers,
        Self::Notes,
        Self::Body,
    ];
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn bit(self) -> u8 {
        1 << self.index()
    }
    /// Default ranking weight: title/author matches first, then tags/identifiers, then
    /// journal/abstract, annotations and finally page text (DESIGN §10.2).
    pub fn default_weight(self) -> f64 {
        match self {
            Self::Title => 10.0,
            Self::Authors => 8.0,
            Self::Tags | Self::Identifiers => 5.0,
            Self::Journal | Self::Abstract => 3.0,
            Self::Notes => 2.0,
            Self::Body => 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldWeight {
    pub field: SearchField,
    /// 0.0..=100.0. Zero removes the field's contribution to the score (not its hits).
    pub weight: f64,
}

/// Resolved per-field weights, indexed by `SearchField::index`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weights(pub [f64; 8]);
impl Default for Weights {
    fn default() -> Self {
        Self(SearchField::ALL.map(SearchField::default_weight))
    }
}
impl Weights {
    pub fn get(&self, field: SearchField) -> f64 {
        self.0[field.index()]
    }
    pub fn best(&self, mask: u8) -> f64 {
        SearchField::ALL
            .into_iter()
            .filter(|f| mask & f.bit() != 0)
            .map(|f| self.get(f))
            .fold(0.0, f64::max)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentSource {
    /// Bibliographic record: title, contributors, abstract, journal, tags, identifiers.
    Paper,
    /// Annotation quote/comment or note.
    Annotation,
    /// One extracted PDF page.
    Page,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSet {
    pub bibliographic: bool,
    pub annotations: bool,
    pub pages: bool,
}
impl Default for SourceSet {
    fn default() -> Self {
        Self {
            bibliographic: true,
            annotations: true,
            pages: true,
        }
    }
}
impl SourceSet {
    pub fn contains(self, source: DocumentSource) -> bool {
        match source {
            DocumentSource::Paper => self.bibliographic,
            DocumentSource::Annotation => self.annotations,
            DocumentSource::Page => self.pages,
        }
    }
    pub fn sources(self) -> Vec<DocumentSource> {
        [
            DocumentSource::Paper,
            DocumentSource::Annotation,
            DocumentSource::Page,
        ]
        .into_iter()
        .filter(|s| self.contains(*s))
        .collect()
    }
}

/// Where the bounded short-term (1–2 scalar) literal scan may look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortTermScope {
    /// Bibliographic and annotation documents always; PDF pages only after an explicit
    /// narrowing filter (doi/tag/collection/year, `collection` or `papers`). Otherwise the
    /// response lists PDF pages as unsearched instead of reporting zero page hits.
    #[default]
    Bounded,
    /// Every requested source including all PDF pages, still one bounded window per call;
    /// continue with `SearchCoverage::continuation` to scan the next window.
    AllPages,
}

/// Continuation/page position. `scan_after` is the last document id the previous short-term
/// window examined (0 = start); `offset` pages through that window's ranked results.
/// Separate calls do not share a snapshot: concurrent edits can shift results, and a
/// `search.rebuild` renumbers documents, so a cursor taken before a rebuild may skip or
/// repeat hits (restart from no cursor after a rebuild; see docs/ADR/fulltext-search.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchCursor {
    pub scan_after: i64,
    pub offset: u32,
}

fn default_limit() -> u32 {
    DEFAULT_LIMIT
}
fn default_candidates() -> u32 {
    DEFAULT_CANDIDATE_LIMIT
}
fn default_snippet() -> u32 {
    DEFAULT_SNIPPET_CHARS
}
fn default_timeout() -> u32 {
    DEFAULT_TIMEOUT_MS
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    /// Search DSL (docs/ADR/search-dsl.md): terms, "phrases", doi:, tag:, year:, collection:, has:pdf.
    /// May be blank only when `collection` or `papers` scopes the request: it then lists the
    /// scope's papers (a blank query without a scope is an `EmptyInput` diagnostic).
    pub query: String,
    #[serde(default)]
    pub visibility: PaperVisibility,
    /// Unambiguous collection filter by ID (direct membership), ANDed with the DSL.
    #[serde(default)]
    pub collection: Option<CollectionId>,
    /// Explicit paper scope (at most 500), ANDed with the DSL. Empty = whole library.
    #[serde(default)]
    pub papers: Vec<PaperId>,
    #[serde(default)]
    pub sources: SourceSet,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<SearchCursor>,
    #[serde(default)]
    pub short_term_scope: ShortTermScope,
    /// Documents examined per short-term scan window: 1..=20000. A window that has found no
    /// match after this many documents keeps going until a chunk matches (at most 20000
    /// documents and half the remaining `timeout_ms`), so it does not end empty just
    /// because non-matching documents (e.g. a collection's records) come first.
    #[serde(default = "default_candidates")]
    pub candidate_limit: u32,
    /// Snippet length in Unicode scalars: 32..=1000.
    #[serde(default = "default_snippet")]
    pub snippet_chars: u32,
    /// Overrides of `SearchField::default_weight` (at most one per field).
    #[serde(default)]
    pub weights: Vec<FieldWeight>,
    /// Time budget for the whole request: 1..=10000 ms, enforced inside the database. A
    /// short-term scan that reaches it ends its window early (`ScanStatus::Partial` with a
    /// continuation); an FTS lookup it interrupts yields an empty page with
    /// `coverage.timed_out` and `complete == false` (retry with a larger budget or a narrower
    /// query). Fetching the returned hits may take up to `service::RESULT_GRACE` longer.
    #[serde(default = "default_timeout")]
    pub timeout_ms: u32,
}
impl SearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            visibility: PaperVisibility::Active,
            collection: None,
            papers: Vec::new(),
            sources: SourceSet::default(),
            limit: DEFAULT_LIMIT,
            cursor: None,
            short_term_scope: ShortTermScope::Bounded,
            candidate_limit: DEFAULT_CANDIDATE_LIMIT,
            snippet_chars: DEFAULT_SNIPPET_CHARS,
            weights: Vec::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
    pub fn validate(&self) -> Result<Weights, ServiceError> {
        let invalid = |r: &str| Err(ServiceError::InvalidInput(r.into()));
        if !(1..=MAX_LIMIT).contains(&self.limit) {
            return invalid("limit must be 1..=100");
        }
        if !(1..=MAX_CANDIDATE_LIMIT).contains(&self.candidate_limit) {
            return invalid("candidate_limit must be 1..=20000");
        }
        if !(MIN_SNIPPET_CHARS..=MAX_SNIPPET_CHARS).contains(&self.snippet_chars) {
            return invalid("snippet_chars must be 32..=1000");
        }
        if !(1..=MAX_TIMEOUT_MS).contains(&self.timeout_ms) {
            return invalid("timeout_ms must be 1..=10000");
        }
        if self.papers.len() > MAX_SCOPE_PAPERS
            || self.papers.iter().any(|p| p.0.is_nil())
            || self.collection.is_some_and(|c| c.0.is_nil())
        {
            return invalid("scope needs non-nil IDs and at most 500 papers");
        }
        if !(self.sources.bibliographic || self.sources.annotations || self.sources.pages) {
            return invalid("select at least one source");
        }
        if let Some(cursor) = self.cursor
            && (cursor.scan_after < 0
                || cursor.offset > MAX_CANDIDATE_LIMIT + 2 * MAX_FTS_CANDIDATES)
        {
            return invalid("invalid cursor");
        }
        let mut weights = Weights::default();
        let mut seen = 0u8;
        for w in &self.weights {
            if !w.weight.is_finite() || !(0.0..=MAX_WEIGHT).contains(&w.weight) {
                return invalid("weights must be finite and 0..=100");
            }
            if seen & w.field.bit() != 0 {
                return invalid("at most one weight per field");
            }
            seen |= w.field.bit();
            weights.0[w.field.index()] = w.weight;
        }
        Ok(weights)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SearchReply {
    Results(Box<SearchPage>),
    /// The DSL did not parse (or a name was ambiguous). Nothing was searched.
    Invalid(QueryErrors),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryErrors {
    pub diagnostics: Vec<QueryDiagnostic>,
    /// Input after a budget was not examined.
    pub truncated: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryDiagnostic {
    /// Stable code, e.g. "UnknownField", "UnclosedQuote", "AmbiguousCollection".
    pub code: String,
    pub message: String,
    /// Half-open UTF-8 byte offsets into `SearchRequest::query`.
    pub start: usize,
    pub end: usize,
    /// Example full-clause replacements (never automatic repairs).
    pub corrections: Vec<QueryCorrection>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryCorrection {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnippetRange {
    /// Half-open UTF-8 byte offsets into `SearchHit::snippet`.
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub paper_id: PaperId,
    pub paper_title: String,
    pub doi: Option<String>,
    /// The paper is in the trash (only with `PaperVisibility::Deleted`/`All`).
    pub deleted: bool,
    pub source: DocumentSource,
    pub attachment_id: Option<AttachmentId>,
    pub annotation_id: Option<String>,
    /// 0-based physical page index (PDF viewer jump target); display page = index + 1.
    pub page_index: Option<u32>,
    /// PDF page label (may differ from the physical number, e.g. "iv").
    pub page_label: Option<String>,
    /// Fields where a query term occurs, compared like the unicode61 index: case-insensitive,
    /// Latin diacritics folded and any whitespace/line-break run equal to one space. May be
    /// empty for another tokenizer-only match (e.g. a phrase whose words are separated by
    /// punctuation) or a filter-only query.
    pub matched_fields: Vec<SearchField>,
    pub snippet_field: Option<SearchField>,
    pub snippet: String,
    pub highlights: Vec<SnippetRange>,
    /// The snippet is an excerpt of a longer field.
    pub snippet_truncated: bool,
    /// Higher is better. Sum over query terms of the best weighted tokenizer score.
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
    /// The query had no 1–2 scalar term.
    NotNeeded,
    /// Every document in the scanned scope was examined.
    Complete,
    /// The candidate window or the time budget ended first; continue with `continuation`.
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsearchedScope {
    /// Short terms were not looked up in PDF page text (no narrowing filter under
    /// `ShortTermScope::Bounded`). Missing page hits are unknown, not zero.
    PdfPagesForShortTerms,
    /// Documents after the scan window were not examined for short terms.
    DocumentsAfterWindow,
    /// PDF attachments in scope whose text is not indexed yet (extraction pending).
    PendingExtraction { attachments: u32 },
    /// PDF attachments in scope without searchable text: image-only, encrypted,
    /// damaged, too large or failed extraction (see `FulltextRequest::Status`).
    NoExtractableText { attachments: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unresolved {
    /// `tag:` key with no matching tag: the conjunction matches nothing.
    Tag(String),
    /// `collection:` name with no matching collection: the conjunction matches nothing.
    Collection(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchCoverage {
    /// Every requested scope was searched and nothing was truncated or timed out.
    pub complete: bool,
    /// An FTS clause reached `MAX_FTS_CANDIDATES`; lower-ranked matches were dropped.
    pub fts_truncated: bool,
    pub timed_out: bool,
    /// Terms answered by the bounded literal fallback instead of the trigram index.
    pub short_terms: Vec<String>,
    pub scan: ScanStatus,
    pub scanned_documents: u32,
    pub unsearched: Vec<UnsearchedScope>,
    /// Pass as `SearchRequest::cursor` to scan the next short-term window.
    pub continuation: Option<SearchCursor>,
    pub unresolved: Vec<Unresolved>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    /// Next page of this window's ranked results.
    pub next_page: Option<SearchCursor>,
    /// Ranked hits in this window (all pages).
    pub total_hits: u32,
    pub coverage: SearchCoverage,
}

pub const MAX_READ_PAGES: u32 = 50;
pub const MAX_READ_CHARS: u32 = 400_000;

/// Bounded read of extracted page text (MCP `get_paper_fulltext`, viewer search, §14.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageTextRequest {
    pub attachment: AttachmentId,
    /// 0-based first page.
    pub from_page: u32,
    /// 1..=50.
    pub max_pages: u32,
    /// Total Unicode scalars returned across pages: 1..=400000.
    pub max_chars: u32,
}
impl PageTextRequest {
    pub fn validate(&self) -> Result<(), ServiceError> {
        if self.attachment.0.is_nil()
            || !(1..=MAX_READ_PAGES).contains(&self.max_pages)
            || !(1..=MAX_READ_CHARS).contains(&self.max_chars)
        {
            return Err(ServiceError::InvalidInput(
                "page read needs an attachment, 1..=50 pages and 1..=400000 characters".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageText {
    pub page_index: u32,
    pub page_label: Option<String>,
    pub text: String,
    /// Cut by the request's character budget.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageTextRead {
    pub attachment_id: AttachmentId,
    pub paper_id: PaperId,
    pub extraction: super::extraction::AttachmentExtraction,
    /// Stored pages (with or without text) in the requested range, in page order.
    pub pages: Vec<PageText>,
    /// First page after this response, if more stored pages exist.
    pub next_page: Option<u32>,
}
