//! In-process transport for #10. The worker owns all library setup and SQLite I/O.
use cm_core::papers::PaperService;
use cm_protocol::library::{Completion, LibraryClient, LibraryReply, LibraryRequest, ServiceError};
use cm_storage::{LibraryDb, LibraryDirectory};
use std::{path::PathBuf, sync::mpsc, thread};

type Work = (LibraryRequest, Completion);

#[derive(Clone)]
pub struct InProcessLibraryClient {
    sender: mpsc::Sender<Work>,
}
impl InProcessLibraryClient {
    /// Starts the worker without opening or inspecting the path on the caller thread.
    /// No implicit user-library fallback. #25 will own multi-process writer exclusion.
    pub fn start(path: PathBuf) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::channel::<Work>();
        thread::Builder::new()
            .name("library-worker".into())
            .spawn(move || {
                let mut db = None;
                while let Ok((request, complete)) = receiver.recv() {
                    // Failed opens are retryable. Existing DB upgrade without #9's backup hook
                    // fails closed in #8; this adapter must not bypass it.
                    if db.is_none() {
                        match LibraryDirectory::prepare(path.clone())
                            .and_then(|dir| LibraryDb::open(&dir))
                        {
                            Ok(opened) => db = Some(opened),
                            Err(error) => {
                                // Preserve the actionable error for the local user, never log paper content.
                                complete(Err(ServiceError::InvalidInput(format!(
                                    "Cannot open library: {error}"
                                ))));
                                continue;
                            }
                        }
                    }
                    let mut service =
                        PaperService::new(db.as_mut().expect("opened above").papers());
                    let result = match request {
                        LibraryRequest::List(query) => {
                            service.list(&query).map(LibraryReply::Papers)
                        }
                        LibraryRequest::Get(id) => {
                            service.get(id).map(|p| LibraryReply::Paper(Box::new(p)))
                        }
                        LibraryRequest::Create(input) => service
                            .create(*input)
                            .map(|p| LibraryReply::Paper(Box::new(p))),
                        LibraryRequest::Update(paper) => service
                            .update(&paper)
                            .map(|p| LibraryReply::Paper(Box::new(p))),
                    };
                    complete(result);
                }
                // If the process remains alive, dropping the last client drains queued work.
                // App quit ends the process without joining this worker; save confirmation
                // must be observed before quitting to know whether a write committed.
            })?;
        Ok(Self { sender })
    }
}
impl LibraryClient for InProcessLibraryClient {
    fn submit(&self, request: LibraryRequest, complete: Completion) -> Result<(), ServiceError> {
        self.sender
            .send((request, complete))
            .map_err(|_| ServiceError::Unavailable)
    }
}
