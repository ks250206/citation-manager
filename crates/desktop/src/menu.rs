//! #84 popup menus (row context menu, "＋ Add", settings): entries and keyboard
//! navigation, independent of GUI code. One level of submenus is enough for
//! "Add to collection ▸" and "Tags ▸".

#[derive(Debug, Clone, PartialEq)]
pub enum Entry<A> {
    Item {
        label: String,
        /// Display text of the shortcut that runs the same action, if any.
        shortcut: Option<String>,
        enabled: bool,
        /// Some(state) shows a check box (tag membership).
        checked: Option<bool>,
        action: A,
    },
    Submenu {
        label: String,
        enabled: bool,
        entries: Vec<Entry<A>>,
    },
    Separator,
    /// Explanatory text, never selectable.
    Note(String),
}

impl<A> Entry<A> {
    pub fn item(label: impl Into<String>, action: A) -> Self {
        Self::Item {
            label: label.into(),
            shortcut: None,
            enabled: true,
            checked: None,
            action,
        }
    }
    pub fn shortcut(mut self, keys: Option<String>) -> Self {
        if let Self::Item { shortcut, .. } = &mut self {
            *shortcut = keys;
        }
        self
    }
    pub fn enabled(mut self, on: bool) -> Self {
        match &mut self {
            Self::Item { enabled, .. } | Self::Submenu { enabled, .. } => *enabled = on,
            Self::Separator | Self::Note(_) => {}
        }
        self
    }
    pub fn checked(mut self, state: bool) -> Self {
        if let Self::Item { checked, .. } = &mut self {
            *checked = Some(state);
        }
        self
    }
    pub fn submenu(label: impl Into<String>, entries: Vec<Entry<A>>) -> Self {
        let enabled = entries.iter().any(Entry::selectable);
        Self::Submenu {
            label: label.into(),
            enabled,
            entries,
        }
    }
    pub fn selectable(&self) -> bool {
        matches!(
            self,
            Self::Item { enabled: true, .. } | Self::Submenu { enabled: true, .. }
        )
    }
}

/// What a key did to the menu.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<A> {
    /// Still open (highlight moved, submenu opened or closed).
    Open,
    /// Run this action; the menu is closed.
    Run(A),
    /// Closed without an action.
    Closed,
}

/// An open menu: its entries, the highlighted row and an optional open submenu.
#[derive(Debug, Clone, PartialEq)]
pub struct Menu<A> {
    pub entries: Vec<Entry<A>>,
    /// Highlighted top-level row (always a selectable entry when one exists).
    pub index: usize,
    /// Highlighted row of the open submenu of `entries[index]`.
    pub sub: Option<usize>,
}

impl<A: Clone> Menu<A> {
    pub fn new(entries: Vec<Entry<A>>) -> Self {
        let index = first_selectable(&entries).unwrap_or(0);
        Self {
            entries,
            index,
            sub: None,
        }
    }
    fn sub_entries(&self) -> Option<&[Entry<A>]> {
        match self.entries.get(self.index) {
            Some(Entry::Submenu { entries, .. }) => Some(entries),
            _ => None,
        }
    }
    /// Move the highlight in the innermost open level, wrapping and skipping separators,
    /// notes and disabled rows.
    pub fn step(&mut self, delta: isize) {
        if let Some(current) = self.sub {
            if let Some(entries) = self.sub_entries() {
                self.sub = Some(step_in(entries, current, delta));
            }
        } else {
            self.index = step_in(&self.entries, self.index, delta);
        }
    }
    /// Right arrow: open the highlighted submenu.
    pub fn open_submenu(&mut self) -> bool {
        if self.sub.is_none()
            && let Some(Entry::Submenu {
                enabled: true,
                entries,
                ..
            }) = self.entries.get(self.index)
            && let Some(first) = first_selectable(entries)
        {
            self.sub = Some(first);
            return true;
        }
        false
    }
    /// Left arrow: back to the parent level.
    pub fn close_submenu(&mut self) -> bool {
        self.sub.take().is_some()
    }
    /// Escape closes a submenu first, then the menu.
    pub fn escape(&mut self) -> Outcome<A> {
        if self.close_submenu() {
            Outcome::Open
        } else {
            Outcome::Closed
        }
    }
    /// Enter: run the highlighted item, or open the highlighted submenu.
    pub fn confirm(&mut self) -> Outcome<A> {
        let entry = match (self.sub, self.entries.get(self.index)) {
            (Some(sub), Some(Entry::Submenu { entries, .. })) => entries.get(sub),
            (_, entry) => entry,
        };
        match entry {
            Some(Entry::Item {
                enabled: true,
                action,
                ..
            }) => Outcome::Run(action.clone()),
            Some(Entry::Submenu { enabled: true, .. }) => {
                self.open_submenu();
                Outcome::Open
            }
            _ => Outcome::Open,
        }
    }
    /// Pointer: highlight a top-level row (opening its submenu when it has one) or a row
    /// of the open submenu.
    pub fn hover(&mut self, index: usize, sub: Option<usize>) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        if !entry.selectable() {
            return;
        }
        if index != self.index {
            self.sub = None;
        }
        self.index = index;
        match (entry, sub) {
            (Entry::Submenu { entries, .. }, Some(s))
                if entries.get(s).is_some_and(Entry::selectable) =>
            {
                self.sub = Some(s)
            }
            (Entry::Submenu { .. }, None) => {
                self.open_submenu();
            }
            _ => {}
        }
    }
    /// Pointer click on a row: the action it runs, if any.
    pub fn activate(&mut self, index: usize, sub: Option<usize>) -> Outcome<A> {
        self.hover(index, sub);
        if self.index != index {
            return Outcome::Open;
        }
        match sub {
            // A submenu's own row only opens it, even when one of its items is highlighted.
            None => match self.entries.get(index) {
                Some(Entry::Item {
                    enabled: true,
                    action,
                    ..
                }) => Outcome::Run(action.clone()),
                Some(Entry::Submenu { .. }) => {
                    self.open_submenu();
                    Outcome::Open
                }
                _ => Outcome::Open,
            },
            Some(s) if self.sub == Some(s) => self.confirm(),
            Some(_) => Outcome::Open,
        }
    }
}

fn first_selectable<A>(entries: &[Entry<A>]) -> Option<usize> {
    entries.iter().position(Entry::selectable)
}

fn step_in<A>(entries: &[Entry<A>], current: usize, delta: isize) -> usize {
    let n = entries.len() as isize;
    if n == 0 || !entries.iter().any(Entry::selectable) {
        return current;
    }
    let direction = if delta < 0 { -1 } else { 1 };
    let mut index = current as isize;
    let mut remaining = delta.unsigned_abs().max(1);
    while remaining > 0 {
        index = (index + direction).rem_euclid(n);
        if entries[index as usize].selectable() {
            remaining -= 1;
        }
    }
    index as usize
}
