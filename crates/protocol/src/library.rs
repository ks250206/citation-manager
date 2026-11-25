//! Transport-neutral desktop library client. #25 can replace the injected transport.
//! Completion is exactly once, may be on a worker thread, and must not block submit.
use crate::domain::{Paper, PaperId};
pub use cm_core::{
    ServiceError,
    papers::{NewPaper, PaperList},
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum LibraryRequest {
    List(PaperList),
    Get(PaperId),
    Create(Box<NewPaper>),
    Update(Box<Paper>),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// #18 full-text search (bibliographic records, annotations, PDF page text).
    fn search(
        &self,
        _request: crate::search::SearchRequest,
        _complete: Reply<crate::search::SearchReply>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unavailable)
    }
    /// #18 extraction status and bounded page-text reads.
    fn fulltext(
        &self,
        _request: crate::search::FulltextRequest,
        _complete: Reply<crate::search::FulltextReply>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unavailable)
    }
    /// Highlights/notes (#19). Clients without annotation support report Unavailable.
    fn annotations(
        &self,
        _request: crate::annotations::AnnotationRequest,
        _complete: Reply<crate::annotations::AnnotationReply>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unavailable)
    }
    /// #26 tokens and API settings. Clients without daemon support report Unavailable.
    fn api(
        &self,
        _command: crate::api::ApiCommand,
        _complete: Reply<crate::api::ApiReply>,
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
