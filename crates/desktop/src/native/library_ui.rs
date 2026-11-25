use super::*;
impl LibraryView {
    pub(super) fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let enabled = !self.locked();
        let tab = |id: &'static str, label: &'static str, screen: Screen| {
            nav_item(id, label, self.screen == screen).px_3()
        };
        // Organize & export and API & integrations open from ⚙ (and the palette); while one
        // is shown its name stays visible here so the top bar still says where you are.
        let settings_screen = match self.screen {
            Screen::Settings => Some("Organize & export"),
            Screen::Api => Some("API & integrations"),
            _ => None,
        };
        let menu_open = |kind| self.menu.as_ref().is_some_and(|m| m.kind == kind);
        div()
            .px_4()
            .py_2()
            .flex()
            .items_center()
            .gap_2()
            .bg(rgb(SURFACE))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .mr_2()
                    .child("Citation Manager"),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .p(px(2.))
                    .rounded_md()
                    .bg(rgb(SIDEBAR))
                    .child(
                        tab("library", "Library", Screen::Library).on_click(
                            cx.listener(|this, _, w, cx| this.show(Screen::Library, w, cx)),
                        ),
                    )
                    .child(
                        tab(
                            "review-tab",
                            if self.reviews.needs_attention() {
                                "Import review •"
                            } else {
                                "Import review"
                            },
                            Screen::Import,
                        )
                        .on_click(cx.listener(|this, _, w, cx| this.show(Screen::Import, w, cx))),
                    )
                    .child(
                        tab("pdfs", "PDFs", Screen::Pdf)
                            .on_click(cx.listener(|this, _, _, cx| this.open_pdfs(cx))),
                    )
                    .when_some(settings_screen, |el, name| {
                        el.child(nav_item("settings-screen", name, true).px_3())
                    }),
            )
            .child(div().flex_1())
            .child(
                primary_button("add-menu", "＋ Add ▾", enabled)
                    .when(menu_open(menu_ui::MenuKind::Add), |b| b.bg(rgb(0x2654a8)))
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        let position = point(event.position().x - px(40.), px(58.));
                        this.open_menu(menu_ui::MenuKind::Add, position, window, cx);
                    })),
            )
            .child(
                button("settings-menu", "⚙ Settings ▾", true)
                    .when(
                        settings_screen.is_some() || menu_open(menu_ui::MenuKind::Settings),
                        |b| b.bg(rgb(ACCENT_SOFT)),
                    )
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        let position = point(event.position().x - px(60.), px(58.));
                        this.open_menu(menu_ui::MenuKind::Settings, position, window, cx);
                    })),
            )
            .child(
                button("reload", "↻", !self.busy(Lane::Query))
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            )
    }
    /// Where the ＋ Add menu opens from the palette: under the toolbar button.
    pub(super) fn add_menu_position(&self, window: &Window) -> Point<Pixels> {
        point(window.viewport_size().width - px(330.), px(58.))
    }
    /// Header click / palette sort command. Keyset paging restarts under the new order.
    pub(super) fn sort_by(&mut self, field: SortField, cx: &mut Context<Self>) {
        let sort = crate::columns::next_sort(self.library.query.sort, field);
        self.library.query.sort = sort;
        self.settings.sort = sort;
        self.persist(cx);
        self.reload(cx);
        self.status(
            format!("Sorted by {}", crate::columns::sort_label(sort)),
            false,
            cx,
        );
    }
    pub(super) fn open_pdfs(&mut self, cx: &mut Context<Self>) {
        if !self.locked() {
            self.invalidate_doi();
            self.load_pdf_form(self.pdf_row, cx);
            self.screen = Screen::Pdf;
            cx.notify();
        }
    }
    /// Enter/Search: full-text search when "Full text" is on, else the metadata filter.
    pub(super) fn search_or_filter(&mut self, cx: &mut Context<Self>) {
        if self.find_search.fulltext {
            self.run_fulltext(cx);
        } else {
            self.filter(cx);
        }
    }
    pub(super) fn clear_filters(&mut self, cx: &mut Context<Self>) {
        self.library.query.filter = LibraryFilter {
            visibility: self.library.query.filter.visibility,
            ..LibraryFilter::default()
        };
        for field in [
            &self.search,
            &self.author,
            &self.year_from,
            &self.year_to,
            &self.paper_type,
        ] {
            field.update(cx, |f, cx| f.set_text(String::new(), cx));
        }
        self.reload(cx);
    }
    pub(super) fn filter(&mut self, cx: &mut Context<Self>) {
        let text = |field: &Entity<TextInput>| {
            let text = field.read(cx).content.to_string();
            if text.trim().is_empty() {
                None
            } else {
                Some(text)
            }
        };
        let mut filter = self.library.query.filter.clone();
        filter.keyword = text(&self.search);
        filter.author = text(&self.author);
        filter.paper_type = text(&self.paper_type);
        let year = |field: &Entity<TextInput>| -> Result<Option<i32>, ServiceError> {
            text(field)
                .map(|s| {
                    s.parse().map_err(|_| {
                        ServiceError::InvalidInput("Year filters require numbers".into())
                    })
                })
                .transpose()
        };
        match (year(&self.year_from), year(&self.year_to)) {
            (Ok(from), Ok(to)) => {
                filter.year_from = from;
                filter.year_to = to;
            }
            _ => {
                self.status("Year filters require numbers", true, cx);
                return;
            }
        }
        let query = LibraryQuery {
            filter: filter.clone(),
            sort: self.library.query.sort,
            after: None,
            limit: 100,
        };
        if let Err(e) = query.validate() {
            self.status(e.to_string(), true, cx);
            return;
        }
        self.library.query = query;
        self.reload(cx);
    }
    pub(super) fn library_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let details = self.details_screen(window, cx);
        let filter = &self.library.query.filter;
        let filtered = filter.keyword.is_some()
            || filter.author.is_some()
            || filter.year_from.is_some()
            || filter.year_to.is_some()
            || filter.paper_type.is_some()
            || filter.has_pdf.is_some()
            || filter.collection.is_some()
            || !filter.tags.is_empty();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .when(!self.settings.reading, |el| el.child(self.navigation(cx)))
            .when(!self.settings.reading, |el| el.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px_3()
                            .pt_3()
                            .pb_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().min_w(px(160.)).child(self.search.clone()))
                            .child(div().w(px(130.)).child(self.author.clone()))
                            .child(div().w(px(88.)).child(self.year_from.clone()))
                            .child(div().w(px(88.)).child(self.year_to.clone()))
                            .child(div().w(px(100.)).child(self.paper_type.clone()))
                            .child(
                                primary_button("apply", "Search", true)
                                    .on_click(cx.listener(|this, _, _, cx| this.search_or_filter(cx))),
                            )
                            .child(
                                nav_item("fulltext-mode", "Full text", self.find_search.fulltext)
                                    .flex_none()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.find_search.fulltext = !this.find_search.fulltext;
                                        if this.find_search.fulltext {
                                            this.run_fulltext(cx);
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .px_3()
                            .pb_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                button(
                                    "has-pdf",
                                    format!(
                                        "PDF: {}",
                                        match filter.has_pdf {
                                            None => "any",
                                            Some(true) => "attached",
                                            Some(false) => "none",
                                        }
                                    ),
                                    true,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.library.query.filter.has_pdf =
                                        match this.library.query.filter.has_pdf {
                                            None => Some(true),
                                            Some(true) => Some(false),
                                            Some(false) => None,
                                        };
                                    this.reload(cx);
                                })),
                            )
                            .child(
                                button("reset-filter", "Clear filters", filtered).on_click(
                                    cx.listener(|this, _, _, cx| this.clear_filters(cx)),
                                ),
                            )
                            .child(
                                div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(hint(
                                    if self.find_search.fulltext {
                                        "↩ full text: titles, abstracts, notes, PDF text · \"phrase\" tag: year: doi: collection:"
                                    } else {
                                        "↩ to search · metadata only · literal keywords · direct collection membership"
                                    },
                                )),
                            ),
                    )
                    .when(!self.find_search.fulltext, |el| el.child(self.table(cx)))
                    .when(self.find_search.fulltext, |el| el.child(self.search_results(cx))),
            ))
            .child(details)
    }
    fn navigation(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let filter = &self.library.query.filter;
        let trash = filter.visibility == cm_protocol::library_query::PaperVisibility::Deleted;
        let mut ordered = Vec::new();
        fn walk(
            org: &Organization,
            parent: Option<CollectionId>,
            depth: usize,
            out: &mut Vec<(Collection, usize)>,
        ) {
            if depth > 128 {
                return;
            }
            for item in org.collections.iter().filter(|c| c.parent == parent) {
                out.push((item.clone(), depth));
                walk(org, Some(item.id), depth + 1, out);
            }
        }
        walk(&self.organization, None, 0, &mut ordered);
        div()
            .id("navigation")
            .h_full()
            .w(px(self.settings.navigation_width as f32))
            .flex_shrink_0()
            .p_2()
            .flex()
            .flex_col()
            .gap(px(2.))
            .overflow_y_scroll()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(BORDER))
            .child(section("LIBRARY"))
            .child(
                nav_item(
                    "all",
                    "📚  All papers",
                    !trash && filter.collection.is_none(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.library.query.filter.collection = None;
                    this.library.query.filter.visibility =
                        cm_protocol::library_query::PaperVisibility::Active;
                    this.reload(cx);
                })),
            )
            .child(
                nav_item("trash", "🗑  Trash", trash).on_click(cx.listener(|this, _, _, cx| {
                    this.library.query.filter.collection = None;
                    this.library.query.filter.visibility =
                        cm_protocol::library_query::PaperVisibility::Deleted;
                    this.reload(cx);
                })),
            )
            .child(section("COLLECTIONS"))
            .when(ordered.is_empty(), |el| {
                el.child(div().px_2().child(hint(
                    "None yet — create one in ⚙ Settings › Organize & export",
                )))
            })
            .children(ordered.into_iter().enumerate().map(|(i, (c, depth))| {
                let id = c.id;
                nav_item(
                    ("collection", i),
                    format!("📁  {}", c.name),
                    filter.collection == Some(id),
                )
                .pl(px(8. + depth as f32 * 14.))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.library.query.filter.collection = Some(id);
                    this.library.query.filter.visibility =
                        cm_protocol::library_query::PaperVisibility::Active;
                    this.reload(cx);
                }))
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(div().flex_1().child(section("TAGS")))
                    .when(!self.organization.tags.is_empty(), |el| {
                        el.child(
                            nav_item(
                                "tag-mode",
                                if filter.tag_match == TagMatch::All {
                                    "match all"
                                } else {
                                    "match any"
                                },
                                false,
                            )
                            .mt_2()
                            .text_size(px(11.))
                            .text_color(rgb(ACCENT_TEXT))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.library.query.filter.tag_match =
                                    if this.library.query.filter.tag_match == TagMatch::All {
                                        TagMatch::Any
                                    } else {
                                        TagMatch::All
                                    };
                                this.reload(cx);
                            })),
                        )
                    }),
            )
            .when(self.organization.tags.is_empty(), |el| {
                el.child(div().px_2().child(hint("None yet")))
            })
            .children(self.organization.tags.iter().enumerate().map(|(i, t)| {
                let id = t.id;
                let on = filter.tags.contains(&id);
                nav_item(
                    ("tag", i),
                    format!("{} #{}", if on { "☑" } else { "☐" }, t.name),
                    on,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(n) = this.library.query.filter.tags.iter().position(|t| *t == id) {
                        this.library.query.filter.tags.remove(n);
                    } else {
                        this.library.query.filter.tags.push(id);
                    }
                    this.reload(cx);
                }))
            }))
    }
    fn table(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        use crate::columns::{self, ColumnKind};
        let visible = columns::visible(&self.settings.columns);
        let sort = self.library.query.sort;
        let focused_table = self.table_focus.clone();
        let cell = |kind: ColumnKind, width: u16| {
            // Title absorbs spare width so rows span the table. When the pane is narrow the
            // text columns give way (down to their minimum), so Year and PDF stay on screen.
            div()
                .flex_basis(px(width as f32))
                .when(kind.flexible(), |el| el.flex_grow_1())
                .map(|el| match kind.min_width() {
                    Some(min) => el.flex_shrink_1().min_w(px(min.min(width) as f32)),
                    None => el.flex_shrink_0(),
                })
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .map(|el| match kind {
                    // Narrow badge column: centred, little padding.
                    ColumnKind::Pdf => el.px_1().flex().justify_center(),
                    _ => el.px_3(),
                })
        };
        div()
            .id("paper-table")
            .track_focus(&self.table_focus)
            .key_context("PaperTable")
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::edit_selected))
            .on_action(cx.listener(|this, _: &ExtendNext, _, cx| this.extend_selection(1, cx)))
            .on_action(cx.listener(|this, _: &ExtendPrevious, _, cx| this.extend_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectAll, window, cx| {
                this.run_command(crate::commands::ids::SELECT_ALL, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowActions, window, cx| {
                this.run_command(crate::commands::ids::ACTIONS, window, cx)
            }))
            .on_action(cx.listener(|this, _: &TrashSelected, window, cx| {
                this.run_command(crate::commands::workbench_ids::TRASH, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyCitation, window, cx| {
                this.run_command(crate::commands::ids::COPY, window, cx)
            }))
            .on_key_down(cx.listener(Self::table_key))
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .mx_3()
            .mb_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .overflow_hidden()
            .child(
                div()
                    .id("columns")
                    .overflow_x_scroll()
                    .flex_shrink_0()
                    .bg(rgb(0xf8f9fb))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    // Right-click a header for the column toggles.
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.open_menu(menu_ui::MenuKind::Columns, event.position, window, cx)
                        }),
                    )
                    .child(div().flex().w_full().children(visible.iter().enumerate().map(
                        |(i, &(kind, width))| {
                            let field = kind.sort_field();
                            let active = field.is_some_and(|f| f == sort.field);
                            cell(kind, width)
                                .id(("column", i))
                                .py_1()
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(if active { ACCENT_TEXT } else { MUTED }))
                                .child(format!("{}{}", kind.name(), columns::sort_arrow(kind, sort)))
                                .when_some(field, |el, field| {
                                    el.cursor_pointer()
                                        .hover(|s| s.bg(rgb(0xeef1f5)))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.sort_by(field, cx)
                                        }))
                                })
                        },
                    ))),
            )
            .when(self.library.papers.is_empty(), |el| {
                let trash = self.library.query.filter.visibility
                    == cm_protocol::library_query::PaperVisibility::Deleted;
                el.child(
                    div()
                        .p_6()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .text_color(rgb(MUTED))
                        .child(if self.busy(Lane::Query) {
                            "Loading library…"
                        } else if trash {
                            "Trash is empty."
                        } else {
                            "No matching papers."
                        })
                        .when(!self.busy(Lane::Query) && !trash, |el| {
                            el.child(hint(format!(
                                "Add one from ＋ Add (new paper: {}, DOI, PDFs, BibTeX/RIS), or clear filters.",
                                shortcut("N")
                            )))
                        }),
                )
            })
            .child(
                uniform_list(
                    "paper-rows",
                    self.library.papers.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                let p = &this.library.papers[index];
                                let id = p.id;
                                let selected = this.library.selected.contains(&id);
                                let has_pdf = this.library.has_pdf(id);
                                let focus = focused_table.clone();
                                let menu_focus = focused_table.clone();
                                div()
                                    .id(index)
                                    .w_full()
                                    .h(px(32.))
                                    .flex()
                                    .items_center()
                                    .bg(rgb(if selected {
                                        ACCENT_SOFT
                                    } else if index % 2 == 1 {
                                        0xfafbfc
                                    } else {
                                        SURFACE
                                    }))
                                    .when(selected, |el| el.text_color(rgb(ACCENT_TEXT)))
                                    .when(!selected, |el| el.hover(|s| s.bg(rgb(0xf0f4fa))))
                                    .overflow_hidden()
                                    .cursor_pointer()
                                    .children(visible.iter().map(|&(kind, width)| {
                                        cell(kind, width)
                                            .when(kind != ColumnKind::Title, |el| {
                                                el.text_color(rgb(if selected {
                                                    ACCENT_TEXT
                                                } else {
                                                    MUTED
                                                }))
                                            })
                                            .map(|el| match kind {
                                                // A small badge reads better than a glyph.
                                                ColumnKind::Pdf if has_pdf => el.child(
                                                    div()
                                                        .px_1()
                                                        .rounded_sm()
                                                        .bg(rgb(0xfde8e4))
                                                        .text_color(rgb(0xb42318))
                                                        .text_size(px(10.))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child("PDF"),
                                                ),
                                                _ => el.child(kind.cell(p, has_pdf)),
                                            })
                                    }))
                                    .on_click(cx.listener(
                                        move |this, event: &ClickEvent, window, cx| {
                                            focus.focus(window, cx);
                                            let modifiers = event.modifiers();
                                            this.select(
                                                id,
                                                crate::library::ClickKind {
                                                    toggle: modifiers.secondary(cx),
                                                    range: modifiers.shift,
                                                },
                                                cx,
                                            )
                                        },
                                    ))
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener(
                                            move |this, event: &MouseDownEvent, window, cx| {
                                                menu_focus.focus(window, cx);
                                                // Right-clicking outside the selection acts on
                                                // that row alone, like other file lists; inside
                                                // it, the clicked row becomes the focused one.
                                                this.change_selection(cx, |library| {
                                                    library.focus_within(id)
                                                });
                                                if this.library.selected.contains(&id) {
                                                    this.open_menu(
                                                        menu_ui::MenuKind::Row,
                                                        event.position,
                                                        window,
                                                        cx,
                                                    );
                                                }
                                            },
                                        ),
                                    )
                            })
                            .collect()
                    }),
                )
                .track_scroll(&self.rows)
                .flex_1()
                .min_h_0(),
            )
            .child(
                div()
                    .px_3()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(0xf8f9fb))
                    .child(
                        // #98's page summary (paper/papers, selection) plus #84's sort label.
                        hint(format!(
                            "{} · {}{}",
                            self.library.page_summary(false),
                            columns::sort_label(sort),
                            if self.busy(Lane::Query) {
                                " · loading…"
                            } else {
                                ""
                            }
                        ))
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap(),
                    )
                    .child(button("columns-menu", "Columns…", true).on_click(cx.listener(
                        |this, event: &ClickEvent, window, cx| {
                            let position = point(event.position().x - px(120.), event.position().y - px(250.));
                            this.open_menu(menu_ui::MenuKind::Columns, position, window, cx)
                        },
                    )))
                    .child(
                        button(
                            "previous",
                            "‹ Previous",
                            !self.library.previous.is_empty() && !self.busy(Lane::Query),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.busy(Lane::Query)
                                && let Some(query) = this.library.previous_page()
                            {
                                this.call(Lane::Query, Event::Query, |c, done| {
                                    c.query(query, done)
                                });
                                cx.notify();
                            }
                        })),
                    )
                    .child(
                        button(
                            "next",
                            "Next ›",
                            self.library.next.is_some() && !self.busy(Lane::Query),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.busy(Lane::Query)
                                && let Some(query) = this.library.next_page()
                            {
                                this.call(Lane::Query, Event::Query, |c, done| {
                                    c.query(query, done)
                                });
                                cx.notify();
                            }
                        })),
                    ),
            )
    }
}
