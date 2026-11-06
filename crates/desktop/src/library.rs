//! Framework-free paging/selection state. Query/filter/sort form one cursor identity.
use cm_protocol::{domain::*, library_query::*};
#[derive(Default)]
pub struct Library {
    pub query: LibraryQuery,
    pub papers: Vec<Paper>,
    pub next: Option<LibraryCursor>,
    pub previous: Vec<Option<LibraryCursor>>,
    pub selected: Vec<PaperId>,
    pub generation: u64,
}
impl Library {
    pub fn reload(&mut self) -> LibraryQuery {
        self.generation += 1;
        self.query.after = None;
        self.previous.clear();
        self.next = None;
        self.query.clone()
    }
    pub fn next_page(&mut self) -> Option<LibraryQuery> {
        let cursor = self.next.take()?;
        self.previous.push(self.query.after.clone());
        self.query.after = Some(cursor);
        self.generation += 1;
        Some(self.query.clone())
    }
    pub fn previous_page(&mut self) -> Option<LibraryQuery> {
        self.query.after = self.previous.pop()?;
        self.generation += 1;
        Some(self.query.clone())
    }
    pub fn complete(&mut self, generation: u64, page: LibraryPage) -> bool {
        if generation != self.generation {
            return false;
        }
        self.papers = page.papers;
        self.next = page.next;
        true
    }
    pub fn select(&mut self, id: PaperId, multiple: bool) {
        if !multiple {
            self.selected.clear();
        }
        if let Some(index) = self.selected.iter().position(|p| *p == id) {
            self.selected.remove(index);
        } else {
            self.selected.push(id);
        }
    }
}
