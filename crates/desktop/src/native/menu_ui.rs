//! #84 popup menus: the row context menu, "＋ Add", the ⚙ settings menu and the table
//! column toggles. Entries and keyboard navigation live in `crate::menu`; this file only
//! builds entries from the current state, draws them and runs the chosen action.
use super::*;
use crate::{
    bulk,
    columns::{self, ColumnKind},
    commands::{api_ids, ids, workbench_ids as wb},
    menu::{Entry, Menu, Outcome},
};

#[derive(Debug, Clone, PartialEq)]
pub(super) enum MenuAction {
    /// A palette command (same availability checks and handler).
    Command(&'static str),
    Collection(CollectionId),
    Tag(TagId, bool),
    Column(ColumnKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuKind {
    Row,
    Add,
    Settings,
    Columns,
}

pub(super) struct OpenMenu {
    pub kind: MenuKind,
    pub menu: Menu<MenuAction>,
    pub position: Point<Pixels>,
}

const ITEM_H: f32 = 26.;
const SEPARATOR_H: f32 = 9.;
const NOTE_H: f32 = 24.;

fn entry_height<A>(entry: &Entry<A>) -> f32 {
    match entry {
        Entry::Separator => SEPARATOR_H,
        Entry::Note(_) => NOTE_H,
        _ => ITEM_H,
    }
}

impl LibraryView {
    /// Shortcut text shown next to a menu entry: the GPUI binding that works in every key
    /// mode first, then the user keymap.
    fn menu_shortcut(&self, id: &str) -> Option<String> {
        palette_ui::fixed_shortcut(id)
            .map(palette_ui::display_keys)
            .or_else(|| self.shortcut_label(id))
    }
    fn command_entry(&self, label: impl Into<String>, id: &'static str) -> Entry<MenuAction> {
        let enabled = matches!(
            self.registry.availability(id, &self.command_context()),
            crate::commands::Availability::Enabled
        );
        Entry::item(label, MenuAction::Command(id))
            .shortcut(self.menu_shortcut(id))
            .enabled(enabled)
    }
    fn column_entries(&self) -> Vec<Entry<MenuAction>> {
        self.settings
            .columns
            .iter()
            .filter_map(|c| ColumnKind::from_name(&c.name).map(|k| (k, c.visible)))
            .map(|(kind, visible)| {
                Entry::item(kind.name(), MenuAction::Column(kind))
                    .checked(visible)
                    .enabled(kind.can_hide())
            })
            .collect()
    }
    fn menu_entries(&self, kind: MenuKind) -> Vec<Entry<MenuAction>> {
        match kind {
            MenuKind::Add => vec![
                self.command_entry("New paper", ids::NEW),
                self.command_entry("Add paper by DOI…", ids::DOI),
                self.command_entry("Add PDFs…", wb::VIEW_PDFS),
                Entry::Separator,
                self.command_entry("Import BibTeX…", ids::IMPORT),
                self.command_entry("Import RIS…", ids::IMPORT_RIS),
            ],
            MenuKind::Settings => vec![
                self.command_entry("Organize & export", wb::VIEW_ORGANIZE),
                self.command_entry("API & integrations", api_ids::OPEN_SETTINGS),
                Entry::Separator,
                Entry::submenu("Table columns", self.column_entries()),
                self.command_entry("Vim keys in the table", wb::TOGGLE_VIM)
                    .checked(self.settings.vim),
                self.command_entry("Reading layout", wb::READING)
                    .checked(self.settings.reading),
                Entry::Separator,
                self.command_entry("Command Palette…", ids::PALETTE),
            ],
            MenuKind::Columns => self.column_entries(),
            MenuKind::Row => self.row_entries(),
        }
    }
    fn row_entries(&self) -> Vec<Entry<MenuAction>> {
        let selected = self.library.selected_in_order();
        let n = selected.len();
        let filter = &self.library.query.filter;
        let trash = filter.visibility == PaperVisibility::Deleted;
        let many = |one: &str, more: String| if n > 1 { more } else { one.to_string() };
        let viewing = filter.collection;
        let collections: Vec<Entry<MenuAction>> = self
            .organization
            .collections
            .iter()
            .filter(|c| Some(c.id) != viewing)
            .map(|c| Entry::item(c.name.clone(), MenuAction::Collection(c.id)))
            .collect();
        // One paper: check boxes from its loaded memberships (disabled until they arrive;
        // the open menu is rebuilt then). Several papers: a tag is added to all of them.
        let member_tags: Vec<TagId> = self.membership.tags.iter().map(|t| t.id).collect();
        let membership = self.membership_for.map(|p| (p, member_tags.as_slice()));
        let tags: Vec<Entry<MenuAction>> = self
            .organization
            .tags
            .iter()
            .map(|t| match bulk::tag_choice(t.id, &selected, membership) {
                bulk::TagChoice::Toggle { present } => {
                    Entry::item(format!("#{}", t.name), MenuAction::Tag(t.id, !present))
                        .checked(present)
                }
                bulk::TagChoice::AddToAll => {
                    Entry::item(format!("Add #{}", t.name), MenuAction::Tag(t.id, true))
                }
                bulk::TagChoice::Loading => Entry::item(
                    format!("#{} (loading…)", t.name),
                    MenuAction::Tag(t.id, true),
                )
                .enabled(false),
            })
            .collect();
        let mut entries = vec![
            self.command_entry("Open", ids::OPEN).enabled(n >= 1),
            self.command_entry(
                many("Copy citation", format!("Copy {n} citations")),
                ids::COPY,
            )
            .enabled(n >= 1),
            Entry::Separator,
        ];
        if trash {
            entries.push(Entry::Note("Restore papers to file them".into()));
        } else {
            entries.push(if collections.is_empty() {
                Entry::submenu("Add to collection (none yet)", vec![])
            } else if viewing.is_some() {
                Entry::submenu("Move to collection", collections)
            } else {
                Entry::submenu("Add to collection", collections)
            });
            entries.push(if tags.is_empty() {
                Entry::submenu("Tags (none yet)", vec![])
            } else {
                Entry::submenu("Tags", tags)
            });
        }
        entries.push(Entry::Separator);
        entries.push(
            self.command_entry(
                match (trash, n > 1) {
                    (false, false) => "Move to Trash".to_string(),
                    (false, true) => format!("Move {n} papers to Trash"),
                    (true, false) => "Restore from Trash".to_string(),
                    (true, true) => format!("Restore {n} papers"),
                },
                wb::TRASH,
            )
            .enabled(n >= 1),
        );
        entries
    }
    /// Rebuild an open menu from the current state (e.g. memberships arrived), keeping the
    /// highlighted row when it is still selectable.
    pub(super) fn refresh_open_menu(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = self.menu.as_ref().map(|m| m.kind) else {
            return;
        };
        let entries = self.menu_entries(kind);
        if let Some(open) = &mut self.menu {
            let (index, sub) = (open.menu.index, open.menu.sub);
            let mut menu = Menu::new(entries);
            if menu.entries.get(index).is_some_and(Entry::selectable) {
                menu.index = index;
                if sub.is_some() && menu.open_submenu() {
                    menu.sub = sub
                        .filter(|s| match &menu.entries[index] {
                            Entry::Submenu { entries, .. } => {
                                entries.get(*s).is_some_and(Entry::selectable)
                            }
                            _ => false,
                        })
                        .or(menu.sub);
                }
            }
            open.menu = menu;
        }
        cx.notify();
    }
    pub(super) fn open_menu(
        &mut self,
        kind: MenuKind,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resolver.reset();
        self.menu = Some(OpenMenu {
            kind,
            menu: Menu::new(self.menu_entries(kind)),
            position,
        });
        // Deferred: a right-click opens the menu during mouse-down, and the table under the
        // pointer would otherwise take focus back after this handler.
        let focus = self.menu_focus.clone();
        window.defer(cx, move |window, cx| focus.focus(window, cx));
        cx.notify();
    }
    /// Keyboard-opened menus have no pointer position: open just below the focused row
    /// (or at the top of the list), a little in from its left edge.
    pub(super) fn keyboard_menu_position(&self) -> Point<Pixels> {
        let state = self.rows.0.borrow();
        let bounds = state.base_handle.bounds();
        let offset = state.base_handle.offset();
        let row = self
            .library
            .focused()
            .and_then(|id| self.library.index_of(id))
            .map_or(0., |i| (i + 1) as f32 * 32.);
        let y = (bounds.origin.y + offset.y + px(row))
            .clamp(bounds.origin.y, bounds.origin.y + bounds.size.height);
        point(bounds.origin.x + px(160.), y)
    }
    pub(super) fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_none() {
            return;
        }
        if self.screen == Screen::Library {
            self.list_focus().focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }
    fn menu_outcome(
        &mut self,
        outcome: Outcome<MenuAction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Outcome::Open => cx.notify(),
            Outcome::Closed => self.close_menu(window, cx),
            Outcome::Run(action) => {
                let Some(open) = self.menu.as_ref() else {
                    return;
                };
                let (kind, position, index, sub) =
                    (open.kind, open.position, open.menu.index, open.menu.sub);
                self.close_menu(window, cx);
                let column = matches!(action, MenuAction::Column(_));
                self.run_menu_action(action, window, cx);
                // Column toggles keep their menu open so several can be changed in a row.
                if column {
                    self.open_menu(kind, position, window, cx);
                    if let Some(open) = &mut self.menu {
                        open.menu.index = index;
                        open.menu.sub = sub;
                    }
                }
            }
        }
    }
    fn run_menu_action(&mut self, action: MenuAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            MenuAction::Command(id) => self.run_command(id, window, cx),
            MenuAction::Column(kind) => {
                if columns::toggle(&mut self.settings.columns, kind) {
                    self.persist(cx);
                }
            }
            MenuAction::Collection(target) => {
                let papers = self.library.selected_in_order();
                let requests = bulk::collection_requests(
                    &papers,
                    self.library.query.filter.collection,
                    target,
                );
                let name = self
                    .organization
                    .collections
                    .iter()
                    .find(|c| c.id == target)
                    .map_or_else(String::new, |c| c.name.clone());
                let verb = if self.library.query.filter.collection.is_some() {
                    format!("Moved to {name}:")
                } else {
                    format!("Added to {name}:")
                };
                self.run_bulk(
                    Lane::Organization,
                    verb,
                    requests.into_iter().map(bulk::organization_step).collect(),
                    cx,
                );
            }
            MenuAction::Tag(tag, present) => {
                let papers = self.library.selected_in_order();
                let name = self
                    .organization
                    .tags
                    .iter()
                    .find(|t| t.id == tag)
                    .map_or_else(String::new, |t| t.name.clone());
                let verb = if present {
                    format!("Tagged #{name}:")
                } else {
                    format!("Removed #{name} from")
                };
                self.run_bulk(
                    Lane::Organization,
                    verb,
                    bulk::tag_requests(&papers, tag, present)
                        .into_iter()
                        .map(bulk::organization_step)
                        .collect(),
                    cx,
                );
            }
        }
    }
    /// Send single-paper requests one after another; one status line reports the result.
    pub(super) fn run_bulk(
        &mut self,
        lane: Lane,
        verb: String,
        steps: Vec<bulk::Step>,
        cx: &mut Context<Self>,
    ) {
        if steps.is_empty() {
            self.status("Nothing to change", false, cx);
            return;
        }
        // One bulk run at a time, whatever its lane: runs share the status line and the
        // follow-up reads, and a second run must never replace another run's ticket.
        if let Some(running) = self.active_bulk() {
            let text = format!(
                "Wait until “{}” finishes",
                running.verb.trim_end_matches(':')
            );
            self.status(text, true, cx);
            return;
        }
        if self.locked() || self.busy(lane) {
            self.status("Wait for the current operation", true, cx);
            return;
        }
        self.call(lane, Event::Bulk, |client, done| {
            bulk::run(client, steps, done);
            Ok(())
        });
        if let Some(&ticket) = self.pending.get(&lane) {
            self.bulk.start(
                lane,
                ticket,
                bulk::BulkRun {
                    verb,
                    // Trash / restore changes the open paper itself; memberships do not.
                    refetch: lane == Lane::Editor,
                },
            );
        }
        cx.notify();
    }
    /// The bulk run still in progress, if any.
    pub(super) fn active_bulk(&self) -> Option<&bulk::BulkRun> {
        self.bulk.active(|lane| self.pending.get(&lane).copied())
    }
    pub(super) fn bulk_reply(
        &mut self,
        lane: Lane,
        ticket: u64,
        result: Result<bulk::BulkReport, ServiceError>,
        cx: &mut Context<Self>,
    ) {
        // The verb and follow-up belong to this run, never to another one.
        let run = self.bulk.finish(lane, ticket).unwrap_or(bulk::BulkRun {
            verb: "Bulk change:".into(),
            refetch: lane == Lane::Editor,
        });
        match result {
            Ok(report) => {
                let failed = report.error.is_some();
                self.status(report.summary(&run.verb), failed, cx);
            }
            Err(e) => self.status(
                format!("{} failed: {e}", run.verb.trim_end_matches(':')),
                true,
                cx,
            ),
        }
        self.reload(cx);
        self.load_membership();
        // The open paper may have moved to or from Trash: show its stored state. (This makes
        // it the whole selection; after a collection or tag change the selection is kept.)
        if let Some(id) = self.editor.as_ref().map(|p| p.id)
            && run.refetch
            && !self.dirty
        {
            self.call(Lane::Editor, Event::Loaded, |c, done| {
                c.submit(LibraryRequest::Get(id), done)
            });
        }
    }
    /// Trash (or restore, in the Trash view) every selected paper. One paper keeps the
    /// details pane's own path, which also shows the result in the editor.
    pub(super) fn trash_selected(&mut self, cx: &mut Context<Self>) {
        let papers = self.library.selected_in_order();
        if papers.len() <= 1 {
            if let Some(&id) = papers.first()
                && self.editor.as_ref().map(|p| p.id) != Some(id)
            {
                return self.status("Open the paper first", true, cx);
            }
            return self.toggle_trash(cx);
        }
        if !self.can_replace_editor(cx) {
            return;
        }
        let restore = self.library.query.filter.visibility == PaperVisibility::Deleted;
        let verb = if restore {
            "Restored".to_string()
        } else {
            "Moved to Trash:".to_string()
        };
        self.run_bulk(
            Lane::Editor,
            verb,
            papers
                .into_iter()
                .map(|id| bulk::lifecycle_step(id, !restore))
                .collect(),
            cx,
        );
    }
    pub(super) fn menu_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(open) = &self.menu else {
            return div().into_any_element();
        };
        let menu = &open.menu;
        let sub = match (&menu.entries.get(menu.index), menu.sub) {
            (Some(Entry::Submenu { entries, .. }), Some(_)) => Some(entries),
            _ => None,
        };
        let sub_top: f32 = menu.entries[..menu.index.min(menu.entries.len())]
            .iter()
            .map(entry_height)
            .sum();
        div()
            .id("menu-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close_menu(window, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, window, cx| this.close_menu(window, cx)),
            )
            .child(
                anchored()
                    .position(open.position)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("menu")
                            .key_context("Menu")
                            .track_focus(&self.menu_focus)
                            .on_action(cx.listener(|this, _: &MenuNext, _, cx| {
                                if let Some(m) = &mut this.menu {
                                    m.menu.step(1);
                                }
                                cx.notify();
                            }))
                            .on_action(cx.listener(|this, _: &MenuPrevious, _, cx| {
                                if let Some(m) = &mut this.menu {
                                    m.menu.step(-1);
                                }
                                cx.notify();
                            }))
                            .on_action(cx.listener(|this, _: &MenuOpenSubmenu, _, cx| {
                                if let Some(m) = &mut this.menu {
                                    m.menu.open_submenu();
                                }
                                cx.notify();
                            }))
                            .on_action(cx.listener(|this, _: &MenuCloseSubmenu, _, cx| {
                                if let Some(m) = &mut this.menu {
                                    m.menu.close_submenu();
                                }
                                cx.notify();
                            }))
                            .on_action(cx.listener(|this, _: &MenuConfirm, window, cx| {
                                if let Some(outcome) = this.menu.as_mut().map(|m| m.menu.confirm())
                                {
                                    this.menu_outcome(outcome, window, cx);
                                }
                            }))
                            .on_action(cx.listener(|this, _: &MenuClose, window, cx| {
                                if let Some(outcome) = this.menu.as_mut().map(|m| m.menu.escape()) {
                                    this.menu_outcome(outcome, window, cx);
                                }
                            }))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                            .flex()
                            .items_start()
                            .child(self.menu_panel(&menu.entries, menu.index, None, cx))
                            .when_some(sub, |el, entries| {
                                el.child(div().mt(px(sub_top)).child(self.menu_panel(
                                    entries,
                                    menu.sub.unwrap_or(0),
                                    Some(menu.index),
                                    cx,
                                )))
                            }),
                    ),
            )
            .into_any_element()
    }
    /// One level of a menu. `parent` is the top-level row a submenu belongs to.
    fn menu_panel(
        &self,
        entries: &[Entry<MenuAction>],
        highlighted: usize,
        parent: Option<usize>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let any_checks = entries.iter().any(|e| {
            matches!(
                e,
                Entry::Item {
                    checked: Some(_),
                    ..
                }
            )
        });
        div()
            .id(("menu-panel", parent.map_or(0, |p| p + 1)))
            .min_w(px(220.))
            .max_w(px(360.))
            .p_1()
            .flex()
            .flex_col()
            .rounded(px(RADIUS.md))
            .border_1()
            .border_color(rgb(tok().border))
            .bg(rgb(tok().surface))
            .shadow(shadow_of(theme::Elevation::Floating))
            .children(entries.iter().enumerate().map(|(i, entry)| {
                let (top, sub) = match parent {
                    Some(p) => (p, Some(i)),
                    None => (i, None),
                };
                let row = div()
                    .id(("menu-row", top * 1000 + sub.unwrap_or(999)))
                    .flex_shrink_0();
                match entry {
                    Entry::Separator => row
                        .h(px(SEPARATOR_H))
                        .flex()
                        .items_center()
                        .child(div().h(px(1.)).w_full().bg(rgb(tok().border))),
                    Entry::Note(text) => row
                        .h(px(NOTE_H))
                        .px_3()
                        .flex()
                        .items_center()
                        .child(hint(text.clone())),
                    Entry::Item { .. } | Entry::Submenu { .. } => {
                        let (label, shortcut, enabled, checked, submenu) = match entry {
                            Entry::Item {
                                label,
                                shortcut,
                                enabled,
                                checked,
                                ..
                            } => (label, shortcut.clone(), *enabled, *checked, false),
                            Entry::Submenu { label, enabled, .. } => {
                                (label, None, *enabled, None, true)
                            }
                            _ => unreachable!(),
                        };
                        let active = i == highlighted && enabled;
                        row.h(px(ITEM_H))
                            .px_2()
                            .rounded(px(RADIUS.sm))
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(active, |el| el.bg(rgb(tok().accent_soft)))
                            .text_color(rgb(if enabled {
                                tok().text
                            } else {
                                tok().disabled_text
                            }))
                            .when(any_checks, |el| {
                                let color = if enabled {
                                    tok().text
                                } else {
                                    tok().disabled_text
                                };
                                el.child(
                                    div()
                                        .w(px(14.))
                                        .flex()
                                        .items_center()
                                        .when(checked == Some(true), |el| {
                                            el.child(icon(Icon::Check, color))
                                        }),
                                )
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(label.clone()),
                            )
                            .when_some(shortcut, |el, keys| {
                                el.child(
                                    div()
                                        .pl_3()
                                        .text_size(px(dims().font_caption))
                                        .text_color(rgb(tok().muted))
                                        .whitespace_nowrap()
                                        .child(keys),
                                )
                            })
                            .when(submenu, |el| {
                                el.child(icon(Icon::ChevronRight, tok().muted))
                            })
                            .when(enabled, |el| {
                                el.cursor_pointer()
                                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                        if let Some(m) = &mut this.menu
                                            && (m.menu.index != top || m.menu.sub != sub)
                                        {
                                            m.menu.hover(top, sub);
                                            cx.notify();
                                        }
                                    }))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(outcome) =
                                            this.menu.as_mut().map(|m| m.menu.activate(top, sub))
                                        {
                                            this.menu_outcome(outcome, window, cx);
                                        }
                                    }))
                            })
                    }
                }
            }))
    }
}
