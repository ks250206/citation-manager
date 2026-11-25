//! #84 library list: columns, header sorting, multi-selection, popup menus, bulk actions
//! and the viewer's scroll re-anchoring. All framework-free.
use cm_desktop::{
    bulk::{self, BulkReport},
    columns::{self, ColumnKind},
    library::{ClickKind, Library},
    menu::{Entry, Menu, Outcome},
    pdf_viewer::{PAGE_GAP, page_offsets, rescale_offset},
};
use cm_protocol::{
    domain::{Bibliography, CollectionId, Contributor, Paper, PaperId, TagId},
    library::{LibraryClient, LibraryRequest, Reply, ServiceError},
    library_query::{LibraryPage, LibrarySort, SortDirection, SortField},
    organization::{Organization, OrganizationRequest},
    settings::Settings,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

fn id(n: u128) -> PaperId {
    PaperId(uuid::Uuid::from_u128(n))
}
fn paper(n: u128) -> Paper {
    Paper {
        id: id(n),
        title: format!("paper {n}"),
        external_ids: vec![],
        bibliography: Bibliography::default(),
        created_at: Some("2026-11-20T09:15:00.000Z".into()),
        updated_at: None,
        deleted_at: None,
    }
}
fn person(
    family: Option<&str>,
    literal: Option<&str>,
    given: Option<&str>,
    role: &str,
) -> Contributor {
    Contributor {
        given_name: given.map(Into::into),
        family_name: family.map(Into::into),
        literal_name: literal.map(Into::into),
        orcid: None,
        role: role.into(),
    }
}
/// A loaded page of rows 1..=n.
fn library(n: u128) -> Library {
    let mut library = Library::default();
    library.reload();
    library.complete(
        library.generation,
        LibraryPage {
            papers: (1..=n).map(paper).collect(),
            next: None,
            with_pdf: vec![id(2)],
        },
    );
    library
}
const PLAIN: ClickKind = ClickKind {
    toggle: false,
    range: false,
};
const TOGGLE: ClickKind = ClickKind {
    toggle: true,
    range: false,
};
const RANGE: ClickKind = ClickKind {
    toggle: false,
    range: true,
};
const TOGGLE_RANGE: ClickKind = ClickKind {
    toggle: true,
    range: true,
};
fn ids(ns: &[u128]) -> Vec<PaperId> {
    ns.iter().copied().map(id).collect()
}

#[test]
fn author_cell_is_first_author_plus_et_al_and_skips_editors() {
    let mut b = Bibliography::default();
    assert_eq!(columns::author_label(&b), "");
    b.authors = vec![person(None, Some("Editors"), None, "editor")];
    assert_eq!(columns::author_label(&b), "", "editors are not authors");
    b.authors
        .push(person(Some("Tanaka"), None, Some("Kenji"), "author"));
    assert_eq!(columns::author_label(&b), "Tanaka");
    b.authors
        .push(person(None, Some("CERN Collaboration"), None, "author"));
    assert_eq!(columns::author_label(&b), "Tanaka et al.");
    // Same name preference as the daemon's sort key: family, literal, given.
    b.authors = vec![
        person(Some("  "), Some("ATLAS"), Some("X"), "author"),
        person(Some("B"), None, None, "author"),
    ];
    assert_eq!(columns::author_label(&b), "ATLAS et al.");
    b.authors = vec![person(None, None, Some("Madonna"), "author")];
    assert_eq!(columns::author_label(&b), "Madonna");
}

#[test]
fn cells_columns_and_header_sorting() {
    let mut p = paper(1);
    p.title = "Line\nbreak".into();
    p.bibliography.journal = Some("Nature Energy".into());
    p.bibliography.year = Some(2024);
    p.bibliography.doi = Some("10.1/x".into());
    p.bibliography.paper_type = Some("article".into());
    assert_eq!(ColumnKind::Title.cell(&p, false), "Line break");
    assert_eq!(ColumnKind::Journal.cell(&p, false), "Nature Energy");
    assert_eq!(ColumnKind::Year.cell(&paper(2), false), "—");
    assert_eq!(ColumnKind::Pdf.cell(&p, true), "📄");
    assert_eq!(ColumnKind::Pdf.cell(&p, false), "");
    assert_eq!(ColumnKind::Added.cell(&p, false), "2026-11-20");
    assert_eq!(ColumnKind::Doi.cell(&p, false), "10.1/x");
    assert_eq!(ColumnKind::Type.cell(&p, false), "article");

    // Default layout: Author/Title/Year/Journal/PDF; Type, DOI, Added via the toggles.
    let mut settings = Settings::default();
    assert!(settings.valid());
    let shown: Vec<_> = columns::visible(&settings.columns)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    use ColumnKind::*;
    assert_eq!(shown, [Author, Title, Year, Journal, Pdf]);
    assert!(columns::toggle(&mut settings.columns, Doi));
    assert!(
        !columns::toggle(&mut settings.columns, Title),
        "Title stays"
    );
    assert!(columns::toggle(&mut settings.columns, Pdf));
    assert!(settings.valid(), "toggled layouts are still persistable");
    let shown: Vec<_> = columns::visible(&settings.columns)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(shown, [Author, Title, Year, Journal, Doi]);
    for kind in ColumnKind::ALL {
        assert_eq!(ColumnKind::from_name(kind.name()), Some(kind));
    }
    // Names and order match the persisted settings layout.
    let names: Vec<_> = cm_protocol::settings::COLUMNS.iter().map(|c| c.0).collect();
    let kinds: Vec<_> = ColumnKind::ALL.iter().map(|k| k.name()).collect();
    assert_eq!(names, kinds);

    // Only columns with a daemon sort key are clickable.
    assert_eq!(Author.sort_field(), Some(SortField::Author));
    assert_eq!(Journal.sort_field(), Some(SortField::Journal));
    assert_eq!(Added.sort_field(), Some(SortField::CreatedAt));
    assert_eq!(Pdf.sort_field(), None);
    assert_eq!(Doi.sort_field(), None);
    let start = LibrarySort::default();
    let author = columns::next_sort(start, SortField::Author);
    assert_eq!(author.direction, SortDirection::Asc);
    assert_eq!(columns::sort_arrow(Author, author), " ▲");
    assert_eq!(columns::sort_arrow(Title, author), "");
    let flipped = columns::next_sort(author, SortField::Author);
    assert_eq!(
        (flipped.field, flipped.direction),
        (SortField::Author, SortDirection::Desc)
    );
    assert_eq!(columns::sort_arrow(Author, flipped), " ▼");
    assert_eq!(
        columns::next_sort(flipped, SortField::Author).direction,
        SortDirection::Asc
    );
    // The added date starts with the newest papers.
    let added = columns::next_sort(flipped, SortField::CreatedAt);
    assert_eq!(added.direction, SortDirection::Desc);
    assert_eq!(columns::sort_label(added), "Date added, newest first");
    assert_eq!(columns::sort_label(author), "First author, A→Z");
    assert_eq!(columns::sort_label(start), "Default order");
}

#[test]
fn plain_toggle_and_shift_clicks_follow_desktop_conventions() {
    let mut l = library(8);
    assert!(l.has_pdf(id(2)));
    assert!(!l.has_pdf(id(3)));
    assert_eq!(l.click(id(2), PLAIN), Some(id(2)));
    assert_eq!(l.selected, ids(&[2]));
    // Shift: anchor..clicked, replacing the selection; the clicked row is focused.
    assert_eq!(l.click(id(5), RANGE), Some(id(5)));
    assert_eq!(l.selected_in_order(), ids(&[2, 3, 4, 5]));
    assert_eq!(l.focused(), Some(id(5)));
    // A second Shift-click keeps the same anchor (shrinks, or flips to the other side).
    l.click(id(3), RANGE);
    assert_eq!(l.selected_in_order(), ids(&[2, 3]));
    l.click(id(1), RANGE);
    assert_eq!(l.selected_in_order(), ids(&[1, 2]));
    assert_eq!(l.focused(), Some(id(1)));
    // Cmd/Ctrl adds and removes single rows and moves the anchor.
    assert_eq!(l.click(id(7), TOGGLE), Some(id(7)));
    assert_eq!(l.selected_in_order(), ids(&[1, 2, 7]));
    assert_eq!(
        l.click(id(7), TOGGLE),
        Some(id(1)),
        "focus falls back to the last focused"
    );
    assert_eq!(l.selected_in_order(), ids(&[1, 2]));
    l.click(id(6), TOGGLE);
    // Cmd/Ctrl+Shift adds the anchor..clicked range to what is selected.
    l.click(id(8), TOGGLE_RANGE);
    assert_eq!(l.selected_in_order(), ids(&[1, 2, 6, 7, 8]));
    assert_eq!(l.focused(), Some(id(8)));
    // Toggling everything off leaves nothing focused.
    let mut l = library(3);
    l.click(id(1), TOGGLE);
    assert_eq!(l.click(id(1), TOGGLE), None);
    assert!(l.selected.is_empty());
    // Shift with no anchor on this page selects just that row.
    let mut l = library(3);
    l.select_only(id(99));
    l.click(id(2), RANGE);
    assert_eq!(l.selected, ids(&[2]));
}

#[test]
fn shift_arrows_grow_and_shrink_a_range_from_the_anchor() {
    let mut l = library(6);
    l.click(id(3), PLAIN);
    assert_eq!(l.extend(1), Some(id(4)));
    assert_eq!(l.extend(1), Some(id(5)));
    assert_eq!(l.selected_in_order(), ids(&[3, 4, 5]));
    assert_eq!(l.extend(-1), Some(id(4)));
    assert_eq!(l.selected_in_order(), ids(&[3, 4]));
    // Past the anchor the range flips to the other side.
    l.extend(-1);
    l.extend(-1);
    assert_eq!(l.selected_in_order(), ids(&[2, 3]));
    assert_eq!(l.focused(), Some(id(2)));
    // Plain arrows continue from the moving end; the page edge clamps.
    assert_eq!(l.adjacent(1), Some(id(3)));
    for _ in 0..5 {
        l.extend(-1);
    }
    assert_eq!(l.selected_in_order(), ids(&[1, 2, 3]));
    // Shift+Down with nothing selected starts at the first row.
    let mut l = library(4);
    assert_eq!(l.extend(1), Some(id(1)));
    assert_eq!(l.selected, ids(&[1]));
    assert_eq!(l.extend(1), Some(id(2)));
    assert_eq!(l.selected_in_order(), ids(&[1, 2]));
    // Ctrl+A keeps the focused row focused; a right-click inside the selection only
    // refocuses, outside it replaces the selection.
    let mut l = library(4);
    l.click(id(3), PLAIN);
    assert_eq!(l.select_all(), Some(id(3)));
    assert_eq!(l.selected_in_order(), ids(&[1, 2, 3, 4]));
    assert_eq!(l.focus_within(id(1)), Some(id(1)));
    assert_eq!(l.selected.len(), 4);
    assert_eq!(l.focused(), Some(id(1)));
    l.focus_within(id(99));
    assert_eq!(l.selected, ids(&[99]));
    assert_eq!(l.focused(), None, "not on this page");
}

#[derive(Debug, Clone, PartialEq)]
enum Act {
    A,
    B,
    Sub(u8),
}
fn sample() -> Menu<Act> {
    Menu::new(vec![
        Entry::Note("about".into()),
        Entry::item("A", Act::A).shortcut(Some("Ctrl+A".into())),
        Entry::Separator,
        Entry::item("disabled", Act::B).enabled(false),
        Entry::submenu(
            "More",
            vec![
                Entry::item("one", Act::Sub(1)),
                Entry::Separator,
                Entry::item("two", Act::Sub(2)).checked(true),
            ],
        ),
        Entry::submenu("Empty", Vec::<Entry<Act>>::new()),
        Entry::item("B", Act::B),
    ])
}

#[test]
fn menus_skip_inert_rows_and_navigate_one_submenu_level() {
    let mut m = sample();
    assert_eq!(m.index, 1, "first selectable row");
    m.step(1);
    assert_eq!(m.index, 4, "separator and disabled row skipped");
    m.step(1);
    assert_eq!(m.index, 6, "empty submenu is disabled");
    m.step(1);
    assert_eq!(m.index, 1, "wraps");
    m.step(-1);
    assert_eq!(m.index, 6);
    m.step(-1);
    assert_eq!(m.confirm(), Outcome::Open, "Enter on a submenu opens it");
    assert_eq!(m.sub, Some(0));
    m.step(1);
    assert_eq!(m.sub, Some(2));
    assert_eq!(m.confirm(), Outcome::Run(Act::Sub(2)));
    assert!(m.close_submenu());
    assert_eq!(m.escape(), Outcome::Closed);
    assert!(m.open_submenu());
    assert_eq!(m.escape(), Outcome::Open, "Esc closes the submenu first");
    assert_eq!(m.sub, None);
    m.step(-1);
    assert_eq!(m.confirm(), Outcome::Run(Act::A));
    // Pointer: hovering a submenu row opens it; clicking that row never runs its items.
    let mut m = sample();
    m.hover(4, None);
    assert_eq!(m.sub, Some(0));
    assert_eq!(m.activate(4, None), Outcome::Open);
    assert_eq!(m.activate(4, Some(2)), Outcome::Run(Act::Sub(2)));
    assert_eq!(m.activate(3, None), Outcome::Open, "disabled row");
    assert_eq!(m.activate(1, None), Outcome::Run(Act::A));
    assert_eq!(m.sub, None, "moving to another row closes the submenu");
    // A menu with nothing selectable stays put.
    let mut inert = Menu::<Act>::new(vec![Entry::Note("x".into()), Entry::Separator]);
    inert.step(1);
    assert_eq!(inert.confirm(), Outcome::Open);
}

#[test]
fn bulk_requests_move_within_a_collection_and_add_otherwise() {
    let (from, to) = (
        CollectionId(uuid::Uuid::from_u128(10)),
        CollectionId(uuid::Uuid::from_u128(11)),
    );
    let papers = ids(&[1, 2]);
    let add = bulk::collection_requests(&papers, None, to);
    assert!(matches!(
        add.as_slice(),
        [
            OrganizationRequest::CollectionMember(a, c1, true),
            OrganizationRequest::CollectionMember(b, c2, true),
        ] if *a == id(1) && *b == id(2) && *c1 == to && *c2 == to
    ));
    let moved = bulk::collection_requests(&papers, Some(from), to);
    assert!(matches!(
        moved.as_slice(),
        [OrganizationRequest::MoveCollectionMember(a, f, t), _]
            if *a == id(1) && *f == from && *t == to
    ));
    assert!(bulk::collection_requests(&papers, Some(to), to).is_empty());
    let tag = TagId(uuid::Uuid::from_u128(12));
    assert!(matches!(
        bulk::tag_requests(&papers, tag, false).as_slice(),
        [OrganizationRequest::TagMember(_, t, false), _] if *t == tag
    ));
}

/// Records organization/lifecycle calls; fails the call with the given number.
struct Fake {
    calls: Mutex<Vec<String>>,
    fail_at: Option<usize>,
    reject_at: Option<usize>,
}
impl Fake {
    fn step(&self, label: String) -> Result<Option<ServiceError>, ServiceError> {
        let mut calls = self.calls.lock().unwrap();
        calls.push(label);
        let n = calls.len();
        if self.reject_at == Some(n) {
            return Err(ServiceError::Unavailable);
        }
        Ok((self.fail_at == Some(n)).then(|| ServiceError::InvalidInput("no".into())))
    }
}
impl LibraryClient for Fake {
    fn submit(
        &self,
        _: LibraryRequest,
        _: cm_protocol::library::Completion,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unavailable)
    }
    fn organization(
        &self,
        request: OrganizationRequest,
        complete: Reply<Organization>,
    ) -> Result<(), ServiceError> {
        let failure = self.step(format!("{request:?}"))?;
        // Reply from another thread, like the real worker.
        std::thread::spawn(move || {
            complete(match failure {
                Some(e) => Err(e),
                None => Ok(Organization::default()),
            })
        });
        Ok(())
    }
    fn lifecycle(
        &self,
        paper_id: PaperId,
        deleted: bool,
        complete: Reply<Paper>,
    ) -> Result<(), ServiceError> {
        let failure = self.step(format!("lifecycle {} {deleted}", paper_id.0))?;
        complete(match failure {
            Some(e) => Err(e),
            None => Ok(paper(paper_id.0.as_u128())),
        });
        Ok(())
    }
}
fn run(fake: Fake, steps: Vec<bulk::Step>) -> (BulkReport, Vec<String>) {
    let fake = Arc::new(fake);
    let (tx, rx) = std::sync::mpsc::channel();
    bulk::run(fake.clone(), steps, Box::new(move |r| tx.send(r).unwrap()));
    let report = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap()
        .unwrap();
    // Exactly one report.
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(50))
            .is_err()
    );
    let calls = fake.calls.lock().unwrap().clone();
    (report, calls)
}

