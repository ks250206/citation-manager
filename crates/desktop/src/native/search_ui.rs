//! #21 cross search results, opening hits at the right attachment/page/characters, in-PDF
//! find over extracted text, PDF Vim keys and `paper://` links.
use super::*;
use crate::{
    deep_links::{DeepLink, Target},
    search::{
        self as presentation, coverage_notes, find_start, focus_text, snippet_runs, source_label,
    },
};
use cm_protocol::search::{
    ExtractionState, FulltextReply, FulltextRequest, PageTextRequest, SearchCoverage, SearchHit,
    SearchPage, SearchReply, SearchRequest, ShortTermScope, UnsearchedScope,
};

/// Stored-text reads per find (50 pages each) before reporting a partial result.
const MAX_FIND_CALLS: u32 = 40;

pub(super) enum SearchOp {
    /// First page (`false`) or "load more" (`true`), tagged with the search sequence it
    /// answers; replies for an older search are dropped.
    Results(bool, u64),
    FindPages {
        attachment: AttachmentId,
        calls: u32,
        generation: u64,
    },
}

/// Where to go once the paper (and its attachment list) has loaded.
#[derive(Clone)]
pub(super) struct OpenTarget {
    pub paper: PaperId,
    pub attachment: Option<AttachmentId>,
    pub page: Option<u32>,
    pub focus: Option<String>,
    pub annotation: Option<String>,
}

/// A query error message with its suggested corrections `(start, end, replacement)`.
pub(super) type QueryError = (String, Vec<(usize, usize, String)>);

pub(super) struct SearchState {
    pub fulltext: bool,
    pub request: Option<SearchRequest>,
    pub hits: Vec<SearchHit>,
    pub total: u32,
    pub coverage: Option<SearchCoverage>,
    pub next: Option<SearchRequest>,
    pub errors: Vec<QueryError>,
    pub loading: bool,
    pub open: Option<OpenTarget>,
    /// Bumped for every new search (not "load more"); stale replies are ignored.
    pub seq: u64,
    /// The query text exactly as sent (trimmed); correction offsets refer to it.
    pub sent_query: String,
}
impl SearchState {
    pub fn new() -> Self {
        Self {
            fulltext: false,
            request: None,
            hits: vec![],
            total: 0,
            coverage: None,
            next: None,
            errors: vec![],
            loading: false,
            open: None,
            seq: 0,
            sent_query: String::new(),
        }
    }
}

pub(super) struct FindState {
    pub input: Entity<TextInput>,
    pub open: bool,
    pub query: String,
    pub attachment: Option<AttachmentId>,
    /// Pages with matches (from stored text), in order, with their counts.
    pub pages: Vec<(u32, usize)>,
    pub current: usize,
    pub status: String,
    /// Stored text unavailable: matches come only from pages already rendered.
    pub loaded_only: bool,
    pub last_g: Option<std::time::Instant>,
    /// Page the find was started from (an opened hit's page): the cursor starts at the
    /// first match on or after it instead of jumping to the first match.
    pub anchor: Option<u32>,
    /// Bumped when a find starts, is closed or its attachment goes away; replies from an
    /// older chain are dropped so two chains never merge their pages.
    pub generation: u64,
}
impl FindState {
    pub fn new(cx: &mut Context<LibraryView>) -> Self {
        Self {
            input: cx.new(|cx| TextInput::new("Find in PDF", cx)),
            open: false,
            query: String::new(),
            attachment: None,
            pages: vec![],
            current: 0,
            status: String::new(),
            loaded_only: false,
            last_g: None,
            anchor: None,
            generation: 0,
        }
    }
}

