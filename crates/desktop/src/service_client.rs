//! UI state independent of GPUI and transport. Only current requests can change it.
use cm_protocol::{
    domain::Paper,
    library::{LibraryReply, LibraryRequest, NewPaper, PaperList, ServiceError},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    List { append: bool },
    Get,
    Save,
}

#[derive(Default)]
pub struct LibraryState {
    pub papers: Vec<Paper>,
    pub selected: Option<Paper>,
    pub message: String,
    pub error: bool,
    pub has_more: bool,
    page_after: Option<cm_protocol::domain::PaperId>,
    dirty: bool,
    save_confirmed: bool,
    generation: u64,
    pending: Option<(u64, Operation)>,
    listing: Option<(u64, bool)>,
    list_overrides: Vec<Paper>,
}
impl LibraryState {
    pub fn edited(&mut self) {
        if !self.editor_busy() {
            self.dirty = true;
            self.save_confirmed = false;
            self.message = "Unsaved changes • ⌘S to save".into();
            self.error = false;
        }
    }
    pub fn loading(&self) -> bool {
        self.pending.is_some() || self.listing.is_some()
    }
    pub fn editor_busy(&self) -> bool {
        self.pending.is_some()
    }
    pub fn saving(&self) -> bool {
        matches!(self.pending, Some((_, Operation::Save)))
    }
    fn begin(&mut self, operation: Operation) -> u64 {
        self.generation += 1;
        if let Operation::List { append } = operation {
            self.listing = Some((self.generation, append));
            self.list_overrides.clear();
        } else {
            self.pending = Some((self.generation, operation));
            self.save_confirmed = false;
        }
        self.message = if operation == Operation::Save {
            "Saving…"
        } else {
            "Loading…"
        }
        .into();
        self.error = false;
        self.generation
    }
    pub fn new_paper(&mut self) {
        if self.saving() {
            return;
        }
        self.generation += 1;
        self.pending = None;
        self.selected = None;
        self.dirty = true;
        self.save_confirmed = false;
        self.message = "New paper • unsaved".into();
        self.error = false;
    }
    pub fn list(&mut self, append: bool) -> Option<(u64, LibraryRequest)> {
        if self.saving() {
            return None;
        }
        let query = PaperList {
            after: if append { self.page_after } else { None },
            ..PaperList::default()
        };
        Some((
            self.begin(Operation::List { append }),
            LibraryRequest::List(query),
        ))
    }
    pub fn select(&mut self, id: cm_protocol::domain::PaperId) -> Option<(u64, LibraryRequest)> {
        if self.saving() {
            return None;
        }
        Some((self.begin(Operation::Get), LibraryRequest::Get(id)))
    }
    pub fn save(&mut self, title: String, detail: String) -> Option<(u64, LibraryRequest)> {
        if self.editor_busy() {
            return None;
        }
        let request = if let Some(paper) = &self.selected {
            // Preserve every field not exposed by this minimal editor.
            let mut paper = paper.clone();
            paper.title = title;
            if !detail.is_empty() || paper.bibliography.abstract_text.is_some() {
                paper.bibliography.abstract_text = Some(detail);
            }
            LibraryRequest::Update(Box::new(paper))
        } else {
            let mut input = NewPaper::titled(title);
            input.bibliography.abstract_text = (!detail.is_empty()).then_some(detail);
            LibraryRequest::Create(Box::new(input))
        };
        Some((self.begin(Operation::Save), request))
    }
    /// Returns true only when the editor should adopt confirmed service data.
    pub fn complete(&mut self, id: u64, result: Result<LibraryReply, ServiceError>) -> bool {
        let operation = if let Some((_, append)) =
            self.listing.filter(|(current, _)| *current == id)
        {
            self.listing = None;
            Operation::List { append }
        } else if let Some((_, operation)) = self.pending.filter(|(current, _)| *current == id) {
            self.pending = None;
            operation
        } else {
            return false;
        };
        self.error = false;
        let overrides = if matches!(operation, Operation::List { .. }) {
            std::mem::take(&mut self.list_overrides)
        } else {
            vec![]
        };
        match (operation, result) {
            (Operation::List { append }, Ok(LibraryReply::Papers(papers))) => {
                self.page_after =
                    papers
                        .last()
                        .map(|p| p.id)
                        .or(if append { self.page_after } else { None });
                self.has_more = papers.len() == PaperList::default().limit as usize;
                if !append {
                    self.papers.clear();
                }
                for paper in papers {
                    if !self.papers.iter().any(|p| p.id == paper.id) {
                        self.papers.push(paper);
                    }
                }
                // Get/Save replies after this list began take precedence over its old snapshot.
                for paper in overrides {
                    if let Some(row) = self.papers.iter_mut().find(|p| p.id == paper.id) {
                        *row = paper;
                    } else {
                        self.papers.push(paper);
                    }
                }
                self.papers.sort_by_key(|p| p.id.0);
                self.message = if self.saving() {
                    "Saving…".into()
                } else {
                    format!(
                        "{}{} papers loaded • offline",
                        if self.dirty {
                            "Unsaved changes • "
                        } else if self.save_confirmed {
                            "Saved locally • "
                        } else {
                            ""
                        },
                        self.papers.len()
                    )
                };
                false
            }
            (Operation::Get | Operation::Save, Ok(LibraryReply::Paper(paper))) => {
                if self.listing.is_some() {
                    self.list_overrides.retain(|p| p.id != paper.id);
                    self.list_overrides.push(*paper.clone());
                }
                self.message = if operation == Operation::Save {
                    "Saved locally"
                } else {
                    "Loaded from library"
                }
                .into();
                if let Some(row) = self.papers.iter_mut().find(|p| p.id == paper.id) {
                    *row = *paper.clone();
                } else {
                    self.papers.push(*paper.clone());
                }
                self.papers.sort_by_key(|p| p.id.0);
                self.selected = Some(*paper);
                self.dirty = false;
                self.save_confirmed = operation == Operation::Save;
                true
            }
            (_, Err(error)) => {
                self.error = true;
                self.message = match operation {
                    Operation::List { .. } => format!(
                        "{}Could not load list: {error}. Reload to retry.",
                        if self.saving() {
                            "Saving… • "
                        } else if self.save_confirmed {
                            "Saved locally • "
                        } else {
                            ""
                        }
                    ),
                    Operation::Get => format!("Could not load paper: {error}. Retry when ready."),
                    Operation::Save => format!(
                        "Could not save: {error}. Nothing was confirmed saved; retry when ready."
                    ),
                };
                false
            }
            _ => {
                self.error = true;
                self.message = "Error: unexpected service response".into();
                false
            }
        }
    }
}

pub type Response = (u64, Result<LibraryReply, ServiceError>);

/// A transport dropping its accepted callback must release the UI's pending state.
pub fn response_completion(
    sender: std::sync::mpsc::Sender<Response>,
    id: u64,
) -> cm_protocol::library::Completion {
    struct Delivery {
        sender: std::sync::mpsc::Sender<Response>,
        id: u64,
        delivered: bool,
    }
    impl Delivery {
        fn finish(mut self, result: Result<LibraryReply, ServiceError>) {
            self.delivered = true;
            let _ = self.sender.send((self.id, result));
        }
    }
    impl Drop for Delivery {
        fn drop(&mut self) {
            if !self.delivered {
                let _ = self.sender.send((self.id, Err(ServiceError::Unavailable)));
            }
        }
    }
    let delivery = Delivery {
        sender,
        id,
        delivered: false,
    };
    Box::new(move |result| delivery.finish(result))
}
