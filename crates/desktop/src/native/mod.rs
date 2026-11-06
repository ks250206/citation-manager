mod bootstrap_views;
mod text_input;
use self::text_input::TextInput;
use crate::service_client::{LibraryState, Response, response_completion};
use cm_protocol::library::{LibraryClient, LibraryRequest};
use gpui::{prelude::*, *};
use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

actions!(
    library,
    [
        New,
        Save,
        Refresh,
        NextField,
        PreviousField,
        NextPaper,
        PreviousPaper,
        Quit
    ]
);

struct LibraryView {
    client: Arc<dyn LibraryClient>,
    state: LibraryState,
    title: Entity<TextInput>,
    detail: Entity<TextInput>,
    focus: FocusHandle,
    sender: mpsc::Sender<Response>,
    receiver: mpsc::Receiver<Response>,
}
impl LibraryView {
    fn new(client: Arc<dyn LibraryClient>, cx: &mut Context<Self>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let mut view = Self {
            client,
            state: LibraryState::default(),
            title: cx.new(|cx| TextInput::new("Title (required)", cx)),
            detail: cx.new(|cx| TextInput::new("Abstract or short detail", cx)),
            focus: cx.focus_handle(),
            sender,
            receiver,
        };
        for field in [&view.title, &view.detail] {
            cx.subscribe(field, |view, _, _: &text_input::Edited, cx| {
                view.state.edited();
                cx.notify();
            })
            .detach();
        }
        let request = view.state.list(false);
        view.dispatch(request);
        view.lock_editor(cx);
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                if view
                    .update(cx, |view, cx| {
                        while let Ok((id, result)) = view.receiver.try_recv() {
                            if view.state.complete(id, result) {
                                let paper = view.state.selected.as_ref().expect("confirmed paper");
                                let title = paper.title.clone();
                                let detail =
                                    paper.bibliography.abstract_text.clone().unwrap_or_default();
                                view.title.update(cx, |field, cx| field.set_text(title, cx));
                                view.detail
                                    .update(cx, |field, cx| field.set_text(detail, cx));
                            }
                            view.lock_editor(cx);
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        view
    }
    fn dispatch(&mut self, request: Option<(u64, LibraryRequest)>) {
        let Some((id, request)) = request else {
            return;
        };
        let sender = self.sender.clone();
        if let Err(error) = self.client.submit(request, response_completion(sender, id)) {
            let _ = self.sender.send((id, Err(error)));
        }
    }
    fn lock_editor(&mut self, cx: &mut Context<Self>) {
        let enabled = !self.state.editor_busy();
        self.title.update(cx, |field, cx| {
            field.enabled = enabled;
            cx.notify();
        });
        self.detail.update(cx, |field, cx| {
            field.enabled = enabled;
            cx.notify();
        });
    }
    fn new_paper(&mut self, _: &New, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.saving() {
            return;
        }
        self.state.new_paper();
        self.title
            .update(cx, |field, cx| field.set_text(String::new(), cx));
        self.detail
            .update(cx, |field, cx| field.set_text(String::new(), cx));
        self.lock_editor(cx);
        self.title.focus_handle(cx).focus(window, cx);
        cx.notify();
    }
    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        // Finish native composition before saving; do not persist an IME preedit.
        if self.title.read(cx).is_composing() || self.detail.read(cx).is_composing() {
            self.state.message = "Finish text conversion before saving".into();
            self.state.error = false;
            cx.notify();
            return;
        }
        let title = self.title.read(cx).content.to_string();
        let detail = self.detail.read(cx).content.to_string();
        let request = self.state.save(title, detail);
        self.dispatch(request);
        self.lock_editor(cx);
        cx.notify();
    }
    fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        let request = self.state.list(false);
        self.dispatch(request);
        self.lock_editor(cx);
        cx.notify();
    }
    fn focus_next(&mut self, _: &NextField, window: &mut Window, cx: &mut Context<Self>) {
        if self.title.focus_handle(cx).is_focused(window) {
            self.detail.focus_handle(cx).focus(window, cx);
        } else {
            self.title.focus_handle(cx).focus(window, cx);
        }
    }
    fn focus_previous(&mut self, _: &PreviousField, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_next(&NextField, window, cx);
    }
    fn select_index(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(paper) = self.state.papers.get(index) {
            let request = self.state.select(paper.id);
            self.dispatch(request);
            self.lock_editor(cx);
            cx.notify();
        }
    }
    fn next_paper(&mut self, _: &NextPaper, _: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .state
            .selected
            .as_ref()
            .and_then(|s| self.state.papers.iter().position(|p| p.id == s.id));
        self.select_index(
            current.map_or(0, |i| {
                (i + 1).min(self.state.papers.len().saturating_sub(1))
            }),
            cx,
        );
    }
    fn previous_paper(&mut self, _: &PreviousPaper, _: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .state
            .selected
            .as_ref()
            .and_then(|s| self.state.papers.iter().position(|p| p.id == s.id));
        self.select_index(current.unwrap_or(0).saturating_sub(1), cx);
    }
}
fn button(id: &'static str, label: &'static str, enabled: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(if enabled { 0xe0eaff } else { 0xeeeeee }))
        .text_color(rgb(if enabled { 0x163c80 } else { 0x777777 }))
        .cursor_pointer()
        .child(label)
}
impl Render for LibraryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let saving = self.state.saving();
        let loading = self.state.loading();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0xf5f6f8))
            .text_color(rgb(0x1c2533))
            .font_family(".AppleSystemUIFont")
            .text_size(px(15.))
            .track_focus(&self.focus)
            .key_context("Library")
            .on_action(cx.listener(Self::new_paper))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_previous))
            .on_action(cx.listener(Self::next_paper))
            .on_action(cx.listener(Self::previous_paper))
            .child(
                div()
                    .p_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().text_size(px(21.)).child("Citation Manager"))
                    .child(button("new", "New paper  ⌘N", !saving).on_click(
                        cx.listener(|this, _, window, cx| this.new_paper(&New, window, cx)),
                    ))
                    .child(button("refresh", "Reload  ⌘R", !saving).on_click(
                        cx.listener(|this, _, window, cx| this.refresh(&Refresh, window, cx)),
                    )),
            )
            .child(
                div()
                    .px_4()
                    .py_2()
                    .bg(rgb(if self.state.error { 0xffe5e5 } else { 0xe8eff9 }))
                    .child(self.state.message.clone()),
            )
            // Host slots for #15. No collection/filter/import implementation is implied.
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(bootstrap_views::navigation_host())
                    .child(
                        div()
                            .id("library-list")
                            .w(px(264.))
                            .flex_shrink_0()
                            .overflow_y_scroll()
                            .p_4()
                            .border_r_1()
                            .border_color(rgb(0xd5dce5))
                            .child(div().mb_3().text_color(rgb(0x667084)).child("LIBRARY"))
                            .when(self.state.papers.is_empty(), |el| {
                                el.child("No papers loaded. Create a paper or reload.")
                            })
                            .children(self.state.papers.iter().enumerate().map(|(index, paper)| {
                                let selected = self
                                    .state
                                    .selected
                                    .as_ref()
                                    .is_some_and(|p| p.id == paper.id);
                                div()
                                    .id(index)
                                    .p_3()
                                    .mb_2()
                                    .rounded_md()
                                    .bg(rgb(if selected { 0xe0eaff } else { 0xffffff }))
                                    .cursor_pointer()
                                    .child(paper.title.clone())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.select_index(index, cx)
                                    }))
                            }))
                            .when(self.state.has_more, |el| {
                                el.child(button("more", "Load more", !loading).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        if !this.state.loading() {
                                            let request = this.state.list(true);
                                            this.dispatch(request);
                                            this.lock_editor(cx);
                                            cx.notify();
                                        }
                                    }),
                                ))
                            }),
                    )
                    .child(
                        div()
                            .id("detail-host")
                            .flex_1()
                            .min_w_0()
                            .overflow_y_scroll()
                            .p_5()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(div().text_size(px(20.)).child(
                                if self.state.selected.is_some() {
                                    "Paper detail / edit"
                                } else {
                                    "New paper"
                                },
                            ))
                            .child("Title")
                            .child(self.title.clone())
                            .child("Abstract / detail (single line)")
                            .child(self.detail.clone())
                            .child(
                                button("save", "Save locally  ⌘S", !self.state.editor_busy())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save(&Save, window, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .text_color(rgb(0x667084))
                                    .child("Tab: switch fields · ⌘↑ / ⌘↓: choose paper"),
                            )
                            .when_some(self.state.selected.as_ref(), |el, paper| {
                                el.child(format!("ID: {}", paper.id.0)).child(format!(
                                    "Updated: {}",
                                    paper.updated_at.as_deref().unwrap_or("—")
                                ))
                            })
                            .child(bootstrap_views::pdf_host()),
                    ),
            )
    }
}
pub fn run(client: Arc<dyn LibraryClient>) {
    gpui_platform::application().run(move |cx: &mut App| {
        text_input::bind_keys(cx);
        cx.bind_keys([
            KeyBinding::new("cmd-n", New, Some("Library")),
            KeyBinding::new("cmd-s", Save, Some("Library")),
            KeyBinding::new("cmd-r", Refresh, Some("Library")),
            KeyBinding::new("tab", NextField, Some("Library")),
            KeyBinding::new("shift-tab", PreviousField, Some("Library")),
            KeyBinding::new("cmd-down", NextPaper, Some("Library")),
            KeyBinding::new("cmd-up", PreviousPaper, Some("Library")),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1000.), px(620.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(760.), px(480.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Citation Manager — local library".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| LibraryView::new(client, cx)),
            )
            .expect("open native window");
        window
            .update(cx, |view, window, cx| {
                view.focus.focus(window, cx);
                cx.activate(true);
            })
            .expect("activate native window");
    });
}
