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
}
