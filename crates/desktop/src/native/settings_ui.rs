use super::*;
impl LibraryView {
    /// #84 the two settings screens link to each other (they also open from ⚙ Settings in
    /// the top bar and from the Command Palette).
    pub(super) fn settings_switcher(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(hint("Settings:"))
            .child(
                div()
                    .flex()
                    .gap_1()
                    .p(px(2.))
                    .rounded(px(RADIUS.md))
                    .bg(rgb(tok().sidebar))
                    .child(
                        nav_item(
                            "switch-organize",
                            "Organize & export",
                            self.screen == Screen::Settings,
                        )
                        .px_3()
                        .on_click(cx.listener(|this, _, w, cx| this.show(Screen::Settings, w, cx))),
                    )
                    .child(
                        nav_item(
                            "switch-api",
                            "API & integrations",
                            self.screen == Screen::Api,
                        )
                        .px_3()
                        .on_click(cx.listener(|this, _, w, cx| {
                            this.open_api(api_ui::ApiFocus::Settings, w, cx)
                        })),
                    ),
            )
            .child(div().flex_1())
            .child(hint("esc returns to the library"))
    }
    pub(super) fn settings_screen(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let enabled = !self.locked() && !self.busy(Lane::Organization);
        let card = || {
            div()
                .p_4()
                .rounded(px(RADIUS.lg))
                .border_1()
                .border_color(rgb(tok().border))
                .bg(rgb(tok().surface))
                .flex()
                .flex_col()
                .gap_2()
        };
        let title = |text: &'static str| {
            div()
                .text_size(px(dims().font_title))
                .font_weight(FontWeight::SEMIBOLD)
                .child(text)
        };
        let row = || div().flex().items_center().gap_2();
        let label = |text: SharedString| div().w(px(150.)).child(text);
        div()
            .id("settings")
            .flex_1()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_4()
            .max_w(px(900.))
            .child(self.settings_switcher(cx))
            // Collections & tags first: it is the most frequent task on this screen.
            .child(
                card()
                    .child(title("Collections & tags"))
                    .child(hint(
                        "Deleting a collection or tag keeps its papers. Papers can belong to several collections.",
                    ))
                    .child(
                        row()
                            .child(div().w(px(320.)).child(self.name.clone()))
                            .child(
                                primary_button(
                                    "collection-save",
                                    if self.editing_collection.is_some() {
                                        "Save collection"
                                    } else {
                                        "Create collection"
                                    },
                                    enabled && self.editing_tag.is_none(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if this.editing_tag.is_some() {
                                        return;
                                    }
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
                                    enabled && self.editing_collection.is_none(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if this.editing_collection.is_some() {
                                        return;
                                    }
                                    let input =
                                        TagInput::named(this.name.read(cx).content.to_string());
                                    let request = if let Some(id) = this.editing_tag {
                                        OrganizationRequest::UpdateTag(id, input)
                                    } else {
                                        OrganizationRequest::CreateTag(input)
                                    };
                                    this.organize(request, cx);
                                })),
                            )
                            .when(
                                self.editing_collection.is_some() || self.editing_tag.is_some(),
                                |el| {
                                    el.child(button("new-name", "Cancel edit", enabled).on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.editing_collection = None;
                                            this.editing_tag = None;
                                            this.parent = None;
                                            this.name.update(cx, |f, cx| {
                                                f.set_text(String::new(), cx)
                                            });
                                            cx.notify();
                                        }),
                                    ))
                                },
                            ),
                    )
                    .when(self.editing_tag.is_none(), |el| {
                        el.child(
                            row()
                                .flex_wrap()
                                .child(hint("Parent collection:"))
                                .child(
                                    nav_item("parent-root", "Top level", self.parent.is_none())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.parent = None;
                                            cx.notify();
                                        })),
                                )
                                .children(
                                    self.organization
                                        .collections
                                        .iter()
                                        .enumerate()
                                        // A collection cannot be its own parent.
                                        .filter(|(_, c)| Some(c.id) != self.editing_collection)
                                        .map(|(i, c)| {
                                            let id = c.id;
                                            nav_item(
                                                ("parent", i),
                                                c.name.clone(),
                                                self.parent == Some(id),
                                            )
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.parent = Some(id);
                                                cx.notify();
                                            }))
                                        }),
                                ),
                        )
                    })
                    .child(section("COLLECTIONS"))
                    .when(self.organization.collections.is_empty(), |el| {
                        el.child(hint("No collections yet."))
                    })
                    .children(self.organization.collections.iter().enumerate().map(|(i, c)| {
                        let id = c.id;
                        row()
                            .child(
                                div()
                                    .flex_1()
                                    .when(self.editing_collection == Some(id), |el| {
                                        el.font_weight(FontWeight::SEMIBOLD)
                                    })
                                    .child(format!(
                                        "{}{}",
                                        c.name,
                                        c.parent
                                            .and_then(|p| self
                                                .organization
                                                .collections
                                                .iter()
                                                .find(|x| x.id == p))
                                            .map(|p| format!("  (in {})", p.name))
                                            .unwrap_or_default()
                                    )),
                            )
                            .child(
                                button(("edit-collection", i), "Rename / move", enabled).on_click(
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
                                button(("delete-collection", i), "Delete", enabled)
                                    .when(enabled, |b| b.text_color(rgb(tok().danger)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.organize(OrganizationRequest::DeleteCollection(id), cx)
                                    })),
                            )
                    }))
                    .child(section("TAGS"))
                    .when(self.organization.tags.is_empty(), |el| {
                        el.child(hint("No tags yet."))
                    })
                    .children(self.organization.tags.iter().enumerate().map(|(i, t)| {
                        let id = t.id;
                        row()
                            .child(
                                div()
                                    .flex_1()
                                    .when(self.editing_tag == Some(id), |el| {
                                        el.font_weight(FontWeight::SEMIBOLD)
                                    })
                                    .child(format!("#{}", t.name)),
                            )
                            .child(button(("edit-tag", i), "Rename", enabled).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(t) =
                                        this.organization.tags.iter().find(|t| t.id == id)
                                    {
                                        this.editing_tag = Some(id);
                                        this.editing_collection = None;
                                        let name = t.name.clone();
                                        this.name.update(cx, |f, cx| f.set_text(name, cx));
                                        cx.notify();
                                    }
                                }),
                            ))
                            .child(
                                button(("delete-tag", i), "Delete", enabled)
                                    .when(enabled, |b| b.text_color(rgb(tok().danger)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.organize(OrganizationRequest::DeleteTag(id), cx)
                                    })),
                            )
                    })),
            )
            .child(self.keyboard_card(cx))
            .child(
                card()
                    .child(title("Export bibliography"))
                    .child(hint(
                        "Writes a new file. PDFs, annotations and collections/tags are not included.",
                    ))
                    .child(
                        row().child(label("Format".into())).children(
                            [(Format::Bibtex, "BibTeX"), (Format::Ris, "RIS")]
                                .into_iter()
                                .map(|(format, name)| {
                                    nav_item(name, name, self.export_format == format).on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            this.export_format = format;
                                            cx.notify();
                                        }),
                                    )
                                }),
                        ),
                    )
                    .child(
                        row().child(label("Papers".into())).children(
                            [
                                "All active papers".to_string(),
                                format!("Selected ({})", self.library.selected.len()),
                                "Current collection".to_string(),
                            ]
                            .into_iter()
                            .enumerate()
                            .map(|(i, name)| {
                                nav_item(("export-scope", i), name, self.export_scope == i)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.export_scope = i;
                                        cx.notify();
                                    }))
                            }),
                        ),
                    )
                    .child(
                        row().child(
                            primary_button("export", "Export to new file…", !self.busy(Lane::Export))
                                .on_click(cx.listener(|this, _, _, cx| this.pick_export(cx))),
                        ),
                    ),
            )
            .child(
                card()
                    .id("appearance")
                    .child(title("Appearance"))
                    .child(hint(
                        "Saved with the other settings of this private library. System follows the light or dark appearance of the OS.",
                    ))
                    .child(
                        row()
                            .child(label("Theme".into()))
                            .children(ThemeMode::ALL.into_iter().map(|mode| {
                                nav_item(
                                    SharedString::from(format!("theme-{}", mode.label())),
                                    mode.label(),
                                    self.settings.theme == mode,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.settings.theme != mode {
                                        this.settings.theme = mode;
                                        this.persist(cx);
                                    }
                                }))
                            })),
                    )
                    .child(
                        row()
                            .child(label("Density".into()))
                            .children(Density::ALL.into_iter().map(|density| {
                                nav_item(
                                    SharedString::from(format!("density-{}", density.label())),
                                    density.label(),
                                    self.settings.density == density,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.settings.density != density {
                                        this.settings.density = density;
                                        this.persist(cx);
                                    }
                                }))
                            })),
                    ),
            )
            .child(
                card()
                    .child(title("Table & panes"))
                    .child(hint("Saved automatically in this private library."))
                    .child(hint(
                        "Columns: also from Settings › Table columns or by right-clicking a column header. Click a header to sort.",
                    ))
                    .children(self.settings.columns.iter().enumerate().map(|(i, c)| {
                        let kind = crate::columns::ColumnKind::from_name(&c.name);
                        let fixed = !kind.is_some_and(|k| k.can_hide());
                        row()
                            .child(
                                div().w(px(150.)).child(
                                    check_item(("visible", i), c.visible, c.name.clone())
                                    // Title stays visible so rows remain identifiable.
                                    .when(fixed, |el| el.cursor_default().text_color(rgb(tok().muted)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(kind) = kind
                                            && crate::columns::toggle(&mut this.settings.columns, kind)
                                        {
                                            this.persist(cx);
                                        }
                                    })),
                                ),
                            )
                            .child(icon_button(("narrow", i), Icon::Minus, "", c.width > 50).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.settings.columns[i].width =
                                        this.settings.columns[i].width.saturating_sub(20).max(50);
                                    this.persist(cx);
                                },
                            )))
                            .child(div().w(px(64.)).text_center().child(format!("{} px", c.width)))
                            .child(icon_button(("wide", i), Icon::Plus, "", c.width < 800).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.settings.columns[i].width =
                                        (this.settings.columns[i].width + 20).min(800);
                                    this.persist(cx);
                                },
                            )))
                    }))
                    .child(
                        row()
                            .child(label("Sidebar width".into()))
                            .child(
                                icon_button("navigation-narrow", Icon::Minus, "", self.settings.navigation_width > 160)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings.navigation_width =
                                            this.settings.navigation_width.saturating_sub(20).max(160);
                                        this.persist(cx);
                                    })),
                            )
                            .child(
                                div()
                                    .w(px(64.))
                                    .text_center()
                                    .child(format!("{} px", self.settings.navigation_width)),
                            )
                            .child(
                                icon_button("navigation-wide", Icon::Plus, "", self.settings.navigation_width < 340)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings.navigation_width =
                                            (this.settings.navigation_width + 20).min(340);
                                        this.persist(cx);
                                    })),
                            ),
                    )
                    .child(
                        row()
                            .child(label("Details width".into()))
                            .child(
                                icon_button("details-narrow", Icon::Minus, "", self.settings.details_width > 300)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings.details_width =
                                            this.settings.details_width.saturating_sub(40).max(300);
                                        this.persist(cx);
                                    })),
                            )
                            .child(
                                div()
                                    .w(px(64.))
                                    .text_center()
                                    .child(format!("{} px", self.settings.details_width)),
                            )
                            .child(
                                icon_button("details-wide", Icon::Plus, "", self.settings.details_width < 660)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings.details_width =
                                            (this.settings.details_width + 40).min(660);
                                        this.persist(cx);
                                    })),
                            ),
                    )
                    .child(
                        row()
                            .flex_wrap()
                            .child(label("Default sort".into()))
                            .children(
                                [
                                    (SortField::Title, "Title"),
                                    (SortField::Year, "Year"),
                                    (SortField::CreatedAt, "Added"),
                                    (SortField::UpdatedAt, "Modified"),
                                    (SortField::Id, "ID"),
                                ]
                                .into_iter()
                                .enumerate()
                                .map(|(i, (field, name))| {
                                    nav_item(("sort", i), name, self.settings.sort.field == field)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.settings.sort.field = field;
                                            this.library.query.sort = this.settings.sort;
                                            this.persist(cx);
                                            this.reload(cx);
                                        }))
                                }),
                            )
                            .child(
                                icon_button(
                                    "direction",
                                    if self.settings.sort.direction == SortDirection::Asc {
                                        Icon::ChevronUp
                                    } else {
                                        Icon::ChevronDown
                                    },
                                    if self.settings.sort.direction == SortDirection::Asc {
                                        "Ascending"
                                    } else {
                                        "Descending"
                                    },
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
                    .child(
                        row().child(
                            button("reset-layout", "Reset layout to defaults", true).on_click(
                                cx.listener(|this, _, _, cx| {
                                    // #87 the layout reset keeps the chosen theme and density.
                                    this.settings = Settings {
                                        theme: this.settings.theme,
                                        density: this.settings.density,
                                        ..Settings::default()
                                    };
                                    this.library.query.sort = this.settings.sort;
                                    this.persist(cx);
                                    this.reload(cx);
                                }),
                            ),
                        ),
                    ),
            )
    }
    fn keyboard_card(&self, cx: &mut Context<Self>) -> Div {
        use crate::commands::workbench_ids as wb;
        let run = |id: &'static str| {
            cx.listener(
                move |this: &mut Self, _: &ClickEvent, window: &mut Window, cx| {
                    this.run_command(id, window, cx)
                },
            )
        };
        div()
            .p_4()
            .rounded(px(RADIUS.lg))
            .border_1()
            .border_color(rgb(tok().border))
            .bg(rgb(tok().surface))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(dims().font_title))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Keyboard"),
            )
            .child(hint(format!(
                "{} opens the Command Palette everywhere. Text fields always receive typed keys.",
                shortcut("K")
            )))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(150.)).child("Vim keys in table"))
                    .child(
                        nav_item("vim-on", "On", self.settings.vim).on_click(cx.listener(
                            |this, _, window, cx| {
                                if !this.settings.vim {
                                    this.run_command(wb::TOGGLE_VIM, window, cx)
                                }
                            },
                        )),
                    )
                    .child(
                        nav_item("vim-off", "Off", !self.settings.vim).on_click(cx.listener(
                            |this, _, window, cx| {
                                if this.settings.vim {
                                    this.run_command(wb::TOGGLE_VIM, window, cx)
                                }
                            },
                        )),
                    )
                    .child(hint(
                        "j/k move · gg/G first/last · y copy citation · / search",
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(150.)).child("Keymap"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(
                                match (self.keymap_custom, self.keymap_diagnostics.is_empty()) {
                                    (true, _) => format!(
                                        "Custom: {}",
                                        self.keymap_path.as_deref().unwrap_or("keymap.json")
                                    ),
                                    (false, false) => {
                                        "Built-in defaults (keymap.json was rejected)".to_owned()
                                    }
                                    (false, true) => "Built-in defaults".to_owned(),
                                },
                            ),
                    )
                    .child(button("keymap-reload", "Reload", true).on_click(run(wb::RELOAD_KEYMAP)))
                    .child(
                        button("keymap-copy", "Copy default JSON", true)
                            .on_click(run(wb::COPY_DEFAULT_KEYMAP)),
                    ),
            )
            .when(!self.keymap_diagnostics.is_empty(), |el| {
                el.child(
                    div()
                        .p_2()
                        .rounded(px(RADIUS.md))
                        .bg(rgb(tok().error_bg))
                        .text_color(rgb(tok().error_text))
                        .text_size(px(dims().font_small))
                        .child(
                            "The whole keymap.json was ignored; fix these and Reload. \
                             Unknown commands, duplicate keys and prefix conflicts are rejected.",
                        )
                        .children(
                            self.keymap_diagnostics
                                .iter()
                                .take(12)
                                .map(|d| div().child(format!("• {d}"))),
                        ),
                )
            })
            .child(hint(format!(
                "Custom keymaps replace the defaults entirely. File: {} (private 0600).",
                self.keymap_path
                    .as_deref()
                    .unwrap_or("keymap.json beside the settings file")
            )))
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
        let Some(directory) = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_absolute())
        else {
            self.status(
                "Cannot open export picker: home directory is unavailable",
                true,
                cx,
            );
            return;
        };
        let picker = cx.prompt_for_new_path(
            &directory,
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
