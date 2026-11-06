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
    generation: u64,
    pending: Option<(u64, Operation)>,
}
impl LibraryState {
    pub fn edited(&mut self) {
        if !self.loading() {
            self.dirty = true;
            self.message = "Unsaved changes • ⌘S to save".into();
            self.error = false;
        }
    }
    pub fn loading(&self) -> bool {
        self.pending.is_some()
    }
    pub fn saving(&self) -> bool {
        matches!(self.pending, Some((_, Operation::Save)))
    }
    fn begin(&mut self, operation: Operation) -> u64 {
        self.generation += 1;
        self.pending = Some((self.generation, operation));
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
        if self.loading() {
            return None;
        }
        let request = if let Some(paper) = &self.selected {
            // Preserve every field not exposed by this minimal editor.
            let mut paper = paper.clone();
            paper.title = title;
            paper.bibliography.abstract_text = Some(detail);
            LibraryRequest::Update(Box::new(paper))
        } else {
            let mut input = NewPaper::titled(title);
            input.bibliography.abstract_text = Some(detail);
            LibraryRequest::Create(Box::new(input))
        };
        Some((self.begin(Operation::Save), request))
    }
    /// Returns true only when the editor should adopt confirmed service data.
    pub fn complete(&mut self, id: u64, result: Result<LibraryReply, ServiceError>) -> bool {
        let Some((current, operation)) = self.pending else {
            return false;
        };
        if id != current {
            return false;
        }
        self.pending = None;
        self.error = false;
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
                self.message = format!(
                    "{}{} papers loaded • offline",
                    if self.dirty {
                        "Unsaved changes • "
                    } else {
                        ""
                    },
                    self.papers.len()
                );
                false
            }
            (Operation::Get | Operation::Save, Ok(LibraryReply::Paper(paper))) => {
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
                true
            }
            (_, Err(error)) => {
                self.error = true;
                self.message =
                    format!("Error: {error}. Nothing was confirmed saved; retry when ready.");
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
