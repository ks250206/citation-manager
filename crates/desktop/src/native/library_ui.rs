use super::*;
impl LibraryView {
    pub(super) fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let enabled = !self.locked();
        div()
            .p_3()
            .flex()
            .items_center()
            .gap_2()
            .child(div().text_size(px(20.)).child("Citation Manager"))
            .child(
                button("library", "Library", true).on_click(cx.listener(|this, _, _, cx| {
                    this.screen = Screen::Library;
                    cx.notify();
                })),
            )
            .child(
                button("new", "New paper", enabled)
                    .on_click(cx.listener(|this, _, w, cx| this.new_paper(&New, w, cx))),
            )
            .child(
                button("bibtex", "Import BibTeX", enabled).on_click(cx.listener(
                    |this, _, _, cx| {
                        if !this.locked() {
                            this.pick_bibliography(Format::Bibtex, cx);
                        }
                    },
                )),
            )
            .child(
                button("ris", "Import RIS", enabled).on_click(cx.listener(|this, _, _, cx| {
                    if !this.locked() {
                        this.pick_bibliography(Format::Ris, cx);
                    }
                })),
            )
            .child(
                button("pdfs", "PDF files / folder", enabled).on_click(cx.listener(
                    |this, _, _, cx| {
                        if !this.locked() {
                            this.invalidate_doi();
                            this.load_pdf_form(this.pdf_row, cx);
                            this.screen = Screen::Pdf;
                            cx.notify();
                        }
                    },
                )),
            )
            .child(
                button("layout", "Layout / organize / export", enabled).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.screen = Screen::Settings;
                        cx.notify();
                    },
                )),
            )
            .child(
                button("review-tab", "Import review", true).on_click(cx.listener(
                    |this, _, _, cx| {
                        if this.locked() {
                            return;
                        }
                        let cancelled = this.invalidate_doi();
                        this.screen = Screen::Import;
                        if cancelled {
                            this.status(
                                "DOI lookup cancelled; retained import review is unchanged",
                                false,
                                cx,
                            );
                        } else {
                            cx.notify();
                        }
                    },
                )),
            )
            .child(
                button("reload", "Reload", !self.busy(Lane::Query))
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            )
    }
    fn filter(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn library_screen(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .flex()
                    .gap_2()
                    .child(div().flex_1().child(self.search.clone()))
                    .child(div().w(px(140.)).child(self.author.clone()))
                    .child(div().w(px(90.)).child(self.year_from.clone()))
                    .child(div().w(px(90.)).child(self.year_to.clone()))
                    .child(div().w(px(110.)).child(self.paper_type.clone()))
                    .child(
                        button("apply", "Search / filter", true)
                            .on_click(cx.listener(|this, _, _, cx| this.filter(cx))),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .pb_2()
                    .flex()
                    .gap_2()
                    .child(line(
                        "Metadata only · literal keywords · direct collection membership",
                    ))
                    .child(
                        button(
                            "has-pdf",
                            format!(
                                "PDF: {}",
                                match self.library.query.filter.has_pdf {
                                    None => "Any",
                                    Some(true) => "Attached",
                                    Some(false) => "None",
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
                        button("reset-filter", "Clear filters", true).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.library.query.filter = LibraryFilter::default();
                                for field in [
                                    &this.search,
                                    &this.author,
                                    &this.year_from,
                                    &this.year_to,
                                    &this.paper_type,
                                ] {
                                    field.update(cx, |f, cx| f.set_text(String::new(), cx));
                                }
                                this.reload(cx);
                            },
                        )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(!self.settings.reading, |el| el.child(self.navigation(cx)))
                    .when(!self.settings.reading, |el| el.child(self.table(cx)))
                    .child(self.details_screen(cx)),
            )
    }
    fn navigation(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let filter = &self.library.query.filter;
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
            .w(px(self.settings.navigation_width as f32))
            .flex_shrink_0()
            .p_3()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(rgb(0xd5dce5))
            .child(
                button("all", "All Papers", true).on_click(cx.listener(|this, _, _, cx| {
                    this.library.query.filter.collection = None;
                    this.library.query.filter.visibility =
                        cm_protocol::library_query::PaperVisibility::Active;
                    this.reload(cx);
                })),
            )
            .child(
                button("trash", "Trash / restore", true).on_click(cx.listener(|this, _, _, cx| {
                    this.library.query.filter.visibility =
                        cm_protocol::library_query::PaperVisibility::Deleted;
                    this.reload(cx);
                })),
            )
            .child(line("Collections"))
            .children(ordered.into_iter().enumerate().map(|(i, (c, depth))| {
                let selected = filter.collection == Some(c.id);
                let id = c.id;
                button(
                    ("collection", i),
                    format!("{}{}", if selected { "▸ " } else { "" }, c.name),
                    true,
                )
                .w_full()
                .pl(px(12. + depth as f32 * 16.))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.library.query.filter.collection = Some(id);
                    this.reload(cx);
                }))
            }))
            .child(line("Tags"))
            .child(
                button(
                    "tag-mode",
                    format!(
                        "Match tags: {}",
                        if filter.tag_match == TagMatch::All {
                            "AND"
                        } else {
                            "OR"
                        }
                    ),
                    true,
                )
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
            .children(self.organization.tags.iter().enumerate().map(|(i, t)| {
                let id = t.id;
                button(
                    ("tag", i),
                    format!(
                        "{} {}",
                        if filter.tags.contains(&id) {
                            "☑"
                        } else {
                            "☐"
                        },
                        t.name
                    ),
                    true,
                )
                .w_full()
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
    fn table(&self, cx: &mut Context<Self>) -> Div {
        let columns = self.settings.columns.clone();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(div().px_3().py_2().child(format!(
                "Page {} · {} rows · {} selected{}",
                self.library.previous.len() + 1,
                self.library.papers.len(),
                self.library.selected.len(),
                if self.busy(Lane::Query) {
                    " · Loading…"
                } else {
                    ""
                }
            )))
            .child(
                div()
                    .id("columns")
                    .overflow_x_scroll()
                    .flex_shrink_0()
                    .child(
                        div().flex().children(
                            columns
                                .iter()
                                .enumerate()
                                .filter(|(_, c)| c.visible)
                                .map(|(i, c)| {
                                    button(("column", i), c.name.clone(), true)
                                        .w(px(c.width as f32))
                                        .flex_shrink_0()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if i >= 2 {
                                                return;
                                            }
                                            let field = match i {
                                                0 => SortField::Title,
                                                1 => SortField::Year,
                                                _ => SortField::Id,
                                            };
                                            if this.library.query.sort.field == field {
                                                this.library.query.sort.direction =
                                                    if this.library.query.sort.direction
                                                        == SortDirection::Asc
                                                    {
                                                        SortDirection::Desc
                                                    } else {
                                                        SortDirection::Asc
                                                    };
                                            } else {
                                                this.library.query.sort = LibrarySort {
                                                    field,
                                                    direction: SortDirection::Asc,
                                                };
                                            }
                                            this.settings.sort = this.library.query.sort;
                                            this.persist(cx);
                                            this.reload(cx);
                                        }))
                                }),
                        ),
                    ),
            )
            .when(self.library.papers.is_empty(), |el| {
                el.child(div().p_4().child(if self.busy(Lane::Query) {
                    "Loading library…"
                } else {
                    "No matching papers. Clear filters, add a paper, or reload."
                }))
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
                                let b = &p.bibliography;
                                let values = [
                                    p.title.replace(['\r', '\n'], " "),
                                    b.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into()),
                                    b.paper_type.clone().unwrap_or_default(),
                                    b.doi.clone().unwrap_or_default(),
                                ];
                                div()
                                    .id(index)
                                    .h(px(42.))
                                    .flex()
                                    .items_center()
                                    .bg(rgb(if selected { 0xe0eaff } else { 0xffffff }))
                                    .border_b_1()
                                    .border_color(rgb(0xe4e8ee))
                                    .overflow_hidden()
                                    .cursor_pointer()
                                    .children(
                                        columns.iter().zip(values).filter(|(c, _)| c.visible).map(
                                            |(c, v)| {
                                                div()
                                                    .w(px(c.width as f32))
                                                    .flex_shrink_0()
                                                    .px_3()
                                                    .overflow_hidden()
                                                    .text_ellipsis()
                                                    .child(v)
                                            },
                                        ),
                                    )
                                    .on_click(cx.listener(
                                        move |this, event: &ClickEvent, _, cx| {
                                            this.select(
                                                id,
                                                event.modifiers().platform
                                                    || event.modifiers().shift,
                                                cx,
                                            )
                                        },
                                    ))
                            })
                            .collect()
                    }),
                )
                .flex_1()
                .min_h_0(),
            )
            .child(
                div()
                    .p_2()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            "previous",
                            "Previous page",
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
                            "Next page",
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
