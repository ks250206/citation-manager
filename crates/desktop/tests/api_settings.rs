//! #26 API & Integrations screen state (headless).
use cm_desktop::api_settings::{
    ApiSettingsState, Confirm, ELEVATION_MINUTES, Group, ISSUABLE, ReplyKind, RevealedSecret,
    SecretText, TokenForm, allows_writes, choose_elevation, elevate_confirm, elevation_kinds,
    elevation_request, format_date, format_time, group, status, toggled_mcp_scopes,
};
use cm_protocol::{
    api::{
        ApiCommand, ApiOverview, ApiSettings, BindAddress, BulkKind, ClientKind, ListenerState,
        Scope, SecretBackend, TokenInfo, profiles, scope_capabilities,
    },
    ipc::GrantId,
};

const DAY: i64 = 86_400_000;

fn token(kind: ClientKind, scopes: &[Scope], revoked: bool) -> TokenInfo {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let id: GrantId =
        serde_json::from_str(&format!("\"01900000-0000-7000-8000-{n:012}\"")).unwrap();
    let scopes: cm_protocol::api::ScopeSet = scopes.iter().copied().collect();
    TokenInfo {
        id,
        label: "t".into(),
        kind,
        capabilities: scope_capabilities(&scopes),
        scopes,
        created_at_ms: 0,
        expires_at_ms: Some(10 * DAY),
        last_used_at_ms: None,
        revoked,
        elevated_until_ms: None,
        elevated_kind: None,
        issued_by: None,
        replaces: None,
    }
}

#[test]
fn issue_form_validates_label_scopes_and_policy_expiry() {
    let mut form = TokenForm::default();
    assert_eq!(form.scopes, [Scope::PapersRead].into());
    assert!(form.request("  ", 0).is_err());
    let request = form.request(" Zotero bridge ", 0).unwrap();
    assert_eq!(request.label, "Zotero bridge");
    assert_eq!(request.expires_at_ms, None, "empty = policy default");
    assert_eq!(form.lifetime_days(), (90, 365));
    form.toggle(Scope::PapersWrite);
    assert_eq!(form.lifetime_days(), (30, 90));
    form.expiry_days = "91".into();
    assert!(form.request("x", 0).unwrap_err().contains("1–90"));
    form.expiry_days = "7".into();
    assert_eq!(
        form.request("x", 5).unwrap().expires_at_ms,
        Some(5 + 7 * DAY)
    );
    form.toggle(Scope::PapersRead);
    form.toggle(Scope::PapersWrite);
    assert!(form.request("x", 0).unwrap_err().contains("scope"));
    assert!(!ISSUABLE.contains(&Scope::BulkElevated));
}

#[test]
fn settings_draft_only_produces_loopback_unprivileged_settings() {
    let mut state = ApiSettingsState::default();
    let overview = ApiOverview {
        settings: ApiSettings::default(),
        secret_backend: SecretBackend::File,
        listener: ListenerState::NotAvailable,
        audit_warning: None,
    };
    state.set_overview(overview.clone(), false);
    assert!(!state.enabled, "off by default");
    assert!(state.settings_request("80").is_err());
    assert!(state.settings_request("port").is_err());
    state.enabled = true;
    state.bind = BindAddress::LoopbackV6;
    let saved = state.settings_request("40001").unwrap();
    assert_eq!(saved.listen_addr().to_string(), "[::1]:40001");
    // A background refresh keeps the unsaved draft; a save reply replaces it.
    state.dirty = true;
    state.set_overview(overview.clone(), false);
    assert!(state.enabled && state.bind == BindAddress::LoopbackV6);
    state.set_overview(overview, true);
    assert!(!state.enabled && !state.dirty);
    assert_eq!(state.secret_backend(), Some(SecretBackend::File));
}

#[test]
fn groups_revoked_rows_and_mcp_write_toggle() {
    let mut state = ApiSettingsState {
        tokens: vec![
            token(ClientKind::Api, &[Scope::PapersRead], false),
            token(ClientKind::Cli, &[Scope::PapersRead], true),
            token(
                ClientKind::Mcp,
                &profiles::mcp_read_only().into_iter().collect::<Vec<_>>(),
                false,
            ),
            token(ClientKind::NativeHost, &[], false),
            token(ClientKind::Desktop, &[], false),
        ],
        ..Default::default()
    };
    assert_eq!(state.rows(Group::Tokens).count(), 1);
    state.show_revoked = true;
    assert_eq!(state.rows(Group::Tokens).count(), 2);
    assert_eq!(state.rows(Group::NativeHosts).count(), 1);
    assert_eq!(group(ClientKind::Desktop), Group::Owner);
    let mcp = state.rows(Group::Mcp).next().unwrap().clone();
    assert!(!allows_writes(&mcp));
    assert_eq!(toggled_mcp_scopes(&mcp), profiles::mcp_write());
    let mut writable = mcp.clone();
    writable.scopes = profiles::mcp_write();
    assert!(allows_writes(&writable));
    assert_eq!(toggled_mcp_scopes(&writable), profiles::mcp_read_only());
    state.upsert(writable.clone());
    assert_eq!(state.token(mcp.id), Some(&writable));
    assert_eq!(status(&writable, 0), "active");
    assert_eq!(status(&writable, 11 * DAY), "expired");
}

