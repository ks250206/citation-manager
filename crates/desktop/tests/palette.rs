//! Pure palette/keymap adapter tests; they do not prove native focus, IME or clipboard behaviour.
use cm_desktop::{
    commands::{Availability, CommandContext, CommandRegistry, DisabledReason, ids, workbench_ids},
    details::citation_text,
    keymap::{Context, DEFAULT_JSON, KeyStroke, Keymap, Mode},
    palette::{MAX_QUERY_BYTES, MAX_RECENT, fuzzy_score, rank, record},
};
use cm_protocol::{
    domain::{Bibliography, Contributor},
    settings::{MAX_RECENT_COMMANDS, Settings},
};

fn connected(registry: &CommandRegistry) -> Vec<&'static str> {
    registry.iter().map(|c| c.id).collect()
}

#[test]
fn fuzzy_matching_prefers_word_starts_and_contiguous_runs() {
    let (np, offsets) = fuzzy_score("np", "New paper", "paper.new").unwrap();
    assert_eq!(offsets, vec![0, 4]);
    let (scattered, _) = fuzzy_score("np", "Open paper", "paper.open").unwrap();
    assert!(np > scattered, "word-start/first-letter match ranks higher");
    assert!(fuzzy_score("zzz", "New paper", "paper.new").is_none());
    // Case-insensitive and whitespace in the query is ignored.
    assert!(fuzzy_score("NEW PAP", "New paper", "paper.new").is_some());
    // ID-only matches rank below any title match.
    let (id_only, highlighted) = fuzzy_score("doi", "Add Paper", "paper.add_by_doi").unwrap();
    assert!(highlighted.is_empty());
    let (title, _) = fuzzy_score("doi", "Add Paper by DOI", "x.y").unwrap();
    assert!(title > id_only);
    // Unicode titles and an over-long query are handled without panicking.
    assert!(fuzzy_score("論文", "論文を追加", "paper.new").is_some());
    assert!(fuzzy_score(&"é".repeat(MAX_QUERY_BYTES), "New paper", "paper.new").is_none());
}

#[test]
fn ranking_puts_recent_first_for_empty_query_and_reports_availability() {
    let registry = CommandRegistry::m1();
    let handlers = connected(&registry);
    let context = CommandContext {
        connected_handlers: &handlers,
        library_ready: true,
        has_selection: false,
        busy: false,
    };
    let recent = vec![
        ids::IMPORT.to_owned(),
        workbench_ids::VIEW_ORGANIZE.to_owned(),
    ];
    let entries = rank(&registry, &context, "", &recent);
    assert_eq!(entries.len(), registry.iter().count());
    assert_eq!(entries[0].id, ids::IMPORT);
    assert_eq!(entries[1].id, workbench_ids::VIEW_ORGANIZE);
    let copy = entries.iter().find(|e| e.id == ids::COPY).unwrap();
    assert_eq!(
        copy.availability,
        Availability::Disabled(DisabledReason::SelectionRequired)
    );
    // #21 and #26 connected the last deferred placeholders: no palette entry is deferred.
    assert!(entries.iter().all(|e| !matches!(
        e.availability,
        Availability::Disabled(DisabledReason::Deferred { .. })
    )));
    let highlight = entries
        .iter()
        .find(|e| e.id == "pdf.highlight_selection")
        .unwrap();
    assert_eq!(
        highlight.availability,
        Availability::Disabled(DisabledReason::SelectionRequired)
    );
    let filtered = rank(&registry, &context, "vim", &recent);
    assert_eq!(filtered[0].id, workbench_ids::TOGGLE_VIM);
    assert!(rank(&registry, &context, "qqqq", &recent).is_empty());
}

#[test]
fn history_is_most_recent_first_bounded_and_registered_only() {
    let registry = CommandRegistry::m1();
    let mut recent = vec!["gone.command".to_owned(), ids::NEW.to_owned()];
    record(&mut recent, ids::SAVE, &registry);
    record(&mut recent, ids::NEW, &registry);
    assert_eq!(recent, vec![ids::NEW.to_owned(), ids::SAVE.to_owned()]);
    record(&mut recent, "not.registered", &registry);
    assert_eq!(recent.len(), 2);
    let all: Vec<_> = registry.iter().map(|c| c.id).collect();
    for id in &all {
        record(&mut recent, id, &registry);
    }
    assert_eq!(recent.len(), MAX_RECENT);
    assert_eq!(recent[0], *all.last().unwrap());
    assert_eq!(MAX_RECENT, MAX_RECENT_COMMANDS);
}