#[test]
fn bulk_runs_in_order_and_stops_at_the_first_failure() {
    let tag = TagId(uuid::Uuid::from_u128(5));
    let steps = || {
        bulk::tag_requests(&ids(&[1, 2, 3]), tag, true)
            .into_iter()
            .map(bulk::organization_step)
            .collect::<Vec<_>>()
    };
    let fake = |fail_at, reject_at| Fake {
        calls: Mutex::new(vec![]),
        fail_at,
        reject_at,
    };
    let (report, calls) = run(fake(None, None), steps());
    assert_eq!(
        report,
        BulkReport {
            total: 3,
            done: 3,
            error: None
        }
    );
    assert_eq!(calls.len(), 3);
    assert!(calls[0].contains(&format!("{:?}", id(1))));
    assert_eq!(report.summary("Tagged #x:"), "Tagged #x: 3 papers");
    // A failed reply stops the run: nothing after it is sent.
    let (report, calls) = run(fake(Some(2), None), steps());
    assert_eq!((report.done, calls.len()), (1, 2));
    assert!(report.error.is_some());
    assert!(
        report
            .summary("Tagged #x:")
            .starts_with("Tagged #x: 1 of 3 papers; stopped:")
    );
    // So does a request the client refuses to queue.
    let (report, calls) = run(fake(None, Some(1)), steps());
    assert_eq!((report.done, calls.len()), (0, 1));
    assert_eq!(report.error, Some(ServiceError::Unavailable));
    // Lifecycle steps (Trash / restore), and an empty run reports at once.
    let (report, calls) = run(
        fake(None, None),
        ids(&[7, 8])
            .into_iter()
            .map(|p| bulk::lifecycle_step(p, true))
            .collect(),
    );
    assert_eq!(report.done, 2);
    assert!(calls.iter().all(|c| c.ends_with("true")));
    let (report, _) = run(fake(None, None), vec![]);
    assert_eq!(report.summary("Moved"), "Moved 0 papers");
    let one = BulkReport {
        total: 1,
        done: 1,
        error: None,
    };
    assert_eq!(one.summary("Restored"), "Restored 1 paper");
}

