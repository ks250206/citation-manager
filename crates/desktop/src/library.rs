//! Framework-free paging/selection state. Query/filter/sort form one cursor identity.
use cm_protocol::{domain::*, library_query::*};
#[derive(Default)]
pub struct Library {
    pub query: LibraryQuery,
    pub papers: Vec<Paper>,
    pub next: Option<LibraryCursor>,
    pub previous: Vec<Option<LibraryCursor>>,
    /// Most recently selected last: keyboard movement and the details pane follow it.
    pub selected: Vec<PaperId>,
    /// Papers of the loaded page with a PDF attachment (the PDF column).
    pub with_pdf: std::collections::HashSet<PaperId>,
    pub generation: u64,
    pending_page: Option<PendingPage>,
    /// Fixed end of a Shift range: the last plain or Cmd/Ctrl-clicked row.
    anchor: Option<PaperId>,
}
/// How a row click combines with the current selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClickKind {
    /// Cmd (macOS) / Ctrl: add or remove one row.
    pub toggle: bool,
    /// Shift: select the rows between the anchor and this one.
    pub range: bool,
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
        self.with_pdf = page.with_pdf.into_iter().collect();
        true
    }
    pub fn has_pdf(&self, id: PaperId) -> bool {
        self.with_pdf.contains(&id)
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
    /// Make `id` the whole selection, e.g. when a search hit or `paper://` link opens it in
    /// the editor, so actions on "the selection" (export Selected) follow the open paper.
    /// Unlike `select`, never toggles it off.
    pub fn select_only(&mut self, id: PaperId) {
        self.selected.clear();
        self.selected.push(id);
        self.anchor = Some(id);
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
        self.anchor = Some(id);
    }
    /// The row keyboard movement starts from and the details pane shows: the most
    /// recently selected paper that is on the loaded page.
    pub fn focused(&self) -> Option<PaperId> {
        self.selected
            .iter()
            .rev()
            .find(|id| self.index_of(**id).is_some())
            .copied()
    }
    /// Mouse selection. Plain: only this row. Cmd/Ctrl: toggle this row, keeping the rest.
    /// Shift: the rows from the anchor to this one (replacing the selection, or added to
    /// it with Cmd/Ctrl as well). The anchor stays put across Shift clicks, so a range
    /// can be grown and shrunk from the same end. Returns the paper to show, if any.
    pub fn click(&mut self, id: PaperId, kind: ClickKind) -> Option<PaperId> {
        let anchor = self.anchor.filter(|a| self.index_of(*a).is_some());
        match (kind.range, anchor) {
            (true, Some(anchor)) => {
                let range = self.range(anchor, id);
                if kind.toggle {
                    self.selected.retain(|p| !range.contains(p));
                    self.selected.extend(range);
                } else {
                    self.selected = range;
                }
            }
            _ if kind.toggle => {
                self.select(id, true);
                return self.focused();
            }
            _ => self.select_only(id),
        }
        Some(id)
    }
    /// Shift+Up/Down: move the moving end of the range by `delta` rows (clamped to the
    /// page) and select everything between it and the anchor. Returns the new end.
    pub fn extend(&mut self, delta: isize) -> Option<PaperId> {
        let head = self.adjacent(delta)?;
        let anchor = self
            .anchor
            .filter(|a| self.index_of(*a).is_some())
            .or(self.focused())
            .unwrap_or(head);
        self.anchor = Some(anchor);
        self.selected = self.range(anchor, head);
        Some(head)
    }
    /// Right-click on a selected row: keep the selection, make that row the focused one
    /// (the one "Open" shows). A row outside the selection becomes the whole selection.
    pub fn focus_within(&mut self, id: PaperId) -> Option<PaperId> {
        if let Some(index) = self.selected.iter().position(|p| *p == id) {
            self.selected.remove(index);
            self.selected.push(id);
        } else {
            self.select_only(id);
        }
        Some(id)
    }
    /// Every loaded row (Cmd/Ctrl+A); the focused row stays focused.
    pub fn select_all(&mut self) -> Option<PaperId> {
        let focused = self.focused();
        self.selected = self.papers.iter().map(|p| p.id).collect();
        if let Some(id) = focused {
            self.selected.retain(|p| *p != id);
            self.selected.push(id);
        }
        self.anchor = self.anchor.or(self.selected.first().copied());
        self.focused()
    }
    /// Selected papers in table order (for bulk actions and copied citations).
    pub fn selected_in_order(&self) -> Vec<PaperId> {
        self.papers
            .iter()
            .map(|p| p.id)
            .filter(|id| self.selected.contains(id))
            .collect()
    }
    /// Rows from `from` to `to` inclusive, ordered so that `to` comes last.
    fn range(&self, from: PaperId, to: PaperId) -> Vec<PaperId> {
        let (Some(a), Some(b)) = (self.index_of(from), self.index_of(to)) else {
            return vec![to];
        };
        let rows = self.papers.iter().map(|p| p.id);
        if a <= b {
            rows.skip(a).take(b - a + 1).collect()
        } else {
            let mut ids: Vec<_> = rows.skip(b).take(a - b + 1).collect();
            ids.reverse();
            ids
        }
    }
}
/// The loaded page's version of the paper the editor shows, when it is newer than the
/// editor's copy (changed by another client or a background job since the editor loaded
/// it). None for a new draft, a paper not on this page, or a row that is the same or older
/// — for example a page loaded before a search hit fetched the latest version into the
/// editor. `updated_at` is the store's fixed-width UTC timestamp
/// (`%Y-%m-%dT%H:%M:%fZ`), so it orders as text. Callers refresh a clean editor from it and
/// leave unsaved local edits alone (#84 review).
pub fn newer_row<'a>(editor: Option<&Paper>, page: &'a [Paper]) -> Option<&'a Paper> {
    let editor = editor?;
    page.iter()
        .find(|p| p.id == editor.id)
        .filter(|p| *p != editor && p.updated_at > editor.updated_at)
}
