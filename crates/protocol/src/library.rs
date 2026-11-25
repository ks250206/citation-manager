//! Transport-neutral desktop library client. #25 can replace the injected transport.
//! Completion is exactly once, may be on a worker thread, and must not block submit.
use crate::domain::{Paper, PaperId};
pub use cm_core::{
    ServiceError,
    papers::{NewPaper, PaperList},
};

#[derive(Debug, Clone)]
pub enum LibraryRequest {
    List(PaperList),
    Get(PaperId),
    Create(Box<NewPaper>),
    Update(Box<Paper>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryReply {
    Papers(Vec<Paper>),
    Paper(Box<Paper>),
}

pub type Completion = Box<dyn FnOnce(Result<LibraryReply, ServiceError>) + Send>;

/// Implementations enqueue work; views never receive a database/repository handle.
/// Once submit returns Ok, even transport failure must complete the callback.
pub trait LibraryClient: Send + Sync + 'static {
    fn submit(&self, request: LibraryRequest, complete: Completion) -> Result<(), ServiceError>;
    fn query(
        &self,
        _query: crate::library_query::LibraryQuery,
        _complete: Reply<crate::library_query::LibraryPage>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unavailable)
    }
    fn organization(
        &self,
        _request: crate::organization::OrganizationRequest,
        _complete: Reply<crate::organization::Organization>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unavailable)
    }
    fn lifecycle(
        &self,
        _paper: PaperId,
        _deleted: bool,
        _complete: Reply<Paper>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unavailable)
    }
    fn load_settings(
        &self,
        _complete: Reply<crate::settings::SettingsLoaded>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unavailable)
    }
    fn save_settings(
        &self,
        _settings: crate::settings::Settings,
        _complete: Reply<()>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unavailable)
    }
    fn import(
        &self,
        _request: crate::exchange::ImportRequestUi,
        _complete: Reply<crate::exchange::ImportReply>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unavailable)
    }
    fn resolve(
        &self,
        _doi: String,
        _cancel: crate::exchange::Cancellation,
        _complete: Reply<crate::exchange::MetadataResolution>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unavailable)
    }
    fn export(
        &self,
        _request: crate::exchange::ExportRequest,
        _complete: Reply<crate::exchange::Exported>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unavailable)
    }
    fn pdf(
        &self,
        _request: crate::attachments::PdfRequest,
        _progress: crate::attachments::PdfProgress,
        _complete: Reply<crate::attachments::PdfReply>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unavailable)
    }
}

pub type Reply<T> = Box<dyn FnOnce(Result<T, ServiceError>) + Send>;