#[test]
fn page_pdf_marks_are_optional_on_the_wire() {
    // A reply from before #84 has no marks and still decodes.
    let old = r#"{"papers":[],"next":null}"#;
    let page: LibraryPage = serde_json::from_str(old).unwrap();
    assert!(page.with_pdf.is_empty());
    // An empty mark list is not sent; a non-empty one round-trips.
    let json = serde_json::to_string(&page).unwrap();
    assert!(!json.contains("with_pdf"), "{json}");
    let marked = LibraryPage {
        with_pdf: ids(&[3]),
        ..page
    };
    let back: LibraryPage = serde_json::from_str(&serde_json::to_string(&marked).unwrap()).unwrap();
    assert_eq!(back, marked);
}

#[test]
fn rescaling_keeps_the_same_page_position_in_view() {
    let sizes: HashMap<u32, (f32, f32)> = HashMap::new();
    let narrow = page_offsets(4, &sizes, 0.5);
    let wide = page_offsets(4, &sizes, 2.0);
    // Halfway down page 3 stays halfway down page 3.
    let (top, height) = narrow[2];
    let moved = rescale_offset(&narrow, &wide, top + height / 2.0);
    assert!(
        (moved - (wide[2].0 + wide[2].1 / 2.0)).abs() < 0.01,
        "{moved}"
    );
    // The very bottom of the last page maps to the bottom of the last page, not past it.
    let (top, height) = narrow[3];
    let bottom = rescale_offset(&narrow, &wide, top + height);
    assert!((bottom - (wide[3].0 + wide[3].1)).abs() < 0.01);
    // In the gap above a page: the same gap above the same page.
    let gap = rescale_offset(&narrow, &wide, narrow[1].0 - PAGE_GAP / 2.0);
    assert!((gap - (wide[1].0 - PAGE_GAP / 2.0)).abs() < 0.01);
    // Top of the document and empty layouts stay at 0.
    assert_eq!(rescale_offset(&narrow, &wide, 0.0), 0.0);
    assert_eq!(rescale_offset(&[], &wide, 100.0), 0.0);
    assert_eq!(rescale_offset(&narrow, &[], 100.0), 0.0);
    // Same scale: unchanged.
    let same = rescale_offset(&narrow, &narrow, 321.0);
    assert!((same - 321.0).abs() < 0.01);
}

