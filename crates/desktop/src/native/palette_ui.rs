//! Command Palette overlay, command dispatch and Vim-key routing for the library table.
use super::*;
use crate::{
    commands::{
        Availability, CommandContext, DisabledReason, ids, pdf_ids as pdf, workbench_ids as wb,
    },
    keymap::{self, InputState, KeyStroke, Mode, Resolution},
    palette,
};

/// Every non-deferred command this host dispatches. Deferred descriptors stay disabled.
pub(super) const CONNECTED: &[&str] = &[
    pdf::NEXT_PAGE,
    pdf::PREVIOUS_PAGE,
    pdf::FIRST_PAGE,
    pdf::LAST_PAGE,
    pdf::FIND,
    pdf::FIND_NEXT,
    pdf::FIND_PREVIOUS,
    pdf::ZOOM_IN,
    pdf::ZOOM_OUT,
    pdf::FIT_WIDTH,
    pdf::ROTATE,
    pdf::HIGHLIGHT,
    pdf::ADD_NOTE,
    pdf::SHOW_ANNOTATIONS,
    pdf::FULL_TEXT,
    pdf::COPY_LINK,
    pdf::OPEN_LINK,
    ids::NEXT,
    ids::PREVIOUS,
    ids::FIRST,
    ids::LAST,
    ids::OPEN,
    ids::SEARCH,
    ids::COPY,
    ids::RESTORE_FOCUS,
    ids::PALETTE,
    ids::NEW,
    ids::SAVE,
    ids::DOI,
    ids::IMPORT,
    wb::VIEW_LIBRARY,
    wb::VIEW_IMPORT_REVIEW,
    wb::VIEW_PDFS,
    wb::VIEW_ORGANIZE,
    wb::RELOAD,
    wb::CLEAR_FILTERS,
    wb::SHOW_TRASH,
    wb::SHOW_ALL,
    wb::TRASH,
    wb::DISCARD,
    wb::READING,
    wb::EXPORT,
    wb::TOGGLE_VIM,
    wb::RELOAD_KEYMAP,
    wb::COPY_DEFAULT_KEYMAP,
];

/// Platform shortcuts bound directly in GPUI (see `key_bindings`), shown next to commands.
fn fixed_shortcut(id: &str) -> Option<&'static str> {
    Some(match id {
        ids::NEW => "Cmd+n",
        ids::SAVE => "Cmd+s",
        ids::SEARCH => "Cmd+f",
        ids::PALETTE => "Cmd+k",
        pdf::FIND => "/",
        pdf::NEXT_PAGE => "J",
        pdf::PREVIOUS_PAGE => "K",
        pdf::HIGHLIGHT => "h",
        pdf::SHOW_ANNOTATIONS => "a",
        pdf::FIND_NEXT => "n",
        pdf::FIND_PREVIOUS => "N",
        ids::OPEN => "Enter",
        ids::NEXT => "Down",
        ids::PREVIOUS => "Up",
        ids::RESTORE_FOCUS => "Esc",
        wb::VIEW_LIBRARY => "Cmd+1",
        wb::VIEW_IMPORT_REVIEW => "Cmd+2",
        wb::VIEW_ORGANIZE => "Cmd+,",
        wb::RELOAD => "Cmd+r",
        _ => return None,
    })
}

