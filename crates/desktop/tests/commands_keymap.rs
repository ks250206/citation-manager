//! Pure headless policy tests: none of these establish real GPUI focus/IME acceptance.
use cm_desktop::{
    commands::{
        Availability, Command, CommandContext, CommandRegistry, DisabledReason, RegistryError,
        Requirement, ids,
    },
    keymap::{
        ConfigSource, Context, DiagnosticCode, InputState, KeyResolver, KeyStroke, Keymap,
        MAX_BINDINGS, MAX_CONFIG_BYTES, MAX_JSON_DEPTH, MAX_KEY_BYTES, MAX_SEQUENCE_KEYS, Mode,
        Resolution, SEQUENCE_TIMEOUT_MS, load_or_default,
    },
};
use serde_json::{Value, json};

fn binding(context: &str, mode: &str, keys: &[&str], command: &str) -> Value {
    json!({"context": context, "mode": mode, "keys": keys, "command": command})
}
fn document(bindings: Vec<Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({"version": 1, "bindings": bindings})).unwrap()
}
fn map(bindings: Vec<Value>, registry: &CommandRegistry) -> Keymap {
    Keymap::parse(&document(bindings), registry).unwrap()
}
fn input(context: Context, mode: Mode) -> InputState {
    InputState {
        context,
        mode,
        editing: false,
        composing: false,
    }
}
fn connected() -> CommandContext<'static> {
    CommandContext {
        connected_handlers: &[
            ids::NEXT,
            ids::PREVIOUS,
            ids::FIRST,
            ids::LAST,
            ids::OPEN,
            ids::SEARCH,
            ids::COPY,
            ids::RESTORE_FOCUS,
            ids::PALETTE,
            ids::NEW,
            ids::SAVE,
            ids::DOI,
            ids::IMPORT,
        ],
        library_ready: true,
        has_selection: true,
        busy: false,
    }
}
fn resolve(
    resolver: &mut KeyResolver,
    key: &str,
    state: InputState,
    time: u64,
    registry: &CommandRegistry,
) -> Resolution {
    resolver.resolve(
        &KeyStroke::parse(key).unwrap(),
        state,
        time,
        registry,
        &connected(),
    )
}
fn expect_command(result: Resolution, expected: &str) {
    assert_eq!(
        result,
        Resolution::Command {
            id: expected.into(),
            availability: Availability::Enabled
        }
    );
}
fn has_code(bytes: &[u8], code: DiagnosticCode, registry: &CommandRegistry) {
    assert!(
        Keymap::parse(bytes, registry)
            .unwrap_err()
            .iter()
            .any(|d| d.code == code)
    );
}

#[test]
fn feature_registration_is_additive_and_duplicate_failure_preserves_original() {
    let mut registry = CommandRegistry::m1();
    let count = registry.iter().count();
    let feature = Command {
        id: "test.inspect",
        title: "日本語 🔬",
        description: "Ångström",
        requirement: Requirement::Ui,
    };
    registry.register(feature).unwrap();
    assert_eq!(registry.iter().count(), count + 1);
    assert_eq!(registry.get(feature.id).unwrap().title, "日本語 🔬");
    assert_eq!(
        registry.register(Command {
            title: "replacement",
            ..feature
        }),
        Err(RegistryError::DuplicateId(feature.id))
    );
    assert_eq!(registry.get(feature.id).unwrap().title, feature.title);
    for invalid in ["", "flat", "Bad.id", "a..b", "a.b-", "a.日本語"] {
        assert_eq!(
            registry.register(Command {
                id: invalid,
                ..feature
            }),
            Err(RegistryError::InvalidId(invalid))
        );
    }
    let custom = document(vec![binding("library", "normal", &["x"], feature.id)]);
    assert!(Keymap::parse(&custom, &registry).is_ok());
}

