//! Feature-local organization requests; no lifecycle/auth envelope.
use crate::domain::*;
pub use cm_core::{collections::CollectionInput, tags::TagInput};
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum OrganizationRequest {
    List,
    ForPaper(PaperId),
    CreateCollection(CollectionInput),
    UpdateCollection(CollectionId, CollectionInput),
    DeleteCollection(CollectionId),
    CollectionMember(PaperId, CollectionId, bool),
    CreateTag(TagInput),
    UpdateTag(TagId, TagInput),
    DeleteTag(TagId),
    TagMember(PaperId, TagId, bool),
}
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Organization {
    pub collections: Vec<Collection>,
    pub tags: Vec<Tag>,
    /// #27 the collection/tag a create/update request produced (REST returns only this).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<OrganizationItem>,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OrganizationItem {
    Collection(Collection),
    Tag(Tag),
}