/// `Cmd+k` -> `⌘K` on macOS, `Ctrl+K` elsewhere; `g g` stays a two-key sequence.
pub(super) fn display_keys(keys: &str) -> String {
    keys.split(' ')
        .map(|token| {
            let (mods, key) = match token.rfind('+') {
                Some(i) if i + 1 < token.len() => (&token[..=i], &token[i + 1..]),
                // `Cmd++`: the bound key is "+" itself.
                Some(i) if i > 0 && token.ends_with("++") => (&token[..i], "+"),
                _ => ("", token),
            };
            let key = match key {
                "Down" => "↓".to_owned(),
                "Up" => "↑".to_owned(),
                "Enter" => "↩".to_owned(),
                "Esc" => "esc".to_owned(),
                k if !mods.is_empty() => k.to_uppercase(),
                k => k.to_owned(),
            };
            let mods = if cfg!(target_os = "macos") {
                mods.replace("Cmd+", "⌘")
                    .replace("Ctrl+", "⌃")
                    .replace("Alt+", "⌥")
                    .replace("Shift+", "⇧")
            } else {
                mods.replace("Cmd+", "Ctrl+")
            };
            format!("{mods}{key}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn reason_text(availability: Availability) -> Option<String> {
    let Availability::Disabled(reason) = availability else {
        return None;
    };
    Some(match reason {
        DisabledReason::NotConnected | DisabledReason::NotRegistered => "Not available here".into(),
        DisabledReason::LibraryUnavailable => "Library is not ready".into(),
        DisabledReason::SelectionRequired => "Select a paper first".into(),
        DisabledReason::Busy => "Wait for the current operation".into(),
        DisabledReason::Deferred { issue, reason } => format!("{reason} (#{issue})"),
    })
}

impl LibraryView {
    pub(super) fn command_context(&self) -> CommandContext<'static> {
        CommandContext {
            connected_handlers: CONNECTED,
            library_ready: true,
            has_selection: self.editor.is_some(),
            busy: self.locked(),
        }
    }
    fn key_mode(&self) -> Mode {
        if self.settings.vim {
            Mode::Normal
        } else {
            Mode::VimOff
        }
    }
    pub(super) fn shortcut_label(&self, id: &str) -> Option<String> {
        self.keymap
            .shortcut(id, keymap::Context::Library, self.key_mode())
            .or_else(|| fixed_shortcut(id).map(str::to_owned))
            .map(|keys| display_keys(&keys))
    }
    /// Validate the user's keymap.json as a whole; any problem falls back to the defaults.
    pub(super) fn apply_keymap(&mut self, document: Option<&[u8]>, path: Option<&std::path::Path>) {
        self.keymap_path = path.map(|p| p.display().to_string());
        self.keymap_diagnostics.clear();
        let loaded = document.map(|bytes| keymap::load_or_default(bytes, &self.registry));
        self.keymap = match loaded {
            None => {
                self.keymap_custom = false;
                keymap::Keymap::defaults(&self.registry).expect("built-in keymap is valid")
            }
            Some(Ok(loaded)) => {
                self.keymap_custom = loaded.source == keymap::ConfigSource::User;
                self.keymap_diagnostics = loaded
                    .diagnostics
                    .iter()
                    .map(|d| match (d.line, d.column) {
                        (Some(line), Some(column)) => {
                            format!("{:?} at {} (line {line}, column {column})", d.code, d.path)
                        }
                        _ => format!("{:?} at {}", d.code, d.path),
                    })
                    .collect();
                loaded.keymap
            }
            Some(Err(_)) => unreachable!("built-in keymap is valid"),
        };
        self.resolver = keymap::KeyResolver::new(self.keymap.clone());
    }
    pub(super) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_open = true;
        self.palette_index = 0;
        self.palette_query
            .update(cx, |f, cx| f.set_text(String::new(), cx));
        self.palette_query.focus_handle(cx).focus(window, cx);
        cx.notify();
    }
    pub(super) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.palette_open {
            return;
        }
        self.palette_open = false;
        if self.screen == Screen::Library {
            self.list_focus().focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }
    fn palette_entries(&self, cx: &App) -> Vec<palette::Entry> {
        palette::rank(
            &self.registry,
            &self.command_context(),
            &self.palette_query.read(cx).content,
            &self.settings.recent_commands,
        )
    }
    pub(super) fn palette_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.palette_entries(cx).len();
        if count == 0 {
            return;
        }
        self.palette_index =
            (self.palette_index as isize + delta).rem_euclid(count as isize) as usize;
        self.palette_scroll.scroll_to_item(self.palette_index);
        cx.notify();
    }
    pub(super) fn palette_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.palette_entries(cx);
        let Some(entry) = entries.get(self.palette_index.min(entries.len().saturating_sub(1)))
        else {
            return;
        };
        let id = entry.id;
        if let Some(reason) = reason_text(entry.availability) {
            self.status(format!("{} — {reason}", entry.title), true, cx);
            return;
        }
        self.close_palette(window, cx);
        palette::record(&mut self.settings.recent_commands, id, &self.registry);
        self.persist(cx);
        self.run_command(id, window, cx);
    }
    /// Single dispatch point for palette and keymap requests. Availability is rechecked here.
    pub(super) fn run_command(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let availability = self.registry.availability(id, &self.command_context());
        if let Some(reason) = reason_text(availability) {
            let title = self.registry.get(id).map_or(id, |c| c.title);
            self.status(format!("{title} — {reason}"), true, cx);
            return;
        }
        self.resolver.reset();
        match id {
            ids::NEXT => self.move_selection(1, cx),
            ids::PREVIOUS => self.move_selection(-1, cx),
            ids::FIRST | ids::LAST => {
                if let Some(paper) = self.library.edge(id == ids::LAST) {
                    self.select(paper, false, cx);
                    if let Some(index) = self.library.index_of(paper) {
                        self.rows.scroll_to_item(index, ScrollStrategy::Nearest);
                    }
                }
            }
            ids::OPEN => {
                self.show(Screen::Library, window, cx);
                if self.settings.details_tab != "Info" {
                    self.settings.details_tab = "Info".into();
                    self.persist(cx);
                }
                self.focus_layout = (self.screen, self.settings.details_tab.clone());
                if let Some(field) = self.fields.first() {
                    field.focus_handle(cx).focus(window, cx);
                }
            }
            ids::SEARCH => self.find(&Find, window, cx),
            ids::COPY => self.copy_citation(cx),
            ids::RESTORE_FOCUS => self.back(&Back, window, cx),
            ids::PALETTE => self.open_palette(window, cx),
            ids::NEW => self.new_paper(&New, window, cx),
            ids::SAVE => {
                if self.screen != Screen::Library {
                    self.show(Screen::Library, window, cx);
                }
                self.save(&Save, window, cx);
            }
            ids::DOI => {
                self.show(Screen::Library, window, cx);
                self.settings.details_tab = "Info".into();
                self.focus_layout = (self.screen, self.settings.details_tab.clone());
                self.doi.focus_handle(cx).focus(window, cx);
                self.status("Enter a DOI and press ↩ to preview it", false, cx);
            }
            ids::IMPORT => {
                if !self.locked() {
                    self.pick_bibliography(Format::Bibtex, cx);
                }
            }
            wb::VIEW_LIBRARY => self.show(Screen::Library, window, cx),
            wb::VIEW_IMPORT_REVIEW => self.show(Screen::Import, window, cx),
            wb::VIEW_PDFS => self.open_pdfs(cx),
            wb::VIEW_ORGANIZE => self.show(Screen::Settings, window, cx),
            wb::RELOAD => self.reload(cx),
            wb::CLEAR_FILTERS => self.clear_filters(cx),
            wb::SHOW_TRASH | wb::SHOW_ALL => {
                self.library.query.filter.collection = None;
                self.library.query.filter.visibility = if id == wb::SHOW_TRASH {
                    PaperVisibility::Deleted
                } else {
                    PaperVisibility::Active
                };
                self.show(Screen::Library, window, cx);
                self.reload(cx);
            }
            wb::TRASH => self.toggle_trash(cx),
            wb::DISCARD => self.discard_edits(cx),
            wb::READING => {
                self.settings.reading = !self.settings.reading;
                self.persist(cx);
            }
            wb::EXPORT => {
                self.show(Screen::Settings, window, cx);
                self.status(
                    "Choose format and papers under Export bibliography, then Export",
                    false,
                    cx,
                );
            }
            wb::TOGGLE_VIM => {
                self.settings.vim = !self.settings.vim;
                self.persist(cx);
                self.status(
                    if self.settings.vim {
                        "Vim keys on: j/k move, gg/G jump, y copies a citation, / searches"
                    } else {
                        "Vim keys off: use arrows, ↩ and the shortcuts shown in the palette"
                    },
                    false,
                    cx,
                );
            }
            wb::RELOAD_KEYMAP => {
                self.call(Lane::LoadSettings, Event::LoadSettings, |c, done| {
                    c.load_settings(done)
                });
                self.status("Reloading settings and keymap…", false, cx);
            }
            wb::COPY_DEFAULT_KEYMAP => {
                cx.write_to_clipboard(ClipboardItem::new_string(
                    String::from_utf8_lossy(keymap::DEFAULT_JSON).into_owned(),
                ));
                let target = self
                    .keymap_path
                    .clone()
                    .unwrap_or_else(|| "keymap.json".into());
                self.status(
                    format!(
                        "Default keymap copied. Save it as {target} (mode 0600), then Reload Keymap"
                    ),
                    false,
                    cx,
                );
            }
            pdf::NEXT_PAGE | pdf::PREVIOUS_PAGE | pdf::FIRST_PAGE | pdf::LAST_PAGE => {
                self.show_pdf_tab(cx);
                if let Some(v) = &mut self.viewer {
                    let current = v.current_page();
                    v.jump(match id {
                        pdf::NEXT_PAGE => current + 1,
                        pdf::PREVIOUS_PAGE => current.saturating_sub(1),
                        pdf::FIRST_PAGE => 0,
                        _ => u32::MAX,
                    });
                }
            }
            pdf::FIND => {
                if self.host.selected.is_none() {
                    self.status("Open a PDF attachment first", true, cx);
                    return;
                }
                self.show_pdf_tab(cx);
                self.focus_layout = (self.screen, self.settings.details_tab.clone());
                self.find.open = true;
                self.find.input.focus_handle(cx).focus(window, cx);
            }
            pdf::FIND_NEXT | pdf::FIND_PREVIOUS => self.find_step(id == pdf::FIND_NEXT, cx),
            pdf::ZOOM_IN | pdf::ZOOM_OUT | pdf::FIT_WIDTH | pdf::ROTATE => {
                self.show_pdf_tab(cx);
                self.viewer_command(id, cx);
            }
            pdf::HIGHLIGHT => self.highlight_selection_command(cx),
            pdf::ADD_NOTE => {
                self.show_pdf_tab(cx);
                self.focus_layout = (self.screen, self.settings.details_tab.clone());
                self.ann.note_open = true;
                self.ann.note.focus_handle(cx).focus(window, cx);
            }
            pdf::SHOW_ANNOTATIONS => {
                self.show(Screen::Library, window, cx);
                self.settings.details_tab = "Notes".into();
                self.persist(cx);
            }
            pdf::FULL_TEXT => {
                self.show(Screen::Library, window, cx);
                self.find_search.fulltext = true;
                self.focus_layout = (self.screen, self.settings.details_tab.clone());
                self.search.focus_handle(cx).focus(window, cx);
                self.status("Full-text search: type and press ↩", false, cx);
            }
            pdf::COPY_LINK => self.copy_link(cx),
            pdf::OPEN_LINK => {
                let text = cx
                    .read_from_clipboard()
                    .and_then(|item| item.text())
                    .unwrap_or_default();
                self.open_link(&text, cx);
            }
            _ => self.status(format!("{id} is not connected"), true, cx),
        }
        cx.notify();
    }
    fn copy_citation(&mut self, cx: &mut Context<Self>) {
        let Some(paper) = &self.editor else {
            self.status("Select a paper to copy its citation", true, cx);
            return;
        };
        let text = crate::details::citation_text(&paper.title, &paper.bibliography);
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.status(format!("Copied: {text}"), false, cx);
    }
    /// Keys the table's GPUI bindings did not consume go through the user keymap
    /// (Vim Normal mode, or VimOff bindings). Text fields never reach this handler.
    pub(super) fn table_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette_open || self.screen != Screen::Library {
            return;
        }
        let m = &event.keystroke.modifiers;
        // Super/Windows (and Fn) are not part of the keymap vocabulary off macOS; never
        // let Super+j act as a bare j.
        if m.function || (!cfg!(target_os = "macos") && m.platform) {
            self.resolver.reset();
            return;
        }
        let (primary, control) = if cfg!(target_os = "macos") {
            (m.platform, m.control)
        } else {
            (m.control, false)
        };
        let Some(key) = KeyStroke::from_platform(
            &event.keystroke.key,
            event.keystroke.key_char.as_deref(),
            primary,
            control,
            m.alt,
            m.shift,
        ) else {
            self.resolver.reset();
            return;
        };
        let input = InputState {
            context: keymap::Context::Library,
            mode: self.key_mode(),
            editing: false,
            composing: false,
        };
        let now = self.started.elapsed().as_millis() as u64;
        let context = self.command_context();
        match self
            .resolver
            .resolve(&key, input, now, &self.registry, &context)
        {
            Resolution::PassThrough => {}
            Resolution::Pending | Resolution::Cancelled => cx.stop_propagation(),
            Resolution::Command { id, .. } => {
                cx.stop_propagation();
                self.run_command(&id, window, cx);
            }
        }
    }
    pub(super) fn palette_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entries = self.palette_entries(cx);
        let selected = self.palette_index.min(entries.len().saturating_sub(1));
        div()
            .id("palette-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(rgba(0x0f172a40))
            .flex()
            .justify_center()
            .items_start()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close_palette(window, cx)),
            )
            .child(
                div()
                    .id("palette")
                    .key_context("Palette")
                    .on_action(cx.listener(|this, _: &PaletteNext, _, cx| this.palette_move(1, cx)))
                    .on_action(
                        cx.listener(|this, _: &PalettePrevious, _, cx| this.palette_move(-1, cx)),
                    )
                    .on_action(cx.listener(|this, _: &PaletteClose, window, cx| {
                        this.close_palette(window, cx)
                    }))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .mt(px(72.))
                    .w(px(620.))
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(SURFACE))
                    .shadow_lg()
                    .overflow_hidden()
                    .child(
                        div()
                            .p_2()
                            .border_b_1()
                            .border_color(rgb(BORDER))
                            .child(self.palette_query.clone()),
                    )
                    .child(
                        div()
                            .id("palette-results")
                            .track_scroll(&self.palette_scroll)
                            .max_h(px(420.))
                            .overflow_y_scroll()
                            .py_1()
                            .when(entries.is_empty(), |el| {
                                el.child(div().p_3().child(hint("No matching commands")))
                            })
                            .children(entries.into_iter().enumerate().map(|(i, entry)| {
                                let disabled = reason_text(entry.availability);
                                let shortcut = self.shortcut_label(entry.id);
                                let id = entry.id;
                                div()
                                    .id(("palette-row", i))
                                    .mx_1()
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .cursor_pointer()
                                    .when(i == selected, |el| el.bg(rgb(ACCENT_SOFT)))
                                    .when(i != selected, |el| el.hover(|s| s.bg(rgb(0xf0f4fa))))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .child(
                                                div()
                                                    .text_color(rgb(if disabled.is_some() {
                                                        0x9aa3af
                                                    } else {
                                                        TEXT
                                                    }))
                                                    .font_weight(if i == selected {
                                                        FontWeight::SEMIBOLD
                                                    } else {
                                                        FontWeight::NORMAL
                                                    })
                                                    .child(entry.title),
                                            )
                                            .child(
                                                hint(disabled.unwrap_or_else(|| {
                                                    entry.description.to_owned()
                                                }))
                                                .whitespace_nowrap()
                                                .overflow_hidden()
                                                .text_ellipsis(),
                                            ),
                                    )
                                    .when_some(shortcut, |el, keys| {
                                        el.child(
                                            div()
                                                .px_2()
                                                .rounded_md()
                                                .bg(rgb(SIDEBAR))
                                                .text_size(px(11.))
                                                .text_color(rgb(MUTED))
                                                .whitespace_nowrap()
                                                .child(keys),
                                        )
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(index) =
                                            this.palette_entries(cx).iter().position(|e| e.id == id)
                                        {
                                            this.palette_index = index;
                                        }
                                        this.palette_confirm(window, cx);
                                    }))
                            })),
                    )
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .border_t_1()
                            .border_color(rgb(BORDER))
                            .child(hint(
                                "↑↓ choose · ↩ run · esc close · recent commands first",
                            )),
                    ),
            )
    }
}
