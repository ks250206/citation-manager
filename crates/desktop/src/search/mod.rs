//! Framework-free cross search (#21): request building from the current library scope,
//! result presentation (snippets, sources, coverage notices) and in-PDF find over the
//! viewer's character layer.
use cm_protocol::{
    attachments::PageChar,
    domain::CollectionId,
    library_query::PaperVisibility,
    search::{
        DocumentSource, ScanStatus, SearchCoverage, SearchHit, SearchRequest, ShortTermScope,
        SnippetRange, Unresolved, UnsearchedScope,
    },
};

/// Results per request in the GUI (the service allows up to 100).
pub const PAGE_SIZE: u32 = 50;
/// Characters of context around each hit.
pub const SNIPPET_CHARS: u32 = 160;
/// In-PDF matches kept per page (find never scans unbounded).
pub const MAX_MATCHES_PER_PAGE: usize = 200;

/// Search within the current sidebar scope (collection, Trash) so filters are kept.
pub fn request(
    query: &str,
    collection: Option<CollectionId>,
    trash: bool,
    all_pages_for_short_terms: bool,
) -> SearchRequest {
    let mut request = SearchRequest::new(query.trim());
    request.collection = collection;
    request.visibility = if trash {
        PaperVisibility::Deleted
    } else {
        PaperVisibility::Active
    };
    request.limit = PAGE_SIZE;
    request.snippet_chars = SNIPPET_CHARS;
    request.short_term_scope = if all_pages_for_short_terms {
        ShortTermScope::AllPages
    } else {
        ShortTermScope::Bounded
    };
    request
}

pub fn source_label(hit: &SearchHit) -> String {
    match hit.source {
        DocumentSource::Paper => "Metadata".into(),
        DocumentSource::Annotation => "Annotation".into(),
        DocumentSource::Page => match (&hit.page_label, hit.page_index) {
            (Some(label), Some(index)) if label != &(index + 1).to_string() => {
                format!("p. {} (label {label})", index + 1)
            }
            (_, Some(index)) => format!("p. {}", index + 1),
            _ => "PDF".into(),
        },
    }
}

/// Snippet split into (text, highlighted) runs. Ranges are half-open UTF-8 byte offsets
/// (as the search service returns them); a character is highlighted when its first byte
/// falls inside a range, so malformed or mid-character ranges never panic.
pub fn snippet_runs(snippet: &str, highlights: &[SnippetRange]) -> Vec<(String, bool)> {
    let mut runs: Vec<(String, bool)> = Vec::new();
    for (offset, c) in snippet.char_indices() {
        let marked = highlights
            .iter()
            .any(|r| offset >= r.start && offset < r.end);
        match runs.last_mut() {
            Some((text, last)) if *last == marked => text.push(c),
            _ => runs.push((c.to_string(), marked)),
        }
    }
    runs
}

/// Human-readable notes on what was not searched; never "0 results" for unsearched areas.
pub fn coverage_notes(coverage: &SearchCoverage) -> Vec<String> {
    let mut notes = Vec::new();
    if !coverage.short_terms.is_empty() {
        notes.push(format!(
            "Short terms ({}) were matched literally{}",
            coverage.short_terms.join(", "),
            match coverage.scan {
                ScanStatus::Partial => format!(
                    "; scanned {} documents so far — load more to continue",
                    coverage.scanned_documents
                ),
                _ => String::new(),
            }
        ));
    }
    for scope in &coverage.unsearched {
        notes.push(match scope {
            UnsearchedScope::PdfPagesForShortTerms => {
                "PDF text was not searched for the short terms; narrow by collection/tag/year or search all pages".into()
            }
            UnsearchedScope::DocumentsAfterWindow => {
                "Later documents were not examined yet; load more to continue".into()
            }
            UnsearchedScope::PendingExtraction { attachments } => {
                format!("{attachments} PDF(s) are still being indexed")
            }
            UnsearchedScope::NoExtractableText { attachments } => format!(
                "{attachments} PDF(s) have no searchable text (image-only, encrypted or damaged)"
            ),
        });
    }
    for unresolved in &coverage.unresolved {
        notes.push(match unresolved {
            Unresolved::Tag(t) => format!("No tag named “{t}”"),
            Unresolved::Collection(c) => format!("No collection named “{c}”"),
        });
    }
    if coverage.fts_truncated {
        notes.push("Very common terms: only the best-ranked matches are shown".into());
    }
    if coverage.timed_out {
        notes.push("The search time limit was reached; results may be incomplete".into());
    }
    notes
}

/// Case-insensitive find of `needle` in a page's character layer. Returns inclusive index
/// ranges into `chars`. Whitespace runs (incl. PDF line breaks) match any whitespace run.
pub fn find_in_page(chars: &[PageChar], needle: &str) -> Vec<(usize, usize)> {
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let pattern: Vec<char> = normalize(needle.chars()).into_iter().map(fold).collect();
    if pattern.is_empty() {
        return vec![];
    }
    // Normalized text with a map back to char indices.
    let mut text: Vec<char> = Vec::with_capacity(chars.len());
    let mut origin: Vec<usize> = Vec::with_capacity(chars.len());
    let mut last_space = true;
    for (i, c) in chars.iter().enumerate() {
        let ch = c.unicode.unwrap_or('\u{fffd}');
        if ch.is_whitespace() {
            if !last_space {
                text.push(' ');
                origin.push(i);
            }
            last_space = true;
        } else {
            text.push(fold(ch));
            origin.push(i);
            last_space = false;
        }
    }
    let mut found = Vec::new();
    let mut start = 0;
    while start + pattern.len() <= text.len() && found.len() < MAX_MATCHES_PER_PAGE {
        if text[start..start + pattern.len()] == pattern[..] {
            found.push((origin[start], origin[start + pattern.len() - 1]));
            start += pattern.len();
        } else {
            start += 1;
        }
    }
    found
}

fn normalize(input: impl Iterator<Item = char>) -> Vec<char> {
    let mut out = Vec::new();
    let mut last_space = true;
    for c in input {
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    if out.last() == Some(&' ') {
        out.pop();
    }
    out
}

/// Text to look for on the page when opening a hit: the highlighted part of the snippet
/// (first run), so the viewer can mark the exact characters.
pub fn focus_text(hit: &SearchHit) -> Option<String> {
    snippet_runs(&hit.snippet, &hit.highlights)
        .into_iter()
        .find(|(_, marked)| *marked)
        .map(|(text, _)| text.trim().to_owned())
        .filter(|t| !t.is_empty())
}

/// Occurrences of `needle` in stored page text with the same rules as `find_in_page`
/// (case-insensitive, any whitespace run matches any whitespace run).
pub fn count_in_text(text: &str, needle: &str) -> usize {
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let pattern: Vec<char> = normalize(needle.chars()).into_iter().map(fold).collect();
    if pattern.is_empty() {
        return 0;
    }
    let hay: Vec<char> = normalize(text.chars()).into_iter().map(fold).collect();
    let mut count = 0;
    let mut start = 0;
    while start + pattern.len() <= hay.len() && count < MAX_MATCHES_PER_PAGE {
        if hay[start..start + pattern.len()] == pattern[..] {
            count += 1;
            start += pattern.len();
        } else {
            start += 1;
        }
    }
    count
}