impl LibraryView {
    fn search_send(&mut self, request: SearchRequest, more: bool) {
        let sender = self.sender.clone();
        if !more {
            self.find_search.seq += 1;
            self.find_search.next = None;
            self.find_search.total = 0;
        }
        let seq = self.find_search.seq;
        self.find_search.loading = true;
        let follow = request.clone();
        let sent = self.client.search(
            request,
            Box::new(move |result| {
                let _ = sender.send(Message {
                    lane: Lane::Pdf,
                    ticket: 0,
                    event: Event::Search(Box::new(SearchOp::Results(more, seq)), Box::new(result)),
                });
            }),
        );
        self.find_search.request = Some(follow);
        if sent.is_err() {
            self.find_search.loading = false;
            self.message = "Full-text search needs the library service".into();
            self.error = true;
        }
    }
    /// Run a full-text search for the search field within the current sidebar scope.
    pub(super) fn run_fulltext(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).content.trim().to_owned();
        if query.is_empty() {
            self.status(
                "Type something to search titles, abstracts, notes and PDF text",
                false,
                cx,
            );
            return;
        }
        let filter = &self.library.query.filter;
        let request = presentation::request(
            &query,
            filter.collection,
            filter.visibility == PaperVisibility::Deleted,
            false,
        );
        self.find_search.hits.clear();
        self.find_search.errors.clear();
        self.find_search.coverage = None;
        self.find_search.sent_query = query.clone();
        self.search_send(request, false);
        self.status(format!("Searching for “{query}”…"), false, cx);
    }
    fn load_more(&mut self, cx: &mut Context<Self>) {
        if let Some(next) = self.find_search.next.clone() {
            self.search_send(next, true);
            cx.notify();
        }
    }
    fn search_all_pages(&mut self, cx: &mut Context<Self>) {
        if let Some(mut request) = self.find_search.request.clone() {
            request.short_term_scope = ShortTermScope::AllPages;
            request.cursor = None;
            self.find_search.hits.clear();
            self.find_search.coverage = None;
            self.search_send(request, false);
            cx.notify();
        }
    }
    pub(super) fn search_reply(
        &mut self,
        op: SearchOp,
        result: Result<SearchReply, ServiceError>,
        cx: &mut Context<Self>,
    ) {
        if let SearchOp::Results(_, seq) = op
            && seq != self.find_search.seq
        {
            return;
        }
        self.find_search.loading = false;
        match (op, result) {
            (SearchOp::Results(more, _), Ok(SearchReply::Results(page))) => {
                let SearchPage {
                    hits,
                    next_page,
                    total_hits,
                    coverage,
                } = *page;
                if !more {
                    self.find_search.hits.clear();
                }
                self.find_search.hits.extend(hits);
                self.find_search.total = total_hits;
                let cursor = next_page.or(coverage.continuation);
                self.find_search.next = cursor.and_then(|c| {
                    self.find_search.request.clone().map(|mut r| {
                        r.cursor = Some(c);
                        r
                    })
                });
                self.find_search.coverage = Some(coverage);
                self.status(
                    format!("{} result(s)", self.find_search.hits.len()),
                    false,
                    cx,
                );
            }
            (SearchOp::Results(..), Ok(SearchReply::Invalid(errors))) => {
                self.find_search.hits.clear();
                self.find_search.next = None;
                self.find_search.total = 0;
                self.find_search.coverage = None;
                self.find_search.errors = errors
                    .diagnostics
                    .into_iter()
                    .map(|d| {
                        (
                            d.message,
                            d.corrections
                                .into_iter()
                                .map(|c| (c.start, c.end, c.replacement))
                                .collect(),
                        )
                    })
                    .collect();
                self.status(
                    "The search needs a small fix — see the suggestions",
                    true,
                    cx,
                );
            }
            (SearchOp::Results(..), Err(e)) => {
                self.find_search.next = None;
                self.status(format!("Search failed: {e}"), true, cx)
            }
            (SearchOp::FindPages { .. }, _) => {}
        }
        cx.notify();
    }
    /// Apply a parser correction to the query (byte range into the original text).
    fn apply_correction(
        &mut self,
        start: usize,
        end: usize,
        replacement: String,
        cx: &mut Context<Self>,
    ) {
        // Offsets refer to the query as sent (trimmed), not the raw field text.
        let query = self.find_search.sent_query.clone();
        if start <= end
            && end <= query.len()
            && query.is_char_boundary(start)
            && query.is_char_boundary(end)
        {
            let fixed = format!("{}{}{}", &query[..start], replacement, &query[end..]);
            self.search.update(cx, |f, cx| f.set_text(fixed, cx));
            self.run_fulltext(cx);
        }
    }
    /// Open a hit (or a deep link): load the paper, then its attachment and page.
    pub(super) fn open_target(&mut self, target: OpenTarget, cx: &mut Context<Self>) {
        if self.dirty {
            self.status(
                "Save or discard metadata edits before opening another paper",
                true,
                cx,
            );
            return;
        }
        let paper = target.paper;
        self.settings.details_tab = if target.attachment.is_some() {
            "PDF".into()
        } else if target.annotation.is_some() {
            "Notes".into()
        } else {
            "Info".into()
        };
        self.find_search.open = Some(target);
        self.call(Lane::Editor, Event::Loaded, |c, done| {
            c.submit(LibraryRequest::Get(paper), done)
        });
        cx.notify();
    }
    /// Called when the attachment list of the opened paper arrives.
    pub(super) fn continue_open(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.find_search.open.clone() else {
            return;
        };
        if self.editor.as_ref().map(|p| p.id) != Some(target.paper) {
            return;
        }
        self.find_search.open = None;
        if let Some(annotation) = &target.annotation
            && let Ok(id) = uuid::Uuid::parse_str(annotation)
        {
            let id = cm_protocol::annotations::AnnotationId(id);
            self.ann_select_when_loaded(id, cx);
        }
        let Some(attachment) = target.attachment else {
            return;
        };
        if !self
            .host
            .attachments
            .iter()
            .any(|(a, _)| a.id == attachment)
        {
            self.status("That PDF is no longer attached to this paper", true, cx);
            return;
        }
        self.host.selected = Some(attachment);
        if let Some(page) = target.page {
            self.pending_jump = Some((attachment, page));
        }
        if let Some(text) = target.focus {
            self.find
                .input
                .update(cx, |f, cx| f.set_text(text.clone(), cx));
            self.find.open = true;
            // Keep the hit's page even when the text also occurs earlier in the PDF.
            self.start_find(attachment, text, target.page, cx);
        }
        cx.notify();
    }
    fn open_hit(&mut self, hit: &SearchHit, cx: &mut Context<Self>) {
        self.open_target(
            OpenTarget {
                paper: hit.paper_id,
                attachment: hit.attachment_id,
                page: hit.page_index,
                focus: (hit.source == cm_protocol::search::DocumentSource::Page
                    && hit.page_index.is_some())
                .then(|| focus_text(hit))
                .flatten(),
                annotation: hit.annotation_id.clone(),
            },
            cx,
        );
    }
    pub(super) fn open_link(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(link) = DeepLink::parse(text) else {
            self.status("The clipboard does not contain a paper:// link", true, cx);
            return;
        };
        let (attachment, page, annotation) = match link.target {
            Target::Paper => (None, None, None),
            Target::Attachment(a) => (Some(a), None, None),
            Target::Page(a, p) => (Some(a), Some(p), None),
            Target::Annotation(n) => (None, None, Some(n.to_string())),
        };
        self.open_target(
            OpenTarget {
                paper: link.paper,
                attachment,
                page,
                focus: None,
                annotation,
            },
            cx,
        );
    }
    pub(super) fn copy_link(&mut self, cx: &mut Context<Self>) {
        let Some(paper) = self.editor.as_ref().map(|p| p.id) else {
            self.status("Select a paper first", true, cx);
            return;
        };
        let target = match (&self.viewer, self.settings.details_tab.as_str()) {
            (Some(v), "PDF") => Target::Page(v.attachment_id(), v.current_page()),
            _ => Target::Paper,
        };
        let link = DeepLink { paper, target }.to_string();
        cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
        self.status(format!("Copied {link}"), false, cx);
    }

    // ---- In-PDF find over stored (extracted) page text ----

    pub(super) fn start_find(
        &mut self,
        attachment: AttachmentId,
        query: String,
        anchor: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        self.find.generation += 1;
        self.find.anchor = anchor;
        self.find.query = query.trim().to_owned();
        self.find.attachment = Some(attachment);
        self.find.pages.clear();
        self.find.current = 0;
        self.find.loaded_only = false;
        if self.find.query.is_empty() {
            self.find.status.clear();
            cx.notify();
            return;
        }
        self.find.status = "Searching…".into();
        self.find_pages(attachment, 0, 0);
        cx.notify();
    }
    fn find_pages(&mut self, attachment: AttachmentId, from_page: u32, calls: u32) {
        let sender = self.sender.clone();
        let generation = self.find.generation;
        let request = FulltextRequest::Pages(PageTextRequest {
            attachment,
            from_page,
            max_pages: 50,
            max_chars: 400_000,
        });
        let sent = self.client.fulltext(
            request,
            Box::new(move |result| {
                let _ = sender.send(Message {
                    lane: Lane::Pdf,
                    ticket: 0,
                    event: Event::Find(
                        Box::new(SearchOp::FindPages {
                            attachment,
                            calls,
                            generation,
                        }),
                        Box::new(result),
                    ),
                });
            }),
        );
        if sent.is_err() {
            self.find.loaded_only = true;
            self.find.status = "Full text unavailable; searching rendered pages only".into();
        }
    }
    pub(super) fn find_reply(
        &mut self,
        op: SearchOp,
        result: Result<FulltextReply, ServiceError>,
        cx: &mut Context<Self>,
    ) {
        let SearchOp::FindPages {
            attachment,
            calls,
            generation,
        } = op
        else {
            return;
        };
        if generation != self.find.generation
            || self.find.attachment != Some(attachment)
            || self.find.query.is_empty()
        {
            return;
        }
        match result {
            Ok(FulltextReply::Pages(read)) => {
                let state = read.extraction.state;
                for page in &read.pages {
                    let n = presentation::count_in_text(&page.text, &self.find.query);
                    if n > 0 {
                        self.find.pages.push((page.page_index, n));
                    }
                }
                let indexed = matches!(state, ExtractionState::Complete | ExtractionState::Partial);
                let capped = read.next_page.is_some() && calls + 1 >= MAX_FIND_CALLS;
                if let Some(next) = read.next_page
                    && !capped
                {
                    self.find_pages(attachment, next, calls + 1)
                }
                if read.next_page.is_none() || capped {
                    let total: usize = self.find.pages.iter().map(|(_, n)| n).sum();
                    self.find.status = if !indexed && self.find.pages.is_empty() {
                        self.find.loaded_only = true;
                        format!(
                            "PDF text not indexed ({}); matching rendered pages only",
                            extraction_label(state)
                        )
                    } else if total == 0 {
                        "No matches".into()
                    } else {
                        format!("{total} match(es) on {} page(s)", self.find.pages.len())
                    };
                    if capped {
                        self.find.status.push_str(" — stopped after 2000 pages");
                    }
                    if let Some((current, jump)) = find_start(&self.find.pages, self.find.anchor) {
                        self.find.current = current;
                        if let Some(page) = jump {
                            self.pending_jump = Some((attachment, page));
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                self.find.loaded_only = true;
                self.find.status = format!("Full text unavailable ({e}); rendered pages only");
            }
        }
        cx.notify();
    }
    pub(super) fn find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(attachment) = self.find.attachment else {
            return;
        };
        if self.host.selected != Some(attachment) {
            return;
        }
        if self.find.pages.is_empty() {
            return;
        }
        let n = self.find.pages.len();
        self.find.current = if forward {
            (self.find.current + 1) % n
        } else {
            (self.find.current + n - 1) % n
        };
        let page = self.find.pages[self.find.current].0;
        self.pending_jump = Some((attachment, page));
        self.find.status = format!("Page {} of {} with matches", self.find.current + 1, n);
        cx.notify();
    }
    /// Forget find results (and any pending jump) that belong to another attachment.
    pub(super) fn reset_find(&mut self) {
        self.find.generation += 1;
        self.find.anchor = None;
        self.find.query.clear();
        self.find.pages.clear();
        self.find.current = 0;
        self.find.status.clear();
        self.find.attachment = None;
        self.find.loaded_only = false;
    }
    /// Where keyboard focus goes on the Library screen: the table, or the root while the
    /// full-text results (which have no table) are shown.
    pub(super) fn list_focus(&self) -> FocusHandle {
        if self.find_search.fulltext {
            self.focus.clone()
        } else {
            self.table_focus.clone()
        }
    }
    pub(super) fn subscribe_search_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.subscribe_in(
            &self.find.input,
            window,
            |this, _, _: &text_input::Submitted, _, cx| {
                let query = this.find.input.read(cx).content.to_string();
                if this.find.query == query.trim() && !this.find.pages.is_empty() {
                    this.find_step(true, cx);
                } else if let Some(attachment) = this.host.selected {
                    this.start_find(attachment, query, None, cx);
                }
            },
        )
        .detach();
    }
    /// PDF Vim keys in the page area (text fields never reach this handler):
    /// j/k scroll, J/K next/previous page, gg/G first/last page, h highlight selection,
    /// a annotation list, / find, n/N next/previous match.
    pub(super) fn viewer_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.settings.vim || self.palette_open {
            return;
        }
        let m = &event.keystroke.modifiers;
        if m.control || m.platform || m.alt || m.function {
            return;
        }
        let Some(ch) = event.keystroke.key_char.as_deref() else {
            return;
        };
        let gg = self
            .find
            .last_g
            .take()
            .is_some_and(|t| t.elapsed() < Duration::from_millis(1000));
        let handled = match ch {
            "j" | "k" => {
                if let Some(v) = &mut self.viewer {
                    v.scroll_by(if ch == "j" { 80.0 } else { -80.0 });
                }
                true
            }
            "J" | "K" => {
                if let Some(v) = &mut self.viewer {
                    let page = v.current_page();
                    let target = if ch == "J" {
                        page + 1
                    } else {
                        page.saturating_sub(1)
                    };
                    v.jump(target);
                }
                true
            }
            "g" if gg => {
                if let Some(v) = &mut self.viewer {
                    v.jump(0);
                }
                true
            }
            "g" => {
                self.find.last_g = Some(std::time::Instant::now());
                true
            }
            "G" => {
                if let Some(v) = &mut self.viewer {
                    v.jump(u32::MAX);
                }
                true
            }
            "h" => {
                self.run_command(crate::commands::pdf_ids::HIGHLIGHT, window, cx);
                true
            }
            "a" => {
                self.run_command(crate::commands::pdf_ids::SHOW_ANNOTATIONS, window, cx);
                true
            }
            "/" => {
                self.run_command(crate::commands::pdf_ids::FIND, window, cx);
                true
            }
            "n" | "N" => {
                self.find_step(ch == "n", cx);
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// Full-text results replacing the paper table.
    pub(super) fn search_results(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut list = div()
            .id("search-results")
            .flex_1()
            .min_h_0()
            .mx_3()
            .mb_2()
            .p_2()
            .rounded(px(RADIUS.md))
            .border_1()
            .border_color(rgb(tok().border))
            .bg(rgb(tok().surface))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2();
        if self.find_search.loading && self.find_search.hits.is_empty() {
            return list.child(hint("Searching…"));
        }
        for (i, (message, corrections)) in self.find_search.errors.iter().enumerate() {
            list = list.child(
                div()
                    .p_2()
                    .rounded(px(RADIUS.md))
                    .bg(rgb(tok().error_bg))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_color(rgb(tok().error_text))
                            .child(message.clone()),
                    )
                    .children(corrections.iter().enumerate().map(|(j, (s, e, r))| {
                        let (s, e, r) = (*s, *e, r.clone());
                        button(("search-fix", i * 100 + j), format!("Use “{r}”"), true).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.apply_correction(s, e, r.clone(), cx)
                            }),
                        )
                    })),
            );
        }
        if let Some(coverage) = &self.find_search.coverage {
            for note in coverage_notes(coverage) {
                list = list.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded(px(RADIUS.md))
                        .bg(rgb(tok().warn_bg))
                        .text_size(px(dims().font_small))
                        .child(note),
                );
            }
            if coverage
                .unsearched
                .contains(&UnsearchedScope::PdfPagesForShortTerms)
            {
                list = list.child(
                    div().child(
                        button("search-all-pages", "Search all PDF pages too", true)
                            .on_click(cx.listener(|this, _, _, cx| this.search_all_pages(cx))),
                    ),
                );
            }
        }
        if self.find_search.hits.is_empty()
            && self.find_search.errors.is_empty()
            && self.find_search.coverage.is_some()
        {
            list = list.child(hint("No results in the searched areas."));
        }
        for (i, hit) in self.find_search.hits.iter().enumerate() {
            let item = hit.clone();
            let link = DeepLink {
                paper: hit.paper_id,
                target: match (hit.attachment_id, hit.page_index) {
                    (Some(a), Some(p)) => Target::Page(a, p),
                    (Some(a), None) => Target::Attachment(a),
                    _ => Target::Paper,
                },
            }
            .to_string();
            list = list.child(
                div()
                    .id(("search-hit", i))
                    .p_2()
                    .rounded(px(RADIUS.md))
                    .border_1()
                    .border_color(rgb(tok().border))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(tok().hover)))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .on_click(cx.listener(move |this, _, _, cx| this.open_hit(&item, cx)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(hit.paper_title.clone()),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .rounded(px(RADIUS.md))
                                    .bg(rgb(tok().sidebar))
                                    .text_size(px(dims().font_caption))
                                    .child(source_label(hit)),
                            )
                            .when(hit.deleted, |el| el.child(hint("in Trash")))
                            .child(button(("search-link", i), "Copy link", true).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
                                    this.status(format!("Copied {link}"), false, cx);
                                }),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .text_size(px(dims().font_small))
                            .text_color(rgb(tok().muted))
                            .children(snippet_runs(&hit.snippet, &hit.highlights).into_iter().map(
                                |(text, marked)| {
                                    div()
                                        .when(marked, |el| {
                                            el.bg(rgb(tok().match_bg)).text_color(rgb(tok().text))
                                        })
                                        .child(text.replace(['\r', '\n'], " "))
                                },
                            )),
                    ),
            );
        }
        if self.find_search.next.is_some() {
            list = list.child(
                div().child(
                    button("search-more", "Load more", !self.find_search.loading)
                        .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                ),
            );
        }
        list
    }
}

fn extraction_label(state: ExtractionState) -> &'static str {
    match state {
        ExtractionState::Pending => "waiting for extraction",
        ExtractionState::Extracting => "extraction in progress",
        ExtractionState::Complete => "complete",
        ExtractionState::Partial => "partially indexed",
        ExtractionState::NoText => "image-only, no text",
        ExtractionState::Encrypted => "encrypted",
        ExtractionState::Malformed => "damaged",
        _ => "not indexed",
    }
}