#[test]
fn availability_requires_connection_and_current_resources_without_grant_semantics() {
    let registry = CommandRegistry::m1();
    assert_eq!(
        registry.availability(ids::NEXT, &CommandContext::default()),
        Availability::Disabled(DisabledReason::NotConnected)
    );
    let mut context = connected();
    assert_eq!(
        registry.availability(ids::DOI, &context),
        Availability::Enabled
    );
    context.has_selection = false;
    assert_eq!(
        registry.availability(ids::COPY, &context),
        Availability::Disabled(DisabledReason::SelectionRequired)
    );
    assert_eq!(
        registry.availability(ids::NEW, &context),
        Availability::Enabled
    );
    context.library_ready = false;
    assert_eq!(
        registry.availability(ids::NEXT, &context),
        Availability::Disabled(DisabledReason::LibraryUnavailable)
    );
    assert_eq!(
        registry.availability(ids::PALETTE, &context),
        Availability::Enabled
    );
    context.busy = true;
    assert_eq!(
        registry.availability(ids::SAVE, &context),
        Availability::Disabled(DisabledReason::Busy)
    );
    assert_eq!(
        registry.availability("future.unknown", &context),
        Availability::Disabled(DisabledReason::NotRegistered)
    );
}

#[test]
fn known_pdf_api_commands_remain_disabled_even_when_host_claims_connection() {
    let registry = CommandRegistry::m1();
    for (id, owner) in [
        ("pdf.save", 20),
        ("pdf.highlight_selection", 21),
        ("pdf.show_annotations", 21),
        ("api.open_settings", 26),
    ] {
        let handlers = [id];
        let context = CommandContext {
            connected_handlers: &handlers,
            ..connected()
        };
        match registry.availability(id, &context) {
            Availability::Disabled(DisabledReason::Deferred { issue, reason }) => {
                assert_eq!(issue, owner);
                assert!(!reason.is_empty());
            }
            other => panic!("expected deferred {id}: {other:?}"),
        }
    }
}

#[test]
fn default_navigation_resolves_in_both_contexts_and_command_k_is_mode_independent() {
    let registry = CommandRegistry::m1();
    for context in [Context::Library, Context::Search] {
        for (key, id) in [
            ("j", ids::NEXT),
            ("k", ids::PREVIOUS),
            ("G", ids::LAST),
            ("Enter", ids::OPEN),
            ("/", ids::SEARCH),
            ("y", ids::COPY),
            ("Esc", ids::RESTORE_FOCUS),
        ] {
            let mut resolver = KeyResolver::new(Keymap::defaults(&registry).unwrap());
            expect_command(
                resolve(
                    &mut resolver,
                    key,
                    input(context, Mode::Normal),
                    0,
                    &registry,
                ),
                id,
            );
        }
        let mut resolver = KeyResolver::new(Keymap::defaults(&registry).unwrap());
        assert_eq!(
            resolve(
                &mut resolver,
                "g",
                input(context, Mode::Normal),
                0,
                &registry
            ),
            Resolution::Pending
        );
        expect_command(
            resolve(
                &mut resolver,
                "g",
                input(context, Mode::Normal),
                1,
                &registry,
            ),
            ids::FIRST,
        );
        assert!(!resolver.has_pending());
        for mode in [Mode::Normal, Mode::Insert, Mode::VimOff] {
            expect_command(
                resolve(&mut resolver, "Cmd+k", input(context, mode), 2, &registry),
                ids::PALETTE,
            );
        }
    }
}

#[test]
fn availability_is_evaluated_at_match_time_and_unknown_registry_fails_closed() {
    let registry = CommandRegistry::m1();
    let mut resolver = KeyResolver::new(Keymap::defaults(&registry).unwrap());
    let state = input(Context::Library, Mode::Normal);
    let key = KeyStroke::parse("g").unwrap();
    assert_eq!(
        resolver.resolve(&key, state, 0, &registry, &connected()),
        Resolution::Pending
    );
    assert_eq!(
        resolver.resolve(&key, state, 1, &registry, &CommandContext::default()),
        Resolution::Command {
            id: ids::FIRST.into(),
            availability: Availability::Disabled(DisabledReason::NotConnected),
        }
    );
    assert_eq!(
        resolver.resolve(
            &KeyStroke::parse("j").unwrap(),
            state,
            2,
            &CommandRegistry::default(),
            &connected()
        ),
        Resolution::Command {
            id: ids::NEXT.into(),
            availability: Availability::Disabled(DisabledReason::NotRegistered),
        }
    );
}