/// Review finding 1: re-selecting the open paper after it changed elsewhere must show the
/// stored version, so a later Save cannot send the old bibliography back.
#[test]
fn the_page_version_of_a_changed_open_paper_is_detected() {
    use cm_desktop::library::newer_row;
    let page: Vec<Paper> = (1..=3).map(paper).collect();
    let open = paper(2);
    assert_eq!(newer_row(Some(&open), &page), None, "unchanged");
    assert_eq!(newer_row(None, &page), None, "new draft");
    assert_eq!(newer_row(Some(&paper(9)), &page), None, "not on this page");
    let mut changed = page.clone();
    changed[1].bibliography.journal = Some("Changed elsewhere".into());
    changed[1].updated_at = Some("2026-11-26T00:00:00.000Z".into());
    assert_eq!(newer_row(Some(&open), &changed), Some(&changed[1]));
}

/// Review finding 2: one bulk run at a time, each completion matched to its own run.
#[test]
fn bulk_runs_are_serialized_and_completions_keep_their_own_state() {
    use cm_desktop::bulk::{BulkRun, BulkSlot};
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Lane {
        Organization,
        Editor,
    }
    let tag = BulkRun {
        verb: "Tagged #x:".into(),
        refetch: false,
    };
    let mut pending: HashMap<&str, u64> = HashMap::new();
    fn key(lane: Lane) -> &'static str {
        match lane {
            Lane::Organization => "org",
            Lane::Editor => "editor",
        }
    }
    let mut slot = BulkSlot::default();
    assert!(slot.active(|l| pending.get(key(l)).copied()).is_none());
    pending.insert("org", 7);
    slot.start(Lane::Organization, 7, tag.clone());
    // While it runs, a Trash run on another lane sees it and must not start.
    assert_eq!(slot.active(|l| pending.get(key(l)).copied()), Some(&tag));
    // A completion for another lane/ticket (e.g. the editor's Get) is not this run's.
    assert_eq!(slot.finish(Lane::Editor, 7), None);
    assert_eq!(slot.finish(Lane::Organization, 6), None);
    assert_eq!(slot.finish(Lane::Organization, 7), Some(tag.clone()));
    assert!(slot.active(|l| pending.get(key(l)).copied()).is_none());
    assert_eq!(slot.finish(Lane::Organization, 7), None, "reported once");
    // A run whose ticket was replaced (or whose reply was lost) never blocks later runs.
    slot.start(Lane::Organization, 8, tag);
    pending.insert("org", 9);
    assert!(slot.active(|l| pending.get(key(l)).copied()).is_none());
}

/// Review finding 3: before the paper's memberships arrive, tags are not offered as
/// "add" (which would re-add an existing tag); they are disabled until then.
#[test]
fn tag_entries_wait_for_the_selected_papers_memberships() {
    use cm_desktop::bulk::{TagChoice, tag_choice};
    let (a, b) = (
        TagId(uuid::Uuid::from_u128(20)),
        TagId(uuid::Uuid::from_u128(21)),
    );
    let one = ids(&[1]);
    // Memberships still loading, or loaded for the previously open paper.
    assert_eq!(tag_choice(a, &one, None), TagChoice::Loading);
    assert_eq!(
        tag_choice(a, &one, Some((id(2), &[a][..]))),
        TagChoice::Loading
    );
    // Loaded for this paper: real check boxes.
    let tags = [a];
    assert_eq!(
        tag_choice(a, &one, Some((id(1), &tags[..]))),
        TagChoice::Toggle { present: true }
    );
    assert_eq!(
        tag_choice(b, &one, Some((id(1), &tags[..]))),
        TagChoice::Toggle { present: false }
    );
    // Several papers: "Add #tag" to all, whatever is loaded.
    assert_eq!(tag_choice(a, &ids(&[1, 2]), None), TagChoice::AddToAll);
}
