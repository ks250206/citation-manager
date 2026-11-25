use super::*;
impl LibraryView {
    pub(super) fn details_screen(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let enabled = !self.locked();
        // The viewer owns its own scrolling, so the PDF tab is not inside the details scroll.
        let viewer = (self.settings.details_tab == "PDF").then(|| self.viewer_panel(window, cx));
        let tab = self.settings.details_tab.as_str();
        div()
            .id("details-host")
            .when(!self.settings.reading, |el| {
                el.w(px(self.settings.details_width as f32)).flex_shrink_0()
            })
            .when(self.settings.reading, |el| el.flex_1().min_w_0())
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(SURFACE))
            .border_l_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .px_2()
                    .py_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .children(["Info", "Notes", "PDF"].into_iter().map(|name| {
                        nav_item(name, name, tab == name)
                            .px_3()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.settings.details_tab = name.into();
                                this.persist(cx);
                            }))
                    }))
                    .child(div().flex_1())
                    .child(
                        button(
                            "reading",
                            if self.settings.reading {
                                "⇤ Exit reading"
                            } else {
                                "⇥ Expand"
                            },
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings.reading = !this.settings.reading;
                            this.persist(cx);
                        })),
                    ),
            )
            .when_some(viewer, |el, viewer| {
                el.child(
                    div()
                        .flex_shrink_0()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(rgb(BORDER))
                        .child(self.pdf_host(cx)),
                )
                .child(viewer)
            })
            .when(tab != "PDF", |el| {
                el.child(
                    div()
                        .id("details-content")
                        .flex_1()
                        .overflow_y_scroll()
                        .p_3()
                        .child(match tab {
                            "Notes" => self.notes_tab(cx).into_any_element(),
                            _ => self.info_tab(enabled, cx).into_any_element(),
                        }),
                )
            })
    }
    fn info_tab(&self, enabled: bool, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(match &self.editor {
                                Some(p) if p.deleted_at.is_some() => "Paper (in Trash)",
                                Some(_) => "Paper metadata",
                                None => "New paper",
                            }),
                    )
                    .child(
                        primary_button(
                            "save",
                            format!(
                                "{}  {}",
                                if self.editor.is_some() {
                                    "Save"
                                } else {
                                    "Preview"
                                },
                                shortcut("S")
                            ),
                            enabled && (self.dirty || self.editor.is_none()),
                        )
                        .on_click(cx.listener(|this, _, w, cx| this.save(&Save, w, cx))),
                    ),
            )
            .when(self.editor.is_none(), |el| {
                el.child(hint(
                    "Fill in the fields, then preview. Nothing is saved until you confirm the preview.",
                ))
            })
            .when_some(self.editor.as_ref(), |el, p| {
                let deleted = p.deleted_at.is_some();
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            hint(format!(
                                "Citation key: {}",
                                p.bibliography.citation_key.as_deref().unwrap_or("—")
                            ))
                            .flex_1(),
                        )
                        .child(
                            button(
                                "delete-restore",
                                if deleted {
                                    "Restore paper"
                                } else {
                                    "Move to Trash"
                                },
                                enabled,
                            )
                            .when(!deleted && enabled, |b| {
                                b.text_color(rgb(0xb42318)).border_color(rgb(0xf1c4bf))
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_trash(cx))),
                        ),
                )
            })
            .children(
                crate::details::FIELDS
                    .iter()
                    .zip(self.fields.iter())
                    .map(|(name, field)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(hint(*name))
                            .child(field.clone())
                    }),
            )
            .child(hint(
                "Unedited contributor roles, identifiers and extra metadata are preserved.",
            ))
            .when(
                self.editor.as_ref().is_some_and(|p| p.deleted_at.is_none()),
                |el| el.child(self.memberships(cx)),
            )
            .child(section("DOI LOOKUP"))
            .child(hint(
                "Read-only lookup. The result opens as an import preview for review before saving.",
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(self.doi.clone()))
                    .when(!self.busy(Lane::Doi), |el| {
                        el.child(
                            button("resolve", "Look up", enabled)
                                .on_click(cx.listener(|this, _, _, cx| this.lookup_doi(cx))),
                        )
                    })
                    .when(self.busy(Lane::Doi), |el| {
                        el.child(button("cancel-doi", "Cancel lookup", true).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.invalidate_doi();
                                this.status("Lookup cancelled. Manual draft retained", false, cx);
                            }),
                        ))
                    }),
            )
    }
    /// Move the open paper to Trash, or restore it when it is already there.
    pub(super) fn toggle_trash(&mut self, cx: &mut Context<Self>) {
        let Some(paper) = &self.editor else {
            return;
        };
        let (id, deleted) = (paper.id, paper.deleted_at.is_some());
        if self.locked() || !self.can_replace_editor(cx) {
            return;
        }
        self.call(Lane::Editor, Event::Editor, |c, done| {
            c.lifecycle(
                id,
                !deleted,
                Box::new(move |result| done(result.map(|p| LibraryReply::Paper(Box::new(p))))),
            )
        });
        cx.notify();
    }
    fn memberships(&self, cx: &mut Context<Self>) -> Div {
        let Some(p) = &self.editor else {
            return div();
        };
        let paper = p.id;
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(section("COLLECTIONS & TAGS"))
            .when(
                self.organization.collections.is_empty() && self.organization.tags.is_empty(),
                |el| {
                    el.child(hint(
                        "Create collections or tags in ⚙ Settings › Organize & export.",
                    ))
                },
            )
            .children(
                self.organization
                    .collections
                    .iter()
                    .enumerate()
                    .map(|(index, c)| {
                        let id = c.id;
                        let present = self.membership.collections.iter().any(|v| v.id == id);
                        nav_item(
                            ("member-collection", index),
                            format!("{} {}", if present { "☑" } else { "☐" }, c.name),
                            present,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.organize(
                                OrganizationRequest::CollectionMember(paper, id, !present),
                                cx,
                            )
                        }))
                    }),
            )
            .children(self.organization.tags.iter().enumerate().map(|(index, t)| {
                let id = t.id;
                let present = self.membership.tags.iter().any(|v| v.id == id);
                nav_item(
                    ("member-tag", index),
                    format!("{} #{}", if present { "☑" } else { "☐" }, t.name),
                    present,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.organize(OrganizationRequest::TagMember(paper, id, !present), cx)
                }))
            }))
    }
    /// Attachment list above the viewer. The rows come first and have their own bounded
    /// scroll area, so every attachment stays reachable above the viewer toolbar (#84);
    /// the attach button and the two drop targets share one compact row below.
    fn pdf_host(&self, cx: &mut Context<Self>) -> Div {
        let enabled = !self.locked();
        let paper = self.editor.as_ref().map(|p| p.id);
        let count = self.host.attachments.len();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(match count {
                                0 => "PDF attachments".to_string(),
                                n => format!("PDF attachments ({n})"),
                            }),
                    )
                    .when_some(paper, |el, id| {
                        el.child(button("attach-picker", "Attach PDF…", enabled).on_click(
                            cx.listener(move |this, _, _, cx| {
                                if !this.locked() {
                                    this.pick_pdfs(false, PdfIntent::AttachExisting(id), cx);
                                }
                            }),
                        ))
                    }),
            )
            .when(paper.is_none(), |el| {
                el.child(hint("Select a saved paper to attach a PDF."))
            })
            .when(self.busy(Lane::Attachments), |el| {
                el.child(hint("Loading attachment references…"))
            })
            .when(
                paper.is_some() && count == 0 && !self.busy(Lane::Attachments),
                |el| el.child(hint("No attachments yet")),
            )
            .when(count > 0, |el| {
                el.child(
                    div()
                        .id("attachment-list")
                        // About four rows; more scroll inside this list, never under the toolbar.
                        .max_h(px(124.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .children(self.host.attachments.iter().enumerate().map(
                            |(index, (a, status))| {
                                let id = a.id;
                                nav_item(
                                    ("attachment", index),
                                    format!(
                                        "📄 {} · {} KB{}",
                                        a.original_name,
                                        a.byte_size.div_ceil(1024),
                                        match status {
                                            AttachmentStatus::Present => String::new(),
                                            other => format!(" · {other:?}"),
                                        }
                                    ),
                                    self.host.selected == Some(id),
                                )
                                .flex_shrink_0()
                                .w_full()
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.host.selected = Some(id);
                                        cx.notify();
                                    },
                                ))
                            },
                        )),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_1()
                    .text_size(px(11.))
                    .when_some(paper, |el, id| {
                        el.child(
                            div()
                                .id("attach-drop")
                                .flex_1()
                                .px_2()
                                .py(px(3.))
                                .rounded_md()
                                .bg(rgb(0xe0eaff))
                                .min_w_0()
                                .child("Drop PDFs to attach")
                                .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                                    if !this.locked() {
                                        this.inspect(
                                            Selection::Files(paths.paths().to_vec()),
                                            PdfIntent::AttachExisting(id),
                                            cx,
                                        );
                                    }
                                })),
                        )
                    })
                    .child(
                        div()
                            .id("register-drop")
                            .flex_1()
                            .px_2()
                            .py(px(3.))
                            .rounded_md()
                            .bg(rgb(0xe5efe8))
                            .min_w_0()
                            .child("Drop PDFs as new papers")
                            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                                if !this.locked() {
                                    this.inspect(
                                        Selection::Files(paths.paths().to_vec()),
                                        PdfIntent::RegisterNew,
                                        cx,
                                    );
                                }
                            })),
                    ),
            )
    }
}