#[test]
fn escape_cancels_pending_sequence_and_explicit_reset_discards_it() {
    let registry = CommandRegistry::m1();
    let mut resolver = KeyResolver::new(Keymap::defaults(&registry).unwrap());
    let state = input(Context::Library, Mode::Normal);
    assert_eq!(
        resolve(&mut resolver, "g", state, 0, &registry),
        Resolution::Pending
    );
    assert_eq!(
        resolve(&mut resolver, "Esc", state, 1, &registry),
        Resolution::Cancelled
    );
    assert!(!resolver.has_pending());
    expect_command(
        resolve(&mut resolver, "Esc", state, 2, &registry),
        ids::RESTORE_FOCUS,
    );
    assert_eq!(
        resolve(&mut resolver, "g", state, 3, &registry),
        Resolution::Pending
    );
    resolver.reset();
    assert_eq!(
        resolve(&mut resolver, "g", state, 4, &registry),
        Resolution::Pending
    );
}

#[test]
fn prefix_timeout_backwards_time_and_context_switch_cannot_finish_old_sequence() {
    let registry = CommandRegistry::m1();
    let state = input(Context::Library, Mode::Normal);
    for second_time in [10 + SEQUENCE_TIMEOUT_MS, 9, u64::MAX] {
        let mut resolver = KeyResolver::new(Keymap::defaults(&registry).unwrap());
        assert_eq!(
            resolve(&mut resolver, "g", state, 10, &registry),
            Resolution::Pending
        );
        assert_eq!(
            resolve(&mut resolver, "g", state, second_time, &registry),
            Resolution::Pending
        );
    }
    let mut resolver = KeyResolver::new(Keymap::defaults(&registry).unwrap());
    assert_eq!(
        resolve(&mut resolver, "g", state, 0, &registry),
        Resolution::Pending
    );
    assert_eq!(
        resolve(
            &mut resolver,
            "g",
            input(Context::Search, Mode::Normal),
            1,
            &registry
        ),
        Resolution::Pending
    );
    expect_command(
        resolve(
            &mut resolver,
            "g",
            input(Context::Search, Mode::Normal),
            2,
            &registry,
        ),
        ids::FIRST,
    );
}

#[test]
fn mismatch_passes_unknown_event_and_retries_known_event_without_stale_prefix() {
    let registry = CommandRegistry::m1();
    let state = input(Context::Library, Mode::Normal);
    let mut resolver = KeyResolver::new(Keymap::defaults(&registry).unwrap());
    assert_eq!(
        resolve(&mut resolver, "g", state, 0, &registry),
        Resolution::Pending
    );
    assert_eq!(
        resolve(&mut resolver, "x", state, 1, &registry),
        Resolution::PassThrough
    );
    assert!(!resolver.has_pending());
    assert_eq!(
        resolve(&mut resolver, "g", state, 2, &registry),
        Resolution::Pending
    );
    expect_command(resolve(&mut resolver, "j", state, 3, &registry), ids::NEXT);
}

#[test]
fn editing_and_ime_pass_every_event_and_reset_prefix_even_for_escape_and_command_k() {
    let registry = CommandRegistry::m1();
    let state = input(Context::Library, Mode::Normal);
    for (editing, composing) in [(true, false), (false, true), (true, true)] {
        for key in [
            "j",
            "g",
            "G",
            "Enter",
            "/",
            "y",
            "Esc",
            "Cmd+k",
            "Ctrl+v",
            "Alt+e",
            "Backspace",
            "Shift+Left",
        ] {
            let mut resolver = KeyResolver::new(Keymap::defaults(&registry).unwrap());
            assert_eq!(
                resolve(&mut resolver, "g", state, 0, &registry),
                Resolution::Pending
            );
            assert_eq!(
                resolve(
                    &mut resolver,
                    key,
                    InputState {
                        editing,
                        composing,
                        ..state
                    },
                    1,
                    &registry
                ),
                Resolution::PassThrough
            );
            assert!(!resolver.has_pending());
            assert_eq!(
                resolve(&mut resolver, "g", state, 2, &registry),
                Resolution::Pending
            );
        }
    }
}

