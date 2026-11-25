//! Framework-free Command Palette ranking and recent-use history.
//! Ranking only orders descriptors; executing a command still rechecks availability.
use crate::commands::{Availability, Command, CommandContext, CommandRegistry};

/// Recent commands kept in settings; older entries fall off.
pub const MAX_RECENT: usize = 16;
/// Query bytes considered; longer input is truncated, never rejected.
pub const MAX_QUERY_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub availability: Availability,
    /// Byte offsets into `title` that matched the query, for highlighting.
    pub matched: Vec<usize>,
}

/// Case-insensitive subsequence match over the title, then the ID.
/// Returns a score (higher is better) and matched title offsets.
pub fn fuzzy_score(query: &str, title: &str, id: &str) -> Option<(i64, Vec<usize>)> {
    let query: Vec<char> = truncate(query)
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if query.is_empty() {
        return Some((0, vec![]));
    }
    if let Some((score, offsets)) = subsequence(&query, title) {
        return Some((score + 1_000, offsets));
    }
    // IDs are ASCII and dotted; a match there ranks below any title match.
    subsequence(&query, id).map(|(score, _)| (score, vec![]))
}

fn truncate(query: &str) -> &str {
    if query.len() <= MAX_QUERY_BYTES {
        return query;
    }
    let mut end = MAX_QUERY_BYTES;
    while !query.is_char_boundary(end) {
        end -= 1;
    }
    &query[..end]
}

fn subsequence(query: &[char], text: &str) -> Option<(i64, Vec<usize>)> {
    let mut offsets = Vec::with_capacity(query.len());
    let mut score = 0i64;
    let mut wanted = query.iter().peekable();
    let mut previous: Option<usize> = None;
    let mut boundary = true;
    for (index, (offset, ch)) in text.char_indices().enumerate() {
        let Some(&&next) = wanted.peek() else { break };
        let lower = ch.to_lowercase().next().unwrap_or(ch);
        if lower == next {
            score += 10;
            if boundary {
                score += 15; // word start: "ns" -> "New paper Save"
            }
            if previous == Some(index.wrapping_sub(1)) {
                score += 20; // contiguous run
            }
            if index == 0 {
                score += 10;
            }
            offsets.push(offset);
            previous = Some(index);
            wanted.next();
        }
        boundary = !ch.is_alphanumeric();
    }
    if wanted.peek().is_some() {
        return None;
    }
    // Prefer shorter titles when the match quality is otherwise equal.
    Some((score - text.chars().count() as i64, offsets))
}

/// Ranks every registered command. Empty query: recent commands first (most recent first),
/// then the rest by title. With a query: by match score, recent use as a tie-breaker.
pub fn rank(
    registry: &CommandRegistry,
    context: &CommandContext<'_>,
    query: &str,
    recent: &[String],
) -> Vec<Entry> {
    let recency = |id: &str| recent.iter().position(|r| r == id);
    let mut scored: Vec<(i64, Option<usize>, &Command, Vec<usize>)> = registry
        .iter()
        .filter_map(|command| {
            fuzzy_score(query, command.title, command.id)
                .map(|(score, matched)| (score, recency(command.id), command, matched))
        })
        .collect();
    let empty = truncate(query).trim().is_empty();
    scored.sort_by(|a, b| {
        let by_recency = match (a.1, b.1) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        let by_score = b.0.cmp(&a.0);
        let primary = if empty {
            by_recency
        } else {
            by_score.then(by_recency)
        };
        primary.then_with(|| a.2.title.cmp(b.2.title))
    });
    scored
        .into_iter()
        .map(|(_, _, command, matched)| Entry {
            id: command.id,
            title: command.title,
            description: command.description,
            availability: command.availability(context),
            matched,
        })
        .collect()
}

/// Moves `id` to the front of the history, dropping duplicates and anything unregistered.
pub fn record(recent: &mut Vec<String>, id: &str, registry: &CommandRegistry) {
    if registry.get(id).is_none() {
        return;
    }
    recent.retain(|r| r != id && registry.get(r).is_some());
    recent.insert(0, id.to_owned());
    recent.truncate(MAX_RECENT);
}
