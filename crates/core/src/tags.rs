//! Tag CRUD uses the domain TagKey for manual and future import paths alike.
use crate::ServiceError;
use cm_domain::{PaperId, Tag, TagId, TagKey, organization_name};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagInput {
    pub name: String,
    /// Optional #RRGGBB. Stored spelling is preserved.
    pub color: Option<String>,
}
impl TagInput {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            color: None,
        }
    }
    pub fn validate(&self) -> Result<(String, TagKey), ServiceError> {
        if self.color.as_ref().is_some_and(|s| {
            s.len() != 7
                || !s.starts_with('#')
                || !s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
        }) {
            return Err(ServiceError::InvalidInput("color must be #RRGGBB".into()));
        }
        let name =
            organization_name(&self.name).map_err(|e| ServiceError::InvalidInput(e.to_string()))?;
        let key = TagKey::new(&name).map_err(|e| ServiceError::InvalidInput(e.to_string()))?;
        Ok((name, key))
    }
}
pub trait TagLibrary {
    fn create(&mut self, input: TagInput) -> Result<Tag, ServiceError>;
    fn get(&self, id: TagId) -> Result<Option<Tag>, ServiceError>;
    fn find(&self, key: &TagKey) -> Result<Option<Tag>, ServiceError>;
    /// Ordered by normalized key (binary), then UUID.
    fn list(&self) -> Result<Vec<Tag>, ServiceError>;
    fn update(&mut self, id: TagId, input: TagInput) -> Result<Tag, ServiceError>;
    /// Deletes the tag and its links only; never papers/attachments.
    fn delete(&mut self, id: TagId) -> Result<(), ServiceError>;
    fn set_membership(
        &mut self,
        paper: PaperId,
        tag: TagId,
        present: bool,
    ) -> Result<(), ServiceError>;
    fn for_paper(&self, paper: PaperId) -> Result<Vec<Tag>, ServiceError>;
}
pub struct TagService<R> {
    repository: R,
}
impl<R: TagLibrary> TagService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }
    pub fn into_repository(self) -> R {
        self.repository
    }
    pub fn create(&mut self, input: TagInput) -> Result<Tag, ServiceError> {
        self.repository.create(input)
    }
    pub fn get(&self, id: TagId) -> Result<Tag, ServiceError> {
        self.repository.get(id)?.ok_or(ServiceError::NotFound)
    }
    pub fn find(&self, raw: &str) -> Result<Option<Tag>, ServiceError> {
        let key = TagKey::new(raw).map_err(|e| ServiceError::InvalidInput(e.to_string()))?;
        self.repository.find(&key)
    }
    pub fn list(&self) -> Result<Vec<Tag>, ServiceError> {
        self.repository.list()
    }
    pub fn update(&mut self, id: TagId, input: TagInput) -> Result<Tag, ServiceError> {
        self.repository.update(id, input)
    }
    pub fn delete(&mut self, id: TagId) -> Result<(), ServiceError> {
        self.repository.delete(id)
    }
    pub fn add_paper(&mut self, paper: PaperId, tag: TagId) -> Result<(), ServiceError> {
        self.repository.set_membership(paper, tag, true)
    }
    pub fn remove_paper(&mut self, paper: PaperId, tag: TagId) -> Result<(), ServiceError> {
        self.repository.set_membership(paper, tag, false)
    }
    pub fn for_paper(&self, paper: PaperId) -> Result<Vec<Tag>, ServiceError> {
        self.repository.for_paper(paper)
    }
}