#[test]
fn normal_insert_vim_off_transitions_preserve_editing_keys_and_drop_sequences() {
    let registry = CommandRegistry::m1();
    // Valid Insert/VimOff shortcut maps still pass ordinary editing events through.
    for mode in [Mode::Insert, Mode::VimOff] {
        let mode_name = if mode == Mode::Insert {
            "insert"
        } else {
            "vim_off"
        };
        let bindings = vec![
            binding("library", "normal", &["g", "g"], ids::FIRST),
            binding("library", mode_name, &["Cmd+k"], ids::PALETTE),
        ];
        let mut resolver = KeyResolver::new(map(bindings, &registry));
        let normal = input(Context::Library, Mode::Normal);
        assert_eq!(
            resolve(&mut resolver, "g", normal, 0, &registry),
            Resolution::Pending
        );
        for key in [
            "j",
            "g",
            "G",
            "Enter",
            "/",
            "y",
            "Esc",
            "Tab",
            "Backspace",
            "Left",
            "Alt+e",
        ] {
            assert_eq!(
                resolve(
                    &mut resolver,
                    key,
                    input(Context::Library, mode),
                    1,
                    &registry
                ),
                Resolution::PassThrough
            );
        }
        assert_eq!(
            resolve(&mut resolver, "g", normal, 2, &registry),
            Resolution::Pending
        );
    }
}

#[test]
fn custom_contexts_and_modes_do_not_leak_bindings() {
    let registry = CommandRegistry::m1();
    let mut resolver = KeyResolver::new(map(
        vec![
            binding("library", "normal", &["x"], ids::NEW),
            binding("search", "normal", &["x"], ids::SEARCH),
            binding("library", "vim_off", &["Cmd+x"], ids::PALETTE),
        ],
        &registry,
    ));
    expect_command(
        resolve(
            &mut resolver,
            "x",
            input(Context::Library, Mode::Normal),
            0,
            &registry,
        ),
        ids::NEW,
    );
    expect_command(
        resolve(
            &mut resolver,
            "x",
            input(Context::Search, Mode::Normal),
            1,
            &registry,
        ),
        ids::SEARCH,
    );
    assert_eq!(
        resolve(
            &mut resolver,
            "x",
            input(Context::Library, Mode::Insert),
            2,
            &registry
        ),
        Resolution::PassThrough
    );
    assert_eq!(
        resolve(
            &mut resolver,
            "Cmd+x",
            input(Context::Library, Mode::Normal),
            3,
            &registry
        ),
        Resolution::PassThrough
    );
    expect_command(
        resolve(
            &mut resolver,
            "Cmd+x",
            input(Context::Library, Mode::VimOff),
            4,
            &registry,
        ),
        ids::PALETTE,
    );
}

#[test]
fn unknown_commands_invalid_keys_and_duplicates_reject_entire_document_before_default_fallback() {
    let registry = CommandRegistry::m1();
    for (bad, code) in [
        (
            binding("library", "normal", &["z"], "future.not_registered"),
            DiagnosticCode::UnknownCommand,
        ),
        (
            binding("library", "normal", &["日本語"], ids::NEW),
            DiagnosticCode::InvalidKey,
        ),
        (
            binding("library", "normal", &["x"], ids::SAVE),
            DiagnosticCode::DuplicateBinding,
        ),
        (
            binding("library", "normal", &["x", "x"], ids::SAVE),
            DiagnosticCode::PrefixConflict,
        ),
    ] {
        let bytes = document(vec![binding("library", "normal", &["x"], ids::NEW), bad]);
        let loaded = load_or_default(&bytes, &registry).unwrap();
        assert_eq!(loaded.source, ConfigSource::Defaults);
        assert!(loaded.diagnostics.iter().any(|d| d.code == code));
        assert_eq!(loaded.keymap, Keymap::defaults(&registry).unwrap());
        let mut resolver = KeyResolver::new(loaded.keymap);
        assert_eq!(
            resolve(
                &mut resolver,
                "x",
                input(Context::Library, Mode::Normal),
                0,
                &registry
            ),
            Resolution::PassThrough
        );
    }
}

