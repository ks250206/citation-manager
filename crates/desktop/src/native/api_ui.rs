//! #26 "API & Integrations" screen: local API settings, tokens, MCP profiles and paired
//! browser hosts. Every change is a `Command::Api` decided by the daemon; this view only
//! renders replies. A new secret is shown once and dropped when leaving the screen.
use super::*;
use crate::api_settings::{
    ApiSettingsState, Confirm, ELEVATE_WARNING, ELEVATION_MINUTES, FILE_STORE_WARNING, Group,
    ISSUABLE, RevealedSecret, allows_writes, format_date, format_time, scope_list, status,
    toggled_mcp_scopes,
};
use cm_protocol::api::{
    ApiCommand, ApiReply, BindAddress, ClientKind, IssueToken, ListenerState, SecretBackend,
    TokenInfo, profiles,
};

/// What a reply belongs to; replies bypass the single-ticket lanes like annotations.
pub(super) enum ApiOp {
    Overview,
    Tokens,
    Audit,
    Issued,
    Changed,
}

/// Where `api.*` palette commands put the person.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ApiFocus {
    Settings,
    NewToken,
    NewMcpProfile,
}

pub(super) struct ApiInputs {
    pub(super) label: Entity<TextInput>,
    pub(super) port: Entity<TextInput>,
    pub(super) days: Entity<TextInput>,
}
impl ApiInputs {
    pub(super) fn new(cx: &mut Context<LibraryView>) -> Self {
        Self {
            label: cx.new(|cx| TextInput::new("Token name, e.g. Zotero bridge", cx)),
            port: cx.new(|cx| TextInput::new("Port", cx)),
            days: cx.new(|cx| TextInput::new("Days (empty = default)", cx)),
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

const DANGER: u32 = 0xb42318;
const WARN_BG: u32 = 0xfff4d6;
const WARN_TEXT: u32 = 0x6b4e00;

impl LibraryView {
    pub(super) fn api_send(&mut self, command: ApiCommand, op: ApiOp) {
        let sender = self.sender.clone();
        let sent = self.client.api(
            command,
            Box::new(move |result| {
                let _ = sender.send(Message {
                    lane: Lane::SaveSettings,
                    ticket: 0,
                    event: Event::Api(Box::new(op), result),
                });
            }),
        );
        if sent.is_err() {
            self.api.notice = Some(("Library service unavailable".into(), true));
        }
    }
    fn api_refresh(&mut self) {
        self.api_send(ApiCommand::Overview, ApiOp::Overview);
        self.api_send(ApiCommand::ListTokens, ApiOp::Tokens);
        self.api_send(ApiCommand::Audit { limit: 20 }, ApiOp::Audit);
    }
    pub(super) fn open_api(
        &mut self,
        focus: ApiFocus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show(Screen::Api, window, cx);
        self.api.confirm = None;
        match focus {
            ApiFocus::Settings => {}
            ApiFocus::NewToken => {
                self.api.form = Default::default();
                self.api_inputs.label.focus_handle(cx).focus(window, cx);
            }
            ApiFocus::NewMcpProfile => {
                self.api.form.kind = ClientKind::Mcp;
                self.api.form.scopes = profiles::mcp_read_only();
                self.api_inputs.label.focus_handle(cx).focus(window, cx);
            }
        }
        self.focus_layout = (self.screen, self.settings.details_tab.clone());
        self.api_refresh();
        cx.notify();
    }
    pub(super) fn api_reply(
        &mut self,
        op: ApiOp,
        result: Result<ApiReply, ServiceError>,
        cx: &mut Context<Self>,
    ) {
        match (op, result) {
            (_, Err(e)) => {
                self.api.notice = Some((format!("Not changed: {e}"), true));
                self.status(format!("API settings: {e}"), true, cx);
            }
            (ApiOp::Overview, Ok(ApiReply::Overview(overview))) => {
                let port = overview.settings.port.to_string();
                self.api_inputs
                    .port
                    .update(cx, |f, cx| f.set_text(port, cx));
                self.api.set_overview(overview);
            }
            (ApiOp::Tokens, Ok(ApiReply::Tokens(tokens))) => self.api.tokens = tokens,
            (ApiOp::Audit, Ok(ApiReply::Audit(entries))) => self.api.audit = entries,
            (
                ApiOp::Issued,
                Ok(ApiReply::Issued {
                    token,
                    credential,
                    stored_in,
                    ..
                }),
            ) => {
                self.api_inputs
                    .label
                    .update(cx, |f, cx| f.set_text(String::new(), cx));
                self.api.notice = Some((
                    format!(
                        "Issued “{}”. Copy the token now; it is shown only once.",
                        token.label
                    ),
                    false,
                ));
                self.api.upsert(token.clone());
                self.api.revealed = Some(RevealedSecret {
                    token,
                    secret: credential.0,
                    stored_in,
                    copied: false,
                });
                self.api_send(ApiCommand::ListTokens, ApiOp::Tokens);
                self.api_send(ApiCommand::Audit { limit: 20 }, ApiOp::Audit);
            }
            (ApiOp::Changed, Ok(reply)) => {
                let text = match &reply {
                    ApiReply::Revoked(_) => {
                        "Revoked. Connected clients are refused from their next request."
                    }
                    ApiReply::Updated(t) if t.elevated_until_ms.is_some() => {
                        "Elevated for bulk operations until the time shown."
                    }
                    ApiReply::Updated(_) => {
                        "Updated. Connected clients use the new scopes immediately."
                    }
                    ApiReply::Overview(_) => "API settings saved.",
                    _ => "Done.",
                };
                self.api.notice = Some((text.into(), false));
                if let ApiReply::Overview(overview) = reply {
                    self.api.set_overview(overview);
                }
                self.api_refresh();
            }
            (_, Ok(_)) => {}
        }
        cx.notify();
    }

    fn api_issue(&mut self, cx: &mut Context<Self>) {
        self.api.form.expiry_days = self.api_inputs.days.read(cx).content.to_string();
        let label = self.api_inputs.label.read(cx).content.to_string();
        match self.api.form.request(&label, now_ms()) {
            Ok(request) => {
                self.api.conceal();
                self.api_send(ApiCommand::IssueToken(request), ApiOp::Issued);
            }
            Err(e) => self.api.notice = Some((e, true)),
        }
        cx.notify();
    }
    fn api_new_mcp(&mut self, cx: &mut Context<Self>) {
        let label = self.api_inputs.label.read(cx).content.to_string();
        let label = if label.trim().is_empty() {
            "AI client (read-only)".to_owned()
        } else {
            label
        };
        self.api.conceal();
        self.api_send(
            ApiCommand::IssueToken(IssueToken {
                label,
                kind: ClientKind::Mcp,
                scopes: profiles::mcp_read_only(),
                expires_at_ms: None,
            }),
            ApiOp::Issued,
        );
        cx.notify();
    }
    fn api_save_settings(&mut self, cx: &mut Context<Self>) {
        let port = self.api_inputs.port.read(cx).content.to_string();
        match self.api.settings_request(&port) {
            Ok(settings) => self.api_send(ApiCommand::SaveSettings(settings), ApiOp::Changed),
            Err(e) => self.api.notice = Some((e, true)),
        }
        cx.notify();
    }
    fn api_pick_export_dir(&mut self, cx: &mut Context<Self>) {
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Allow API exports into this folder".into()),
        });
        cx.spawn(async move |view, cx| {
            let result = picker.await;
            let _ = view.update(cx, |this, cx| {
                if let Ok(Ok(Some(paths))) = result
                    && let Some(path) = paths.into_iter().next()
                    && !this.api.export_dirs.contains(&path)
                {
                    this.api.export_dirs.push(path);
                    this.api.dirty = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }
    /// Runs a confirmed action.
    fn api_confirmed(&mut self, confirm: Confirm, cx: &mut Context<Self>) {
        self.api.confirm = None;
        let command = match confirm {
            Confirm::Revoke(id) => ApiCommand::Revoke(id),
            Confirm::Reissue(id) => {
                self.api.conceal();
                self.api_send(ApiCommand::Reissue(id), ApiOp::Issued);
                cx.notify();
                return;
            }
            Confirm::AllowWrites(id) => match self.api.token(id) {
                Some(t) => ApiCommand::SetScopes {
                    grant: id,
                    scopes: toggled_mcp_scopes(t),
                },
                None => return,
            },
            Confirm::Elevate(id) => ApiCommand::Elevate {
                grant: id,
                duration_ms: ELEVATION_MINUTES * 60_000,
            },
        };
        self.api_send(command, ApiOp::Changed);
        cx.notify();
    }

    pub(super) fn api_screen(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let card = || {
            div()
                .p_4()
                .rounded_lg()
                .border_1()
                .border_color(rgb(BORDER))
                .bg(rgb(SURFACE))
                .flex()
                .flex_col()
                .gap_2()
        };
        let title = |text: &'static str| {
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(text)
        };
        let row = || div().flex().items_center().gap_2();
        let label = |text: &'static str| div().w(px(130.)).child(text);
        let state = &self.api;
        let overview = state.overview.as_ref();
        div()
            .id("api-settings")
            .flex_1()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_4()
            .max_w(px(1200.))
            .when_some(state.notice.clone(), |el, (text, error)| {
                el.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(if error { 0xffe5e5 } else { ACCENT_SOFT }))
                        .text_color(rgb(if error { 0x9b1c1c } else { ACCENT_TEXT }))
                        .child(text),
                )
            })
            .when_some(state.revealed.clone(), |el, revealed| {
                el.child(self.api_secret_box(revealed, cx))
            })
            // 1. Local REST API.
            .child(
                card()
                    .child(title("Local REST API"))
                    .child(hint(
                        "Off by default. Listens on this computer only (loopback); other devices on your network cannot connect. Every request needs a token below.",
                    ))
                    .when(
                        overview.is_some_and(|o| o.listener == ListenerState::NotAvailable),
                        |el| {
                            el.child(hint(
                                "This build stores these settings but does not include the HTTP listener yet.",
                            ))
                        },
                    )
                    .child(
                        row()
                            .child(label("API"))
                            .child(nav_item("api-off", "Off", !state.enabled).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.api.enabled = false;
                                    this.api.dirty = true;
                                    cx.notify();
                                }),
                            ))
                            .child(nav_item("api-on", "On", state.enabled).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.api.enabled = true;
                                    this.api.dirty = true;
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(
                        row()
                            .child(label("Address"))
                            .children([BindAddress::LoopbackV4, BindAddress::LoopbackV6].map(
                                |bind| {
                                    nav_item(bind.as_str(), bind.as_str(), state.bind == bind)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.api.bind = bind;
                                            this.api.dirty = true;
                                            cx.notify();
                                        }))
                                },
                            ))
                            .child(div().w(px(24.)))
                            .child(div().w(px(50.)).child("Port"))
                            .child(div().w(px(110.)).child(self.api_inputs.port.clone())),
                    )
                    .child(
                        row()
                            .items_start()
                            .child(label("Export folders"))
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .when(state.export_dirs.is_empty(), |el| {
                                        el.child(hint(
                                            "None. Tokens with exports:write can only export into folders listed here.",
                                        ))
                                    })
                                    .children(state.export_dirs.iter().enumerate().map(|(i, dir)| {
                                        let dir = dir.clone();
                                        row()
                                            .child(div().flex_1().child(dir.display().to_string()))
                                            .child(button(("export-dir-remove", i), "Remove", true).on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.api.export_dirs.retain(|d| *d != dir);
                                                    this.api.dirty = true;
                                                    cx.notify();
                                                }),
                                            ))
                                    }))
                                    .child(
                                        div().flex().child(
                                            button("export-dir-add", "Add folder…", true).on_click(
                                                cx.listener(|this, _, _, cx| this.api_pick_export_dir(cx)),
                                            ),
                                        ),
                                    ),
                            ),
                    )
                    .child(
                        row()
                            .child(
                                primary_button("api-save", "Save API settings", true).on_click(
                                    cx.listener(|this, _, _, cx| this.api_save_settings(cx)),
                                ),
                            )
                            .when(state.dirty, |el| el.child(hint("Unsaved changes"))),
                    )
                    .child(hint(format!(
                        "Token copies are kept in: {}",
                        state
                            .secret_backend()
                            .map_or("…", |b| b.describe())
                    )))
                    .when(state.secret_backend() == Some(SecretBackend::File), |el| {
                        el.child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(WARN_BG))
                                .text_color(rgb(WARN_TEXT))
                                .text_size(px(12.))
                                .child(FILE_STORE_WARNING),
                        )
                    }),
            )
            // 2. Tokens.
            .child(
                card()
                    .child(
                        row()
                            .child(title("Access tokens (REST API and CLI)"))
                            .child(div().flex_1())
                            .child(
                                nav_item("show-revoked", "Show revoked", state.show_revoked)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.api.show_revoked = !this.api.show_revoked;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(hint(
                        "Each token gets only the scopes you tick and always expires. The secret is shown once; only its SHA-256 is stored in the library.",
                    ))
                    .child(
                        row()
                            .child(label("Name"))
                            .child(div().w(px(300.)).child(self.api_inputs.label.clone()))
                            .child(div().w(px(16.)))
                            .children(
                                [(ClientKind::Api, "REST API"), (ClientKind::Cli, "CLI"), (ClientKind::Mcp, "MCP")]
                                    .map(|(kind, name)| {
                                        nav_item(name, name, state.form.kind == kind).on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.api.form.kind = kind;
                                                cx.notify();
                                            }),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        row()
                            .items_start()
                            .child(label("Scopes"))
                            .child(
                                div().flex_1().flex().flex_wrap().gap_1().children(ISSUABLE.map(|scope| {
                                    let on = state.form.scopes.contains(&scope);
                                    nav_item(scope.name(), if on { format!("✓ {}", scope.name()) } else { scope.name().to_owned() }, on)
                                        .border_1()
                                        .border_color(rgb(BORDER))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.api.form.toggle(scope);
                                            cx.notify();
                                        }))
                                })),
                            ),
                    )
                    .child({
                        let (default, max) = state.form.lifetime_days();
                        row()
                            .child(label("Expires after"))
                            .child(div().w(px(180.)).child(self.api_inputs.days.clone()))
                            .child(hint(format!("days — default {default}, at most {max} for these scopes")))
                    })
                    .child(
                        row().child(
                            primary_button("api-issue", "Issue token", true)
                                .on_click(cx.listener(|this, _, _, cx| this.api_issue(cx))),
                        ),
                    )
                    .child(self.api_table(Group::Tokens, cx)),
            )
            // 3. MCP profiles.
            .child(
                card()
                    .child(title("MCP profiles (AI clients)"))
                    .child(hint(
                        "Profiles are read-only by default. Writing is a separate decision per profile; bulk delete/move additionally needs a 15-minute elevation, and more than 10 deletes or 25 moves per minute are refused without it.",
                    ))
                    .child(
                        row().child(
                            button("mcp-new", "New read-only profile (uses the name above)", true)
                                .on_click(cx.listener(|this, _, _, cx| this.api_new_mcp(cx))),
                        ),
                    )
                    .child(self.api_table(Group::Mcp, cx)),
            )
            // 4. Native hosts.
            .child(
                card()
                    .child(title("Paired browser hosts"))
                    .child(hint(
                        "Browser capture can only preview captures and read collection/tag names; it cannot read papers or files. Revoke a host you no longer use.",
                    ))
                    .child(self.api_table(Group::NativeHosts, cx)),
            )
            // 5. Audit.
            .child(
                card()
                    .child(title("Recent activity"))
                    .child(hint(
                        "Who, what, when and the result. Content, queries, file paths and secrets are never recorded.",
                    ))
                    .when(state.audit.is_empty(), |el| el.child(hint("Nothing recorded yet.")))
                    .children(state.audit.iter().map(|e| {
                        let who = e
                            .kind
                            .map_or("unknown", |k| k.name())
                            .to_owned();
                        div()
                            .flex()
                            .gap_3()
                            .text_size(px(12.))
                            .child(div().w(px(170.)).child(format_time(e.at_ms)))
                            .child(div().w(px(90.)).child(who))
                            .child(div().w(px(170.)).child(format!("{:?}", e.operation)))
                            .child(
                                div()
                                    .w(px(80.))
                                    .text_color(rgb(match e.outcome {
                                        cm_protocol::api::AuditOutcome::Succeeded => TEXT,
                                        _ => DANGER,
                                    }))
                                    .child(format!("{:?}", e.outcome).to_lowercase()),
                            )
                            .child(div().text_color(rgb(MUTED)).child(e.reason.clone().unwrap_or_default()))
                    })),
            )
    }

    fn api_secret_box(&self, revealed: RevealedSecret, cx: &mut Context<Self>) -> Div {
        let secret = revealed.secret.clone();
        div()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0xe0b84d))
            .bg(rgb(WARN_BG))
            .text_color(rgb(WARN_TEXT))
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).child(format!(
                "New {} token “{}” — copy it now. It will not be shown again.",
                revealed.token.kind.name(),
                revealed.token.label
            )))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(SURFACE))
                    .text_color(rgb(TEXT))
                    .font_family("monospace")
                    .child(secret.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        primary_button(
                            "secret-copy",
                            if revealed.copied { "Copied" } else { "Copy" },
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(secret.clone()));
                            if let Some(r) = &mut this.api.revealed {
                                r.copied = true;
                            }
                            cx.notify();
                        })),
                    )
                    .child(button("secret-done", "Done", true).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.api.conceal();
                            this.api.notice = None;
                            cx.notify();
                        },
                    )))
                    .child(hint(format!(
                        "Grant {} · scopes {} · expires {} · stored in {}",
                        revealed.token.id,
                        scope_list(&revealed.token.scopes),
                        format_date(revealed.token.expires_at_ms),
                        revealed
                            .stored_in
                            .map_or("nowhere (copy it now)", |b| b.describe())
                    ))),
            )
    }

    fn api_table(&self, which: Group, cx: &mut Context<Self>) -> Div {
        let state: &ApiSettingsState = &self.api;
        let now = now_ms();
        let rows: Vec<TokenInfo> = state.rows(which).cloned().collect();
        let header = |text: &'static str, w: f32| {
            div()
                .w(px(w))
                .flex_shrink_0()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(MUTED))
                .child(text)
        };
        let cell = |text: String, w: f32| {
            div()
                .w(px(w))
                .flex_shrink_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(text)
        };
        let mut table = div().flex().flex_col().gap_1().mt_2();
        if rows.is_empty() {
            return table.child(hint(match which {
                Group::NativeHosts => "No browser extension is paired.",
                Group::Mcp => "No MCP profiles yet.",
                _ => "No tokens yet.",
            }));
        }
        table = table.child(
            div()
                .flex()
                .gap_2()
                .child(header("NAME", 150.))
                .child(header("SCOPES", 210.))
                .child(header("CREATED", 86.))
                .child(header("LAST USED", 86.))
                .child(header("EXPIRES", 86.))
                .child(header("STATUS", 120.)),
        );
        for (i, t) in rows.into_iter().enumerate() {
            let id = t.id;
            let live = !t.revoked && t.expires_at_ms.is_none_or(|at| at > now);
            let scoped = t.kind != ClientKind::NativeHost;
            let confirm = state.confirm;
            let key = |name: &'static str| (SharedString::from(format!("{name}-{which:?}")), i);
            let mut actions = div().flex().flex_wrap().gap_1().flex_1().min_w_0();
            if live && confirm.is_some_and(|c| matches!(c, Confirm::AllowWrites(g) | Confirm::Elevate(g) | Confirm::Revoke(g) | Confirm::Reissue(g) if g == id)) {
                let c = confirm.expect("checked");
                let (text, primary_label) = match c {
                    Confirm::AllowWrites(_) => (profiles::MCP_WRITE_WARNING, "Allow writes"),
                    Confirm::Elevate(_) => (ELEVATE_WARNING, "Elevate 15 min"),
                    Confirm::Revoke(_) => ("Clients using this token stop working immediately.", "Revoke"),
                    Confirm::Reissue(_) => ("A new secret replaces this one; the old one stops working now.", "Re-issue"),
                };
                table = table.child(self.api_row(&t, now, cell, div()));
                table = table.child(
                    div()
                        .ml_4()
                        .p_2()
                        .rounded_md()
                        .bg(rgb(WARN_BG))
                        .text_color(rgb(WARN_TEXT))
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().min_w_0().text_size(px(12.)).child(text))
                        .child(primary_button(key("confirm"), primary_label, true).on_click(
                            cx.listener(move |this, _, _, cx| this.api_confirmed(c, cx)),
                        ))
                        .child(button(key("cancel"), "Cancel", true).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.api.confirm = None;
                                cx.notify();
                            },
                        ))),
                );
                continue;
            }
            if live && which == Group::Mcp {
                if allows_writes(&t) {
                    actions = actions.child(button(key("ro"), "Make read-only", true).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.api_send(
                                ApiCommand::SetScopes {
                                    grant: id,
                                    scopes: profiles::mcp_read_only(),
                                },
                                ApiOp::Changed,
                            );
                            cx.notify();
                        }),
                    ));
                } else {
                    actions = actions.child(button(key("rw"), "Allow writes…", true).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.api.confirm = Some(Confirm::AllowWrites(id));
                            cx.notify();
                        }),
                    ));
                }
            }
            if live && scoped && which != Group::NativeHosts {
                let elevated = t.elevated_until_ms.is_some_and(|at| at > now);
                let can_elevate = t.scopes.iter().any(|s| {
                    matches!(
                        s,
                        cm_protocol::api::Scope::PapersWrite
                            | cm_protocol::api::Scope::PapersDelete
                    )
                });
                if elevated {
                    actions =
                        actions.child(button(key("unelevate"), "End elevation", true).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.api_send(ApiCommand::EndElevation(id), ApiOp::Changed);
                                cx.notify();
                            }),
                        ));
                } else if can_elevate {
                    actions = actions.child(button(key("elevate"), "Elevate…", true).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.api.confirm = Some(Confirm::Elevate(id));
                            cx.notify();
                        }),
                    ));
                }
                actions = actions.child(button(key("reissue"), "Re-issue…", true).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.api.confirm = Some(Confirm::Reissue(id));
                        cx.notify();
                    }),
                ));
            }
            if !t.revoked {
                actions = actions.child(
                    button(key("revoke"), "Revoke…", true)
                        .text_color(rgb(DANGER))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.api.confirm = Some(Confirm::Revoke(id));
                            cx.notify();
                        })),
                );
            }
            table = table.child(self.api_row(&t, now, cell, actions));
        }
        table
    }

    fn api_row(
        &self,
        t: &TokenInfo,
        now: i64,
        cell: impl Fn(String, f32) -> Div,
        actions: Div,
    ) -> Div {
        let label = if t.kind == ClientKind::Mcp {
            t.label.clone()
        } else {
            format!("{} · {}", t.label, t.kind.name())
        };
        let mut status_text = status(t, now).to_owned();
        if t.kind == ClientKind::Mcp && !t.revoked {
            status_text = format!(
                "{status_text} · {}",
                if allows_writes(t) {
                    "writes"
                } else {
                    "read-only"
                }
            );
        }
        if let Some(until) = t.elevated_until_ms.filter(|at| *at > now) {
            status_text = format!("elevated → {}", &format_time(until)[11..16]);
        }
        div()
            .flex()
            .items_center()
            .gap_2()
            .py(px(3.))
            .border_t_1()
            .border_color(rgb(0xeef1f5))
            .when(t.revoked, |el| el.text_color(rgb(MUTED)))
            .child(cell(label, 150.))
            .child(cell(scope_list(&t.scopes), 210.).text_size(px(12.)))
            .child(cell(format_date(Some(t.created_at_ms)), 86.))
            .child(cell(
                t.last_used_at_ms
                    .map_or("never".into(), |at| format_date(Some(at))),
                86.,
            ))
            .child(cell(format_date(t.expires_at_ms), 86.))
            .child(cell(status_text, 120.))
            .child(actions)
    }
}
