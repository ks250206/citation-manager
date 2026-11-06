//! Shared organization values. Import adapters must reuse `TagKey`, not normalize again.
use crate::{CollectionId, TagId, ValueError};
use serde::{Deserialize, Serialize};

/// V1 key: collapse Unicode White_Space runs to ASCII space, trim, then apply
/// Unicode per-character lowercase (including multi-character expansions).
/// Uses the Unicode tables of the pinned Rust toolchain. No NFC/NFKC, accent,
/// width, locale-specific or full case folding: `é` != `e\u{301}`, `ß` != `ss`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TagKey(String);
impl TagKey {
    pub fn new(raw: &str) -> Result<Self, ValueError> {
        let display = organization_name(raw)?;
        let key: String = display.chars().flat_map(char::to_lowercase).collect();
        if key.chars().count() > 256 {
            return Err(ValueError("normalized tag key exceeds 256 characters"));
        }
        Ok(Self(key))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for TagKey {
    type Error = ValueError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let key = Self::new(&value)?;
        if key.as_str() != value {
            return Err(ValueError("tag key must already be normalized"));
        }
        Ok(key)
    }
}
impl From<TagKey> for String {
    fn from(value: TagKey) -> Self {
        value.0
    }
}

/// Shared display-name policy: at most 4096 input bytes and 256 resulting Unicode
/// scalars, nonempty after whitespace collapse. Non-whitespace controls rejected.
pub fn organization_name(raw: &str) -> Result<String, ValueError> {
    if raw.len() > 4096 || raw.chars().any(|c| c.is_control() && !c.is_whitespace()) {
        return Err(ValueError("invalid organization name"));
    }
    let value = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.is_empty() || value.chars().count() > 256 {
        return Err(ValueError(
            "organization name must contain 1..=256 characters",
        ));
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub id: CollectionId,
    pub parent: Option<CollectionId>,
    pub name: String,
    pub sort_order: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub id: TagId,
    pub name: String,
    pub key: TagKey,
    pub color: Option<String>,
}