#[test]
fn prefix_conflicts_are_order_independent_but_sibling_sequences_and_different_scopes_are_valid() {
    let registry = CommandRegistry::m1();
    for keys in [
        vec![vec!["g"], vec!["g", "g"]],
        vec![vec!["g", "g"], vec!["g"]],
    ] {
        let bytes = document(
            keys.iter()
                .map(|k| binding("library", "normal", k, ids::NEW))
                .collect(),
        );
        let errors = Keymap::parse(&bytes, &registry).unwrap_err();
        assert_eq!(errors[0].code, DiagnosticCode::PrefixConflict);
        assert_eq!(errors[0].path, "$.bindings[1].keys");
        assert_eq!(errors[0].related_binding, Some(0));
    }
    let mut resolver = KeyResolver::new(map(
        vec![
            binding("library", "normal", &["g", "g"], ids::FIRST),
            binding("library", "normal", &["g", "t"], ids::LAST),
            binding("search", "normal", &["g"], ids::SEARCH),
            binding("library", "normal", &["Cmd+g", "Cmd+g"], ids::FIRST),
            binding("library", "vim_off", &["Cmd+g"], ids::NEW),
        ],
        &registry,
    ));
    let state = input(Context::Library, Mode::Normal);
    assert_eq!(
        resolve(&mut resolver, "g", state, 0, &registry),
        Resolution::Pending
    );
    expect_command(resolve(&mut resolver, "t", state, 1, &registry), ids::LAST);
    assert_eq!(
        resolve(&mut resolver, "Cmd+g", state, 2, &registry),
        Resolution::Pending
    );
    expect_command(
        resolve(
            &mut resolver,
            "Cmd+g",
            input(Context::Library, Mode::VimOff),
            3,
            &registry,
        ),
        ids::NEW,
    );
}

#[test]
fn shifted_punctuation_aliases_reject_the_whole_map_before_fallback() {
    let registry = CommandRegistry::m1();
    let bytes = document(vec![
        binding("library", "normal", &["Cmd+?"], ids::PALETTE),
        binding("library", "normal", &["Cmd+Shift+/"], ids::COPY),
    ]);
    let loaded = load_or_default(&bytes, &registry).unwrap();
    assert_eq!(loaded.source, ConfigSource::Defaults);
    assert_eq!(loaded.keymap, Keymap::defaults(&registry).unwrap());
    assert_eq!(loaded.diagnostics.len(), 1);
    assert_eq!(loaded.diagnostics[0].code, DiagnosticCode::InvalidKey);
    assert_eq!(loaded.diagnostics[0].path, "$.bindings[1].keys[0]");
}

#[test]
fn unreachable_sequences_are_rejected_but_modified_escape_and_shortcuts_work() {
    let registry = CommandRegistry::m1();
    for mode in ["insert", "vim_off"] {
        for keys in [&["j"][..], &["Cmd+k", "j"][..], &["Alt+e"][..]] {
            let bytes = document(vec![binding("library", mode, keys, ids::NEW)]);
            let loaded = load_or_default(&bytes, &registry).unwrap();
            assert_eq!(loaded.source, ConfigSource::Defaults);
            assert_eq!(
                loaded.diagnostics[0].code,
                DiagnosticCode::UnreachableBinding
            );
        }
    }
    for keys in [&["g", "Esc"][..], &["Cmd+k", "Esc", "x"][..]] {
        let errors = Keymap::parse(
            &document(vec![binding("library", "normal", keys, ids::NEW)]),
            &registry,
        )
        .unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, DiagnosticCode::UnreachableBinding);
        assert_eq!(errors[0].path, "$.bindings[0].keys[1]");
    }
    let mut resolver = KeyResolver::new(map(
        vec![
            binding("library", "normal", &["g", "Ctrl+Esc"], ids::FIRST),
            binding("library", "normal", &["Esc", "g"], ids::LAST),
            binding("library", "insert", &["Cmd+k", "Ctrl+x"], ids::PALETTE),
        ],
        &registry,
    ));
    let normal = input(Context::Library, Mode::Normal);
    assert_eq!(
        resolve(&mut resolver, "g", normal, 0, &registry),
        Resolution::Pending
    );
    expect_command(
        resolve(&mut resolver, "Ctrl+Esc", normal, 1, &registry),
        ids::FIRST,
    );
    assert_eq!(
        resolve(&mut resolver, "Esc", normal, 2, &registry),
        Resolution::Pending
    );
    expect_command(resolve(&mut resolver, "g", normal, 3, &registry), ids::LAST);
    let insert = input(Context::Library, Mode::Insert);
    assert_eq!(
        resolve(&mut resolver, "Cmd+k", insert, 4, &registry),
        Resolution::Pending
    );
    expect_command(
        resolve(&mut resolver, "Ctrl+x", insert, 5, &registry),
        ids::PALETTE,
    );
}

