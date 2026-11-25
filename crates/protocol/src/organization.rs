//! Feature-local organization requests; no lifecycle/auth envelope.
use crate::domain::*;
pub use cm_core::{collections::CollectionInput, tags::TagInput};
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone, Default)]
pub struct Organization {
    pub collections: Vec<Collection>,
    pub tags: Vec<Tag>,
}
