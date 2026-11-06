use super::{PaperDraft, normalize::spaces};
use std::collections::HashSet;
/// Retain supplied keys if available; deterministic Unicode-safe suffix allocation.
pub fn allocate(draft: &PaperDraft, used: &mut HashSet<String>) -> String {
    let base = draft.bibliography.citation_key.clone().unwrap_or_else(|| {
        let author = draft
            .bibliography
            .authors
            .first()
            .and_then(|a| a.family_name.as_ref().or(a.literal_name.as_ref()))
            .map(String::as_str)
            .unwrap_or("Anon");
        let title = spaces(&draft.title)
            .split_whitespace()
            .next()
            .unwrap_or("Untitled")
            .to_owned();
        let clean = |s: &str| {
            s.chars()
                .filter(|c| c.is_alphanumeric())
                .take(40)
                .collect::<String>()
        };
        format!(
            "{}{}{}",
            clean(author),
            draft
                .bibliography
                .year
                .map(|y| y.to_string())
                .unwrap_or_default(),
            clean(&title)
        )
    });
    let base = if base.is_empty() {
        "Untitled".into()
    } else {
        base
    };
    let mut base = base;
    if draft.bibliography.citation_key.is_none() {
        let mut end = base.len().min(256);
        while !base.is_char_boundary(end) {
            end -= 1;
        }
        base.truncate(end);
    }
    let mut key = base.clone();
    let mut suffix = 2;
    while used.contains(&key) {
        let ending = format!("_{suffix}");
        let mut end = base.len().min(256 - ending.len());
        while !base.is_char_boundary(end) {
            end -= 1;
        }
        key = format!("{}{ending}", &base[..end]);
        suffix += 1;
    }
    used.insert(key.clone());
    key
}