#[test]
fn modifier_aliases_cannot_hide_duplicates_and_invalid_key_spellings_are_rejected() {
    let registry = CommandRegistry::m1();
    has_code(
        &document(vec![
            binding("library", "normal", &["Cmd+Ctrl+k"], ids::NEW),
            binding("library", "normal", &["Ctrl+Cmd+k"], ids::SAVE),
        ]),
        DiagnosticCode::DuplicateBinding,
        &registry,
    );
    for key in [
        "",
        " ",
        "+",
        "Cmd+",
        "cmd+k",
        "Cmd+Cmd+k",
        "Cmd+K",
        "Shift+g",
        "Shift+?",
        "Cmd+Shift+/",
        "Ctrl+Shift+1",
        "Cmd+Shift+?",
        "Cmd+Shift+Plus",
        "F999",
        "Escape",
        "⌘K",
        "é",
        "🙂",
        "g g",
        "Ctrl++",
    ] {
        assert!(KeyStroke::parse(key).is_err(), "unexpectedly valid: {key}");
    }
    assert!(KeyStroke::parse(&"x".repeat(MAX_KEY_BYTES + 1)).is_err());
    for key in [
        "G",
        "?",
        "Plus",
        "Space",
        "Cmd+Shift+k",
        "Cmd+?",
        "Ctrl+!",
        "Cmd+Plus",
        "Ctrl+Alt+Delete",
        "Shift+Tab",
    ] {
        assert!(KeyStroke::parse(key).is_ok(), "unexpectedly invalid: {key}");
    }
}

#[test]
fn malformed_json_duplicate_fields_and_unknown_schema_fields_fail_with_structured_diagnostics() {
    let registry = CommandRegistry::m1();
    for bytes in [
        b"{".as_slice(),
        b"{} garbage",
        b"\xff",
        b"null",
        b"{\"version\":1,\"bindings\":[],\"unknown\":true}",
        b"{\"version\":1,\"version\":1,\"bindings\":[]}",
        b"{\"version\":1,\"bindings\":null}",
    ] {
        let loaded = load_or_default(bytes, &registry).unwrap();
        assert_eq!(loaded.source, ConfigSource::Defaults);
        assert!(matches!(
            loaded.diagnostics[0].code,
            DiagnosticCode::InvalidJson | DiagnosticCode::InvalidSchema
        ));
        assert!(loaded.diagnostics[0].line.is_some());
    }
    for change in [
        json!({"context":"pdf"}),
        json!({"mode":"visual"}),
        json!({"keys":"gg"}),
        json!({"extra":0}),
    ] {
        let mut raw = binding("library", "normal", &["x"], ids::NEW);
        raw.as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        has_code(
            &document(vec![raw]),
            DiagnosticCode::InvalidSchema,
            &registry,
        );
    }
    let duplicate_nested = br#"{"version":1,"bindings":[{"context":"library","mode":"normal","keys":["j"],"command":"paper.new","command":"paper.save"}]}"#;
    has_code(duplicate_nested, DiagnosticCode::InvalidSchema, &registry);
}

#[test]
fn unsupported_version_is_not_guessed_and_valid_empty_map_is_not_replaced() {
    let registry = CommandRegistry::m1();
    for version in [0, 2, u32::MAX] {
        has_code(
            &serde_json::to_vec(&json!({"version":version,"bindings":[]})).unwrap(),
            DiagnosticCode::UnsupportedVersion,
            &registry,
        );
    }
    let loaded = load_or_default(&document(vec![]), &registry).unwrap();
    assert_eq!(loaded.source, ConfigSource::User);
    assert_eq!(loaded.keymap.binding_count(), 0);
    assert!(loaded.diagnostics.is_empty());
}

