//! Collection hierarchy and additive import-destination contracts, on workers.
use crate::{ServiceError, import::ImportDestination};
use cm_domain::{Collection, CollectionId, PaperId, organization_name};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionInput {
    pub name: String,
    pub parent: Option<CollectionId>,
    pub sort_order: i64,
}
impl CollectionInput {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            parent: None,
            sort_order: 0,
        }
    }
    pub fn validate(&self) -> Result<String, ServiceError> {
        organization_name(&self.name).map_err(|e| ServiceError::InvalidInput(e.to_string()))
    }
}

pub trait CollectionLibrary {
    fn create(&mut self, input: CollectionInput) -> Result<Collection, ServiceError>;
    fn get(&self, id: CollectionId) -> Result<Option<Collection>, ServiceError>;
    /// Flat hierarchy in (parent UUID, sort_order, UUID) order; roots first.
    fn list(&self) -> Result<Vec<Collection>, ServiceError>;
    /// Atomic rename/reorder/move; self and descendant parents are rejected.
    fn update(
        &mut self,
        id: CollectionId,
        input: CollectionInput,
    ) -> Result<Collection, ServiceError>;
    /// Promote direct children to this collection's parent and remove its links.
    /// Never delete papers, attachments, tags or descendant collections.
    fn delete(&mut self, id: CollectionId) -> Result<(), ServiceError>;
    /// Only active papers can be organized. Repeated add/remove is idempotent.
    fn set_membership(
        &mut self,
        paper: PaperId,
        collection: CollectionId,
        present: bool,
    ) -> Result<(), ServiceError>;
    fn for_paper(&self, paper: PaperId) -> Result<Vec<Collection>, ServiceError>;
}

pub struct CollectionService<R> {
    repository: R,
}
impl<R: CollectionLibrary> CollectionService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }
    pub fn into_repository(self) -> R {
        self.repository
    }
    pub fn create(&mut self, input: CollectionInput) -> Result<Collection, ServiceError> {
        self.repository.create(input)
    }
    pub fn get(&self, id: CollectionId) -> Result<Collection, ServiceError> {
        self.repository.get(id)?.ok_or(ServiceError::NotFound)
    }
    pub fn list(&self) -> Result<Vec<Collection>, ServiceError> {
        self.repository.list()
    }
    pub fn update(
        &mut self,
        id: CollectionId,
        input: CollectionInput,
    ) -> Result<Collection, ServiceError> {
        self.repository.update(id, input)
    }
    pub fn delete(&mut self, id: CollectionId) -> Result<(), ServiceError> {
        self.repository.delete(id)
    }
    pub fn add_paper(
        &mut self,
        paper: PaperId,
        collection: CollectionId,
    ) -> Result<(), ServiceError> {
        self.repository.set_membership(paper, collection, true)
    }
    pub fn remove_paper(
        &mut self,
        paper: PaperId,
        collection: CollectionId,
    ) -> Result<(), ServiceError> {
        self.repository.set_membership(paper, collection, false)
    }
    pub fn for_paper(&self, paper: PaperId) -> Result<Vec<Collection>, ServiceError> {
        self.repository.for_paper(paper)
    }
}

/// Shared with #33: existing ImportDestination remains ID-based. Check existence
/// again at commit, not just preview. Duplicate tag IDs have set semantics.
/// Applying links is atomic and additive, but does NOT include paper creation:
/// ImportService must compose both in its own single write transaction.
pub trait DestinationLibrary {
    fn validate_destination(&self, destination: &ImportDestination) -> Result<(), ServiceError>;
    fn apply_destination(
        &mut self,
        paper: PaperId,
        destination: &ImportDestination,
    ) -> Result<(), ServiceError>;
}
