//! Minimal persisted values. No UI, transport, SQLite or network dependencies.
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};
use uuid::Uuid;

macro_rules! id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);
    };
}
id!(PaperId);
id!(AttachmentId);
id!(CollectionId);
id!(TagId);

/// Provider namespace and identifier are persisted separately from internal UUIDs.
/// Normalization/duplicate matching is provider-specific and belongs to #33 (adapters: #13/#14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalId {
    pub namespace: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paper {
    pub id: PaperId,
    pub title: String,
    pub external_ids: Vec<ExternalId>,
    #[serde(flatten)]
    pub bibliography: Bibliography,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
}

/// Bibliographic metadata; absent additions preserve the #7 JSON fixtures.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Bibliography {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub paper_type: Option<String>,
    #[serde(rename = "abstract", skip_serializing_if = "Option::is_none")]
    pub abstract_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doi: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citation_key: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<Contributor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_json: Option<serde_json::Value>,
}

/// Ordered contributors, including author/editor and literal organizations.
/// Names/ORCID are retained verbatim; provider-specific reconciliation is #33.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contributor {
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub literal_name: Option<String>,
    pub orcid: Option<String>,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: AttachmentId,
    pub paper_id: PaperId,
    pub storage_key: StorageKey,
    pub sha256: [u8; 32],
    pub original_name: String,
    pub media_type: String,
    pub byte_size: u64,
}

/// Managed relative key, never an arbitrary client-supplied filesystem path.
/// The file-store adapter must additionally prevent symlink escapes (#8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StorageKey(String);

impl TryFrom<String> for StorageKey {
    type Error = ValueError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.contains(['\\', '\0', ':'])
            || value.split('/').any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(ValueError(
                "storage key must contain only relative normal components",
            ));
        }
        Ok(Self(value))
    }
}
impl From<StorageKey> for String {
    fn from(value: StorageKey) -> Self {
        value.0
    }
}
impl StorageKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Zero-based physical index. Maximum is u32::MAX - 1 to allow lossless display conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct PageIndex(u32);

impl TryFrom<u32> for PageIndex {
    type Error = ValueError;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        value
            .checked_add(1)
            .ok_or(ValueError("page index exceeds display range"))?;
        Ok(Self(value))
    }
}
impl From<PageIndex> for u32 {
    fn from(value: PageIndex) -> Self {
        value.0
    }
}
impl PageIndex {
    pub fn physical_display(self) -> PhysicalPageNumber {
        PhysicalPageNumber(self.0 + 1)
    }
}

/// One-based physical display number, independent of any PDF page label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct PhysicalPageNumber(u32);
impl TryFrom<u32> for PhysicalPageNumber {
    type Error = ValueError;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if value == 0 {
            return Err(ValueError("physical page number must be positive"));
        }
        Ok(Self(value))
    }
}
impl From<PhysicalPageNumber> for u32 {
    fn from(value: PhysicalPageNumber) -> Self {
        value.0
    }
}
impl PhysicalPageNumber {
    pub fn index(self) -> PageIndex {
        PageIndex(self.0 - 1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageReference {
    pub attachment_id: AttachmentId,
    pub page_index: PageIndex,
    /// PDF label such as "iv" or "S12"; never interpreted as a physical index.
    pub page_label: Option<String>,
}

/// Provenance, not a permission or proof of a resolved citation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationSource {
    Human,
    Ai,
    Import,
}

/// An unresolved quote must survive persistence without inventing a page location.
/// Coordinates, reanchoring and full annotations are extended in #11/#19.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Anchor {
    Resolved {
        page: PageReference,
        quote: String,
    },
    Unresolved {
        attachment_id: AttachmentId,
        quote: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueError(&'static str);
impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl Error for ValueError {}
