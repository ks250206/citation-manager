use super::*;
impl LibraryView {
    pub(super) fn details_screen(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let enabled = !self.locked();
        let tab = self.settings.details_tab.as_str();
        div().id("details-host").when(!self.settings.reading,|el|el.w(px(self.settings.details_width as f32)).flex_shrink_0()).when(self.settings.reading,|el|el.flex_1().min_w_0()).min_h_0().flex().flex_col().border_l_1().border_color(rgb(0xd5dce5))
            .child(div().flex().p_2().gap_2().children(["Info","Notes","PDF"].into_iter().map(|tab|button(tab,tab,true).on_click(cx.listener(move|this,_,_,cx|{this.settings.details_tab=tab.into();this.persist(cx);}))))
                .child(button("reading",if self.settings.reading{"Exit reading"}else{"Expand"},true).on_click(cx.listener(|this,_,_,cx|{this.settings.reading = !this.settings.reading;this.persist(cx);}))))
            .child(div().id("details-content").flex_1().overflow_y_scroll().p_3()
                .child(match tab{
                    "PDF"=>self.pdf_host(cx).into_any_element(),
                    "Notes"=>div().child(line("Notes" )).child(line("Notes editing is not available in this version. No note text is saved." )).into_any_element(),
                    _=>div().flex().flex_col().gap_2()
                        .child(line(if self.editor.is_some(){"Paper metadata"}else{"New paper — review before save"}))
                        .child(button("save",if self.editor.is_some(){"Save changes  ⌘S"}else{"Preview new paper  ⌘S"},enabled).on_click(cx.listener(|this,_,w,cx|this.save(&Save,w,cx))))
                        .when_some(self.editor.as_ref(),|el,p|{let id=p.id;let deleted=p.deleted_at.is_some();el.child(button("delete-restore",if deleted{"Restore paper"}else{"Move paper to Trash"},enabled).on_click(cx.listener(move|this,_,_,cx|{if this.locked() || !this.can_replace_editor(cx){return;}this.call(Lane::Editor,Event::Editor,|c,done|c.lifecycle(id,!deleted,Box::new(move|result|done(result.map(|p|LibraryReply::Paper(Box::new(p)))))));cx.notify();})))
                            .child(line(format!("Citation key: {}",p.bibliography.citation_key.as_deref().unwrap_or("—"))))})
                        .when(self.editor.as_ref().is_some_and(|p|p.deleted_at.is_none()),|el|el.child(self.memberships(cx)))
                        .children(crate::details::FIELDS.iter().zip(self.fields.iter()).map(|(name,field)|div().child(line(*name)).child(field.clone())))
                        .child(line("Unedited contributor roles, identifiers and extra metadata are preserved."))
                        .child(line("DOI lookup (read-only; review a new import preview before save)"))
                        .child(self.doi.clone())
                        .child(button("resolve","Look up DOI",!self.busy(Lane::Doi)&&enabled).on_click(cx.listener(|this,_,_,cx|this.lookup_doi(cx))))
                        .child(button("cancel-doi","Cancel lookup / keep manual draft",self.busy(Lane::Doi)).on_click(cx.listener(|this,_,_,cx|{this.invalidate_doi();this.status("Lookup cancelled. Manual draft retained",false,cx);})))
                        .into_any_element(),
                }))
    }
    fn memberships(&self, cx: &mut Context<Self>) -> Div {
        let Some(p) = &self.editor else {
            return div();
        };
        let paper = p.id;
        div()
            .child(line("Memberships (multiple allowed)"))
            .children(
                self.organization
                    .collections
                    .iter()
                    .enumerate()
                    .map(|(index, c)| {
                        let id = c.id;
                        let present = self.membership.collections.iter().any(|v| v.id == id);
                        button(
                            ("member-collection", index),
                            format!("{} {}", if present { "☑" } else { "☐" }, c.name),
                            !self.locked(),
                        )
                        .w_full()
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
                button(
                    ("member-tag", index),
                    format!("{} #{}", if present { "☑" } else { "☐" }, t.name),
                    !self.locked(),
                )
                .w_full()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.organize(OrganizationRequest::TagMember(paper, id, !present), cx)
                }))
            }))
    }
    fn pdf_host(&self, cx: &mut Context<Self>) -> Div {
        let enabled = !self.locked();
        div()
            .child(line("PDF attachments"))
            .when(self.editor.is_none(), |el| {
                el.child(line("Select a saved paper to attach a PDF."))
            })
            .when_some(self.editor.as_ref(), |el, p| {
                let id = p.id;
                el.child(
                    button("attach-picker", "Attach PDF to this paper…", enabled).on_click(
                        cx.listener(move |this, _, _, cx| {
                            if !this.locked() {
                                this.pick_pdfs(false, PdfIntent::AttachExisting(id), cx);
                            }
                        }),
                    ),
                )
                .child(
                    div()
                        .id("attach-drop")
                        .p_4()
                        .my_2()
                        .bg(rgb(0xe0eaff))
                        .child(
                            "Drop PDFs here to attach to this paper. Bibliography stays unchanged.",
                        )
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
                    .p_4()
                    .my_2()
                    .bg(rgb(0xe5efe8))
                    .child("Drop PDFs here to review registration as NEW papers.")
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                        if !this.locked() {
                            this.inspect(
                                Selection::Files(paths.paths().to_vec()),
                                PdfIntent::RegisterNew,
                                cx,
                            );
                        }
                    })),
            )
            .when(self.busy(Lane::Attachments), |el| {
                el.child(line("Loading attachment references…"))
            })
            .when(
                self.host.attachments.is_empty() && !self.busy(Lane::Attachments),
                |el| el.child(line("No attachments")),
            )
            .children(
                self.host
                    .attachments
                    .iter()
                    .enumerate()
                    .map(|(index, (a, status))| {
                        let id = a.id;
                        button(
                            ("attachment", index),
                            format!(
                                "{} {} · {} bytes · {:?}",
                                if self.host.selected == Some(id) {
                                    "▸"
                                } else {
                                    ""
                                },
                                a.original_name,
                                a.byte_size,
                                status
                            ),
                            true,
                        )
                        .w_full()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.host.selected = Some(id);
                            cx.notify();
                        }))
                    }),
            )
            .when_some(self.host.selected, |el, id| {
                el.child(
                    div()
                        .id("pdf-viewer-host-slot")
                        .mt_4()
                        .p_4()
                        .h(px(220.))
                        .bg(rgb(0xe7e9ed))
                        .child(line("Attachment selected"))
                        .child(line(format!("{}", id.0)))
                        .child(line(
                            "PDF rendering and annotation tools are not available in this version.",
                        )),
                )
            })
    }
}