#[test]
fn resource_limits_cover_exact_boundaries_and_default_registry_failure_is_explicit() {
    let registry = CommandRegistry::m1();
    let mut at_limit = document(vec![]);
    at_limit.resize(MAX_CONFIG_BYTES, b' ');
    assert!(Keymap::parse(&at_limit, &registry).is_ok());
    at_limit.push(b' ');
    has_code(&at_limit, DiagnosticCode::TooManyBytes, &registry);
    for count in [0, MAX_SEQUENCE_KEYS + 1] {
        has_code(
            &document(vec![binding(
                "library",
                "normal",
                &vec!["g"; count],
                ids::FIRST,
            )]),
            DiagnosticCode::InvalidSequenceLength,
            &registry,
        );
    }
    let keys = vec!["g"; MAX_SEQUENCE_KEYS];
    let mut resolver = KeyResolver::new(map(
        vec![binding("library", "normal", &keys, ids::FIRST)],
        &registry,
    ));
    for i in 0..MAX_SEQUENCE_KEYS - 1 {
        assert_eq!(
            resolve(
                &mut resolver,
                "g",
                input(Context::Library, Mode::Normal),
                i as u64,
                &registry
            ),
            Resolution::Pending
        );
    }
    expect_command(
        resolve(
            &mut resolver,
            "g",
            input(Context::Library, Mode::Normal),
            MAX_SEQUENCE_KEYS as u64,
            &registry,
        ),
        ids::FIRST,
    );
    let unique: Vec<_> = (0..MAX_BINDINGS)
        .map(|i| {
            let keys = [
                format!("{}", (b'a' + (i / 26) as u8) as char),
                format!("{}", (b'a' + (i % 26) as u8) as char),
            ];
            binding("library", "normal", &[&keys[0], &keys[1]], ids::NEW)
        })
        .collect();
    assert_eq!(map(unique.clone(), &registry).binding_count(), MAX_BINDINGS);
    let mut too_many = unique;
    too_many.push(binding("library", "normal", &["z", "z"], ids::NEW));
    has_code(
        &document(too_many),
        DiagnosticCode::TooManyBindings,
        &registry,
    );
    let deep = format!(
        "{}0{}",
        "[".repeat(MAX_JSON_DEPTH + 1),
        "]".repeat(MAX_JSON_DEPTH + 1)
    );
    has_code(deep.as_bytes(), DiagnosticCode::TooDeep, &registry);
    let error = load_or_default(b"bad", &CommandRegistry::default()).unwrap_err();
    assert!(!error.rejected_config.is_empty());
    assert!(
        error
            .invalid_defaults
            .iter()
            .any(|d| d.code == DiagnosticCode::UnknownCommand)
    );
}

#[test]
fn quoted_braces_escapes_and_unicode_are_json_data_not_structural_depth_or_executable_ids() {
    let registry = CommandRegistry::m1();
    let bytes = document(vec![binding(
        "library",
        "normal",
        &["x"],
        "日本語{}[\\\"[[[[[[[[[[[[[[[[[[[[",
    )]);
    has_code(&bytes, DiagnosticCode::UnknownCommand, &registry);
    let diagnostics = Keymap::parse(&bytes, &registry).unwrap_err();
    assert_eq!(diagnostics[0].path, "$.bindings[0].command");
    assert!(!format!("{diagnostics:?}").contains("日本語"));
    for bytes in [
        br#"{"version":1,"bindings":[[[[[[[]]]]]]]}"#.as_slice(),
        b"[[]]",
    ] {
        assert!(
            !Keymap::parse(bytes, &registry)
                .unwrap_err()
                .iter()
                .any(|d| d.code == DiagnosticCode::TooDeep)
        );
    }
}

#[test]
fn known_disabled_command_is_valid_configuration_but_cannot_become_enabled_by_matching() {
    let registry = CommandRegistry::m1();
    let loaded = load_or_default(
        &document(vec![binding("library", "normal", &["p"], "pdf.save")]),
        &registry,
    )
    .unwrap();
    assert_eq!(loaded.source, ConfigSource::User);
    let mut resolver = KeyResolver::new(loaded.keymap);
    match resolve(
        &mut resolver,
        "p",
        input(Context::Library, Mode::Normal),
        0,
        &registry,
    ) {
        Resolution::Command {
            id,
            availability: Availability::Disabled(DisabledReason::Deferred { issue: 20, .. }),
        } => assert_eq!(id, "pdf.save"),
        other => panic!("disabled binding must remain disabled: {other:?}"),
    }
    assert!(!resolver.has_pending());
}
