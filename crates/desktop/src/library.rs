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
    pending_page: Option<PendingPage>,
}
struct PendingPage {
    after: Option<LibraryCursor>,
    previous: Vec<Option<LibraryCursor>>,
}
impl Library {
    pub fn reload(&mut self) -> LibraryQuery {
        self.generation += 1;
        self.pending_page = None;
        self.query.after = None;
        self.previous.clear();
        self.next = None;
        // A new filter/sort starts a new cursor identity; old rows no longer apply.
        self.papers.clear();
        self.query.clone()
    }
    pub fn next_page(&mut self) -> Option<LibraryQuery> {
        let cursor = self.next.clone()?;
        let mut previous = self.previous.clone();
        previous.push(self.query.after.clone());
        self.request_page(Some(cursor), previous)
    }
    pub fn previous_page(&mut self) -> Option<LibraryQuery> {
        let mut previous = self.previous.clone();
        let after = previous.pop()?;
        self.request_page(after, previous)
    }
    fn request_page(
        &mut self,
        after: Option<LibraryCursor>,
        previous: Vec<Option<LibraryCursor>>,
    ) -> Option<LibraryQuery> {
        if self.pending_page.is_some() {
            return None;
        }
        // Keep the visible rows, cursor and page number together until success.
        let mut query = self.query.clone();
        query.after = after.clone();
        self.pending_page = Some(PendingPage { after, previous });
        self.generation += 1;
        Some(query)
    }
    pub fn fail(&mut self, generation: u64) -> bool {
        if generation != self.generation {
            return false;
        }
        self.pending_page = None;
        true
    }
    pub fn complete(&mut self, generation: u64, page: LibraryPage) -> bool {
        if generation != self.generation {
            return false;
        }
        if let Some(pending) = self.pending_page.take() {
            self.query.after = pending.after;
            self.previous = pending.previous;
        }
        self.papers = page.papers;
        self.next = page.next;
        true
    }
    /// Row reached by moving `delta` rows from the most recently selected visible
    /// paper. With no visible selection, Down starts at the first row and Up at
    /// the last. Movement clamps to the loaded page; paging stays explicit.
    pub fn adjacent(&self, delta: isize) -> Option<PaperId> {
        let last = self.papers.len().checked_sub(1)?;
        let current = self
            .selected
            .iter()
            .rev()
            .find_map(|id| self.papers.iter().position(|p| p.id == *id));
        let index = match current {
            Some(i) => i.saturating_add_signed(delta).min(last),
            None if delta < 0 => last,
            None => 0,
        };
        Some(self.papers[index].id)
    }
    /// First or last row of the loaded page (Vim `gg` / `G`).
    pub fn edge(&self, last: bool) -> Option<PaperId> {
        if last {
            self.papers.last()
        } else {
            self.papers.first()
        }
        .map(|p| p.id)
    }
    /// Position of a paper in the loaded page, used to keep it scrolled into view.
    pub fn index_of(&self, id: PaperId) -> Option<usize> {
        self.papers.iter().position(|p| p.id == id)
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