#[test]
fn a_revealed_secret_is_redacted_in_debug_and_dropped_on_conceal() {
    let mut state = ApiSettingsState {
        revealed: Some(RevealedSecret {
            token: token(ClientKind::Api, &[Scope::PapersRead], false),
            secret: SecretText::new("ab".repeat(32)),
            stored_in: Some(SecretBackend::Keychain),
            copied: false,
            shown: false,
        }),
        ..Default::default()
    };
    assert!(!format!("{state:?}").contains(&"ab".repeat(32)));
    let revealed = state.revealed.as_ref().unwrap();
    assert!(!revealed.secret.masked().contains('a'), "masked by default");
    assert!(revealed.secret == *"ab".repeat(32).as_str());
    state.conceal();
    assert!(state.revealed.is_none());
}

#[test]
fn stale_list_replies_are_recognised_by_sequence() {
    let mut state = ApiSettingsState::default();
    let first = state.begin(Some(ReplyKind::Tokens));
    let second = state.begin(Some(ReplyKind::Tokens));
    assert!(!state.current(ReplyKind::Tokens, first));
    assert!(state.current(ReplyKind::Tokens, second));
    assert!(state.current(ReplyKind::Audit, 99), "nothing sent yet");
}

#[test]
fn utc_dates() {
    assert_eq!(format_date(Some(0)), "1970-01-01");
    assert_eq!(format_date(None), "never");
    assert_eq!(format_date(Some(1_795_996_800_000)), "2026-11-30");
    assert_eq!(
        format_time(951_782_400_000 + 61_000),
        "2000-02-29 00:01 UTC"
    );
}

/// Codex review (PR #69): a token with both `papers:delete` and `papers:write` must be
/// elevatable for bulk moves too; the chosen kind is the one sent.
#[test]
fn elevation_offers_every_allowed_kind_and_sends_the_chosen_one() {
    let both = token(
        ClientKind::Api,
        &[Scope::PapersRead, Scope::PapersWrite, Scope::PapersDelete],
        false,
    );
    assert_eq!(
        elevation_kinds(&both),
        vec![BulkKind::Delete, BulkKind::Move]
    );
    let opened = elevate_confirm(&both).unwrap();
    assert_eq!(opened, Confirm::Elevate(both.id, BulkKind::Delete));
    let moved = choose_elevation(opened, &both, BulkKind::Move).unwrap();
    assert_eq!(moved, Confirm::Elevate(both.id, BulkKind::Move));
    let Confirm::Elevate(_, kind) = moved else {
        unreachable!()
    };
    assert_eq!(
        elevation_request(&both, kind),
        Some(ApiCommand::Elevate {
            grant: both.id,
            kind: BulkKind::Move,
            duration_ms: ELEVATION_MINUTES * 60_000,
        })
    );
    assert_eq!(
        elevation_request(&both, BulkKind::Delete),
        Some(ApiCommand::Elevate {
            grant: both.id,
            kind: BulkKind::Delete,
            duration_ms: ELEVATION_MINUTES * 60_000,
        })
    );

    // Only the kinds the scopes allow are offered (and sent).
    let write = token(
        ClientKind::Mcp,
        &[Scope::PapersRead, Scope::PapersWrite],
        false,
    );
    assert_eq!(elevation_kinds(&write), vec![BulkKind::Move]);
    assert_eq!(
        elevate_confirm(&write),
        Some(Confirm::Elevate(write.id, BulkKind::Move))
    );
    assert_eq!(
        choose_elevation(
            Confirm::Elevate(write.id, BulkKind::Move),
            &write,
            BulkKind::Delete
        ),
        None
    );
    assert_eq!(elevation_request(&write, BulkKind::Delete), None);
    let annotations = token(ClientKind::Api, &[Scope::AnnotationsWrite], false);
    assert_eq!(elevation_kinds(&annotations), vec![BulkKind::Delete]);
    let read = token(ClientKind::Api, &[Scope::PapersRead], false);
    assert!(elevation_kinds(&read).is_empty());
    assert_eq!(elevate_confirm(&read), None);
    let revoked = token(
        ClientKind::Api,
        &[Scope::PapersWrite, Scope::PapersDelete],
        true,
    );
    assert_eq!(elevate_confirm(&revoked), None);
    // A choice never moves to another token's confirmation.
    assert_eq!(choose_elevation(opened, &write, BulkKind::Move), None);
    assert_eq!(
        choose_elevation(Confirm::Revoke(both.id), &both, BulkKind::Move),
        None
    );
}