#[test]
fn platform_keys_map_to_canonical_tokens() {
    let key = |k: &str, c: Option<&str>, p, ctrl, alt, shift| {
        KeyStroke::from_platform(k, c, p, ctrl, alt, shift).map(|k| k.to_string())
    };
    assert_eq!(
        key("j", Some("j"), false, false, false, false).as_deref(),
        Some("j")
    );
    // Shift is already applied to the produced character.
    assert_eq!(
        key("g", Some("G"), false, false, false, true).as_deref(),
        Some("G")
    );
    assert_eq!(
        key("/", Some("/"), false, false, false, false).as_deref(),
        Some("/")
    );
    assert_eq!(
        key("k", Some("k"), true, false, false, false).as_deref(),
        Some("Cmd+k")
    );
    assert_eq!(
        key("k", Some("K"), true, false, false, true).as_deref(),
        Some("Cmd+Shift+k")
    );
    assert_eq!(
        key("enter", None, false, false, false, false).as_deref(),
        Some("Enter")
    );
    assert_eq!(
        key("escape", None, false, false, false, false).as_deref(),
        Some("Esc")
    );
    // Non-ASCII text input (IME commits etc.) is never a key binding.
    assert_eq!(key("a", Some("あ"), false, false, false, false), None);
    assert_eq!(key("f13", None, false, false, false, false), None);
}

#[test]
fn platform_punctuation_normalizes_into_parser_vocabulary() {
    let key = |k: &str, c: Option<&str>, p, ctrl, alt, shift| {
        KeyStroke::from_platform(k, c, p, ctrl, alt, shift)
    };
    let parsed = |t: &str| Some(KeyStroke::parse(t).unwrap());
    // The plus character is spelled `Plus`; `+` alone is a modifier separator.
    assert_eq!(
        key("+", Some("+"), false, false, false, true),
        parsed("Plus")
    );
    assert_eq!(
        key("+", Some("+"), false, false, false, false),
        parsed("Plus")
    );
    assert_eq!(
        key("+", Some("+"), true, false, false, false),
        parsed("Cmd+Plus")
    );
    assert_eq!(
        key("+", None, true, false, false, false),
        parsed("Cmd+Plus")
    );
    // Shift reported with an unshifted non-letter becomes the produced character.
    assert_eq!(
        key("/", Some("?"), true, false, false, true),
        parsed("Cmd+?")
    );
    assert_eq!(
        key("=", Some("+"), true, false, false, true),
        parsed("Cmd+Plus")
    );
    assert_eq!(
        key("1", Some("!"), false, true, false, true),
        parsed("Ctrl+!")
    );
    // Platforms that already apply Shift to the key are unchanged.
    assert_eq!(
        key("?", Some("?"), true, false, false, false),
        parsed("Cmd+?")
    );
    assert_eq!(
        key("=", Some("="), true, false, false, false),
        parsed("Cmd+=")
    );
    // Without a produced character the layout-dependent result is unknown: no guess.
    assert_eq!(key("/", None, true, false, false, true), None);
    // Letters keep explicit Shift.
    assert_eq!(
        key("k", Some("K"), true, false, false, true),
        parsed("Cmd+Shift+k")
    );
}

#[test]
fn shortcut_labels_come_from_the_active_keymap() {
    let registry = CommandRegistry::m1();
    let keymap = Keymap::parse(DEFAULT_JSON, &registry).unwrap();
    assert_eq!(
        keymap
            .shortcut(ids::FIRST, Context::Library, Mode::Normal)
            .as_deref(),
        Some("g g")
    );
    assert_eq!(
        keymap
            .shortcut(ids::PALETTE, Context::Library, Mode::VimOff)
            .as_deref(),
        Some("Cmd+k")
    );
    assert_eq!(
        keymap.shortcut(ids::NEXT, Context::Library, Mode::VimOff),
        None
    );
}

#[test]
fn citation_text_omits_missing_parts() {
    let mut b = Bibliography {
        year: Some(2017),
        journal: Some("NeurIPS".into()),
        volume: Some("30".into()),
        issue: Some("1".into()),
        pages: Some("5998–6008".into()),
        doi: Some("10.1/x".into()),
        ..Default::default()
    };
    b.authors = vec![
        Contributor {
            given_name: Some("Ashish".into()),
            family_name: Some("Vaswani".into()),
            literal_name: None,
            orcid: None,
            role: "author".into(),
        },
        Contributor {
            given_name: None,
            family_name: None,
            literal_name: Some("Google Brain".into()),
            orcid: None,
            role: "author".into(),
        },
    ];
    assert_eq!(
        citation_text("Attention Is\nAll You Need.", &b),
        "Vaswani, A.; Google Brain (2017). Attention Is All You Need. NeurIPS, 30(1), 5998–6008. https://doi.org/10.1/x"
    );
    assert_eq!(
        citation_text("Untitled", &Bibliography::default()),
        "Untitled."
    );
}

#[test]
fn settings_accept_old_files_and_reject_invalid_history() {
    let old = r#"{"version":1,"columns":[{"name":"Title","visible":true,"width":300},{"name":"Year","visible":true,"width":65},{"name":"Type","visible":true,"width":90},{"name":"DOI","visible":true,"width":180}],"sort":{"field":"Id","direction":"Asc"},"navigation_width":220,"details_width":380,"reading":false,"details_tab":"Info"}"#;
    let settings: Settings = serde_json::from_str(old).unwrap();
    assert!(settings.vim && settings.recent_commands.is_empty() && settings.valid());
    let mut bad = Settings {
        recent_commands: vec!["Not An Id".into()],
        ..Default::default()
    };
    assert!(!bad.valid());
    bad.recent_commands = vec!["paper.new".into(); MAX_RECENT_COMMANDS + 1];
    assert!(!bad.valid());
}
