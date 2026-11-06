use super::*;
impl LibraryView {
    pub(super) fn settings_screen(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let enabled = !self.locked() && !self.busy(Lane::Organization);
        div()
            .id("settings")
            .flex_1()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(line(
                "Table and panes — saved automatically in this private library",
            ))
            .children(self.settings.columns.iter().enumerate().map(|(i, c)| {
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            ("visible", i),
                            format!("{} {}", if c.visible { "☑" } else { "☐" }, c.name),
                            i != 0,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if i != 0 {
                                this.settings.columns[i].visible =
                                    !this.settings.columns[i].visible;
                                this.persist(cx);
                            }
                        })),
                    )
                    .child(button(("narrow", i), "− 20 px", true).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.settings.columns[i].width =
                                this.settings.columns[i].width.saturating_sub(20).max(50);
                            this.persist(cx);
                        },
                    )))
                    .child(line(format!("{} px", c.width)))
                    .child(button(("wide", i), "+ 20 px", true).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.settings.columns[i].width =
                                (this.settings.columns[i].width + 20).min(800);
                            this.persist(cx);
                        },
                    )))
            }))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            "navigation-width",
                            format!(
                                "Navigation width {} px — cycle",
                                self.settings.navigation_width
                            ),
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings.navigation_width =
                                if this.settings.navigation_width >= 340 {
                                    160
                                } else {
                                    this.settings.navigation_width + 20
                                };
                            this.persist(cx);
                        })),
                    )
                    .child(
                        button(
                            "details-width",
                            format!("Details width {} px — cycle", self.settings.details_width),
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings.details_width = if this.settings.details_width >= 660 {
                                300
                            } else {
                                this.settings.details_width + 40
                            };
                            this.persist(cx);
                        })),
                    )
                    .child(
                        button("reset-layout", "Reset layout", true).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.settings = Settings::default();
                                this.library.query.sort = this.settings.sort;
                                this.persist(cx);
                                this.reload(cx);
                            },
                        )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .children(
                        [
                            SortField::Id,
                            SortField::Title,
                            SortField::Year,
                            SortField::CreatedAt,
                            SortField::UpdatedAt,
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(i, field)| {
                            button(("sort", i), format!("Sort {:?}", field), true).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.settings.sort.field = field;
                                    this.library.query.sort = this.settings.sort;
                                    this.persist(cx);
                                    this.reload(cx);
                                }),
                            )
                        }),
                    )
                    .child(
                        button(
                            "direction",
                            format!("{:?}", self.settings.sort.direction),
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings.sort.direction =
                                if this.settings.sort.direction == SortDirection::Asc {
                                    SortDirection::Desc
                                } else {
                                    SortDirection::Asc
                                };
                            this.library.query.sort = this.settings.sort;
                            this.persist(cx);
                            this.reload(cx);
                        })),
                    ),
            )
            .child(line("Collections and tags"))
            .child(div().w(px(420.)).child(self.name.clone()))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            "parent",
                            format!(
                                "Parent: {} — cycle",
                                self.parent
                                    .and_then(|id| self
                                        .organization
                                        .collections
                                        .iter()
                                        .find(|c| c.id == id))
                                    .map(|c| c.name.as_str())
                                    .unwrap_or("Root")
                            ),
                            enabled,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let next = this
                                .parent
                                .and_then(|id| {
                                    this.organization
                                        .collections
                                        .iter()
                                        .position(|c| c.id == id)
                                })
                                .map_or(0, |i| i + 1);
                            this.parent = this.organization.collections.get(next).map(|c| c.id);
                            cx.notify();
                        })),
                    )
                    .child(
                        button(
                            "collection-save",
                            if self.editing_collection.is_some() {
                                "Save collection"
                            } else {
                                "Create collection"
                            },
                            enabled,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let input = CollectionInput {
                                name: this.name.read(cx).content.to_string(),
                                parent: this.parent,
                                sort_order: 0,
                            };
                            let request = if let Some(id) = this.editing_collection {
                                OrganizationRequest::UpdateCollection(id, input)
                            } else {
                                OrganizationRequest::CreateCollection(input)
                            };
                            this.organize(request, cx);
                        })),
                    )
                    .child(
                        button(
                            "tag-save",
                            if self.editing_tag.is_some() {
                                "Save tag"
                            } else {
                                "Create tag"
                            },
                            enabled,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let input = TagInput::named(this.name.read(cx).content.to_string());
                            let request = if let Some(id) = this.editing_tag {
                                OrganizationRequest::UpdateTag(id, input)
                            } else {
                                OrganizationRequest::CreateTag(input)
                            };
                            this.organize(request, cx);
                        })),
                    )
                    .child(
                        button("new-name", "New collection / tag", enabled).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.editing_collection = None;
                                this.editing_tag = None;
                                this.parent = None;
                                this.name.update(cx, |f, cx| f.set_text(String::new(), cx));
                                cx.notify();
                            },
                        )),
                    ),
            )
            .children(
                self.organization
                    .collections
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let id = c.id;
                        div()
                            .flex()
                            .gap_2()
                            .child(line(format!("Collection: {}", c.name)))
                            .child(
                                button(("edit-collection", i), "Edit / move", enabled).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        if let Some(c) = this
                                            .organization
                                            .collections
                                            .iter()
                                            .find(|c| c.id == id)
                                        {
                                            this.editing_collection = Some(id);
                                            this.editing_tag = None;
                                            this.parent = c.parent;
                                            let name = c.name.clone();
                                            this.name.update(cx, |f, cx| f.set_text(name, cx));
                                            cx.notify();
                                        }
                                    }),
                                ),
                            )
                            .child(
                                button(("delete-collection", i), "Delete collection only", enabled)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.organize(OrganizationRequest::DeleteCollection(id), cx)
                                    })),
                            )
                    }),
            )
            .children(self.organization.tags.iter().enumerate().map(|(i, t)| {
                let id = t.id;
                div()
                    .flex()
                    .gap_2()
                    .child(line(format!("Tag: {}", t.name)))
                    .child(
                        button(("edit-tag", i), "Rename", enabled).on_click(cx.listener(
                            move |this, _, _, cx| {
                                if let Some(t) = this.organization.tags.iter().find(|t| t.id == id)
                                {
                                    this.editing_tag = Some(id);
                                    this.editing_collection = None;
                                    let name = t.name.clone();
                                    this.name.update(cx, |f, cx| f.set_text(name, cx));
                                    cx.notify();
                                }
                            },
                        )),
                    )
                    .child(
                        button(("delete-tag", i), "Delete tag only", enabled).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.organize(OrganizationRequest::DeleteTag(id), cx)
                            }),
                        ),
                    )
            }))
            .child(line(
                "Export bibliography — PDFs, annotations and organization are not transferred",
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            "export-format",
                            format!("Format: {:?}", self.export_format),
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.export_format = if this.export_format == Format::Bibtex {
                                Format::Ris
                            } else {
                                Format::Bibtex
                            };
                            cx.notify();
                        })),
                    )
                    .child(
                        button(
                            "export-scope",
                            format!(
                                "Scope: {}",
                                [
                                    "All active papers",
                                    "Selected papers",
                                    "Current direct collection"
                                ][self.export_scope]
                            ),
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.export_scope = (this.export_scope + 1) % 3;
                            cx.notify();
                        })),
                    )
                    .child(
                        button("export", "Export to new file…", !self.busy(Lane::Export))
                            .on_click(cx.listener(|this, _, _, cx| this.pick_export(cx))),
                    ),
            )
    }
    pub(super) fn pick_export(&mut self, cx: &mut Context<Self>) {
        if self.busy(Lane::Export) {
            return;
        }
        let selection = match self.export_scope {
            0 => ExportSelection::All,
            1 => ExportSelection::Ids(self.library.selected.clone()),
            _ => {
                let Some(id) = self.library.query.filter.collection else {
                    self.status(
                        "Select a collection before exporting its direct members",
                        true,
                        cx,
                    );
                    return;
                };
                ExportSelection::Collection(id)
            }
        };
        let format = self.export_format;
        let picker = cx.prompt_for_new_path(
            std::path::Path::new("/private/tmp"),
            Some(if format == Format::Bibtex {
                "bibliography.bib"
            } else {
                "bibliography.ris"
            }),
        );
        cx.spawn(async move |view, cx| {
            let result = picker.await;
            let _ = view.update(cx, |this, cx| match result {
                Ok(Ok(Some(path))) => {
                    this.call(Lane::Export, Event::Export, |c, done| {
                        c.export(
                            ExportRequest {
                                selection,
                                format,
                                path,
                            },
                            done,
                        )
                    });
                    this.status("Exporting real library query…", false, cx);
                }
                Ok(Ok(None)) => this.status("Export cancelled; no file written", false, cx),
                _ => this.status("Export file picker failed", true, cx),
            });
        })
        .detach();
    }
}
