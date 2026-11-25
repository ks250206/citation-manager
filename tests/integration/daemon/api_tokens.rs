//! #26 token/grant management over the #25 daemon: scope table end to end, secrets kept out
//! of the database and logs, revocation and scope changes on live sessions, read-only
//! bypass attempts, elevation refusal, export folders, settings across restart and the
//! CLI bootstrap's TTY rule.
mod support;
use cm_daemon::ipc::start;
use cm_protocol::{
    api::{
        ApiCommand, ApiReply, AuditOutcome, BindAddress, BulkKind, ClientKind, IssueToken, Scope,
        ScopeSet, SecretBackend, TokenInfo, elevatable_kinds, profiles,
    },
    client::{
        ClientError, Session, handshake,
        secrets::{MemorySecretStore, SecretStore},
    },
    exchange::{ExportRequest, ExportSelection, Format},
    ipc::{
        Capability, CapabilitySet, Command, Credential, Denial, GrantCommand, HandshakeRejection,
        Operation, Reply, WireError,
    },
    library::{LibraryRequest, NewPaper, ServiceError},
    library_query::LibraryQuery,
    organization::{CollectionInput, OrganizationRequest},
    search::SearchRequest,
};
use std::{
    path::Path,
    process::{Command as Process, Stdio},
    sync::Arc,
};
use support::*;

const MARKER_TITLE: &str = "Confidential marker title 7f3a";

fn scopes(list: &[Scope]) -> ScopeSet {
    list.iter().copied().collect()
}
fn api(session: &mut Session, command: ApiCommand) -> Result<ApiReply, WireError> {
    match session.call(Command::Api(command)).unwrap() {
        Ok(Reply::Api(reply)) => Ok(reply),
        Ok(other) => panic!("{other:?}"),
        Err(e) => Err(e),
    }
}
fn issue_token(
    admin: &mut Session,
    label: &str,
    kind: ClientKind,
    list: &[Scope],
) -> (TokenInfo, Credential, String) {
    match api(
        admin,
        ApiCommand::IssueToken(IssueToken {
            label: label.into(),
            kind,
            scopes: scopes(list),
            expires_at_ms: None,
        }),
    )
    .unwrap()
    {
        ApiReply::Issued {
            token,
            credential,
            secret_account,
            ..
        } => (token, credential, secret_account),
        other => panic!("{other:?}"),
    }
}
fn forbidden(result: Result<Result<Reply, WireError>, ClientError>, op: Operation) {
    assert_eq!(
        result.unwrap().unwrap_err(),
        WireError::Denied(Denial::Forbidden(op))
    );
}
fn invalid(result: Result<ApiReply, WireError>) -> String {
    match result {
        Err(WireError::Service(ServiceError::InvalidInput(reason))) => reason,
        other => panic!("expected invalid input, got {other:?}"),
    }
}
fn create(session: &mut Session, title: &str) -> Result<cm_protocol::domain::Paper, WireError> {
    match session
        .call(Command::Library(LibraryRequest::Create(Box::new(
            NewPaper::titled(title),
        ))))
        .unwrap()?
    {
        Reply::Library(cm_protocol::library::LibraryReply::Paper(p)) => Ok(*p),
        other => panic!("{other:?}"),
    }
}
fn query(session: &mut Session) -> Result<Reply, WireError> {
    session
        .call(Command::Query(LibraryQuery::default()))
        .unwrap()
}
fn with_memory_store(env: &Env) -> (cm_daemon::ipc::DaemonConfig, Arc<MemorySecretStore>) {
    let store = Arc::new(MemorySecretStore::default());
    let mut config = env.config();
    config.secret_store = Some(store.clone());
    (config, store)
}
/// Every byte the daemon persisted under the library (database, WAL, audit log, state).
fn persisted_bytes(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = vec![];
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(persisted_bytes(&path));
        } else if let Ok(bytes) = std::fs::read(&path) {
            out.push((path.display().to_string(), bytes));
        }
    }
    out
}
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn scoped_tokens_follow_the_table_and_secrets_stay_out_of_the_database_and_logs() {
    let env = Env::new();
    let (config, store) = with_memory_store(&env);
    let daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    create(&mut desktop, MARKER_TITLE).unwrap();

    let (info, read_token, account) = issue_token(
        &mut desktop,
        "reader",
        ClientKind::Api,
        &[Scope::PapersRead],
    );
    assert_eq!(info.scopes, scopes(&[Scope::PapersRead]));
    assert!(info.expires_at_ms.is_some(), "scoped tokens always expire");
    assert_eq!(store.get(&account).unwrap(), Some(read_token.clone()));
    let mut reader = env.session("api", read_token.clone());
    assert!(matches!(query(&mut reader), Ok(Reply::Page(p)) if p.papers.len() == 1));
    assert_eq!(
        create(&mut reader, "nope").unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::PaperCreate))
    );
    // `/search` needs papers:read + files:read + annotations:read (snippets carry text).
    forbidden(
        reader.call(Command::Search(Box::new(SearchRequest::new("marker")))),
        Operation::FulltextSearch,
    );
    forbidden(
        reader.call(Command::Api(ApiCommand::ListTokens)),
        Operation::GrantList,
    );
    forbidden(reader.call(Command::LoadSettings), Operation::SettingsRead);

    let (_, writer_token, _) = issue_token(
        &mut desktop,
        "writer",
        ClientKind::Cli,
        &[Scope::PapersRead, Scope::PapersWrite],
    );
    let mut writer = env.session("cli", writer_token.clone());
    create(&mut writer, "Written through a scoped token").unwrap();
    forbidden(
        writer.call(Command::Lifecycle {
            paper: create(&mut desktop, "keep").unwrap().id,
            deleted: true,
        }),
        Operation::PaperTrash,
    );

    // Listing shows metadata only.
    let ApiReply::Tokens(tokens) = api(&mut desktop, ApiCommand::ListTokens).unwrap() else {
        panic!()
    };
    assert!(tokens.iter().any(|t| t.kind == ClientKind::Desktop));
    assert!(
        tokens
            .iter()
            .any(|t| t.label == "writer" && t.kind == ClientKind::Cli)
    );

    drop((reader, writer, desktop));
    daemon.shutdown();
    // No plaintext secret (desktop credential file aside) and no content in the audit log.
    let desktop_secret = std::fs::read(env.paths().desktop_credential).unwrap();
    let mut audited = false;
    for (path, bytes) in persisted_bytes(&env.library) {
        if path.ends_with("desktop.credential") {
            continue;
        }
        for secret in [&read_token, &writer_token] {
            assert!(!contains(&bytes, secret.0.as_bytes()), "token in {path}");
        }
        assert!(
            !contains(&bytes, &desktop_secret),
            "desktop secret in {path}"
        );
        if path.ends_with("audit.jsonl") {
            assert!(!contains(&bytes, MARKER_TITLE.as_bytes()));
            assert!(!contains(&bytes, b"reader"), "labels are not audited");
            assert!(contains(&bytes, b"\"paper_create\""));
            audited = true;
        }
    }
    assert!(audited, "audit log written");
}

#[test]
fn revocation_and_scope_changes_reach_live_sessions_immediately() {
    let env = Env::new();
    let (config, store) = with_memory_store(&env);
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    let (info, token, account) = issue_token(
        &mut desktop,
        "assistant",
        ClientKind::Mcp,
        &profiles::mcp_read_only().into_iter().collect::<Vec<_>>(),
    );
    let mut mcp = env.session("mcp", token.clone());
    assert!(query(&mut mcp).is_ok());
    assert!(create(&mut mcp, "before allow").is_err());
    // "Allow writes" applies to the established session without reconnecting.
    let ApiReply::Updated(updated) = api(
        &mut desktop,
        ApiCommand::SetScopes {
            grant: info.id,
            scopes: profiles::mcp_write(),
        },
    )
    .unwrap() else {
        panic!()
    };
    assert!(updated.expires_at_ms.unwrap() <= info.expires_at_ms.unwrap());
    create(&mut mcp, "AI write after explicit allow").unwrap();
    // Back to read-only: the very next request is denied.
    api(
        &mut desktop,
        ApiCommand::SetScopes {
            grant: info.id,
            scopes: profiles::mcp_read_only(),
        },
    )
    .unwrap();
    assert_eq!(
        create(&mut mcp, "after read-only").unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::PaperCreate))
    );
    // Revocation: live session refused, secret removed, new handshakes rejected.
    assert_eq!(
        api(&mut desktop, ApiCommand::Revoke(info.id)).unwrap(),
        ApiReply::Revoked(info.id)
    );
    assert_eq!(
        mcp.call(Command::Status).unwrap().unwrap_err(),
        WireError::Denied(Denial::Revoked)
    );
    assert_eq!(store.get(&account).unwrap(), None);
    let mut stream = std::os::unix::net::UnixStream::connect(env.paths().socket).unwrap();
    assert!(matches!(
        handshake(
            &mut stream,
            env.connector_with("mcp", token).hello().unwrap()
        ),
        Err(ClientError::Rejected(HandshakeRejection::Unauthenticated))
    ));
    // A revoked token cannot be changed or re-issued, and the owner's grant is not revocable here.
    assert!(invalid(api(&mut desktop, ApiCommand::Reissue(info.id))).contains("revoked"));
    let own = desktop.welcome.grant;
    assert!(invalid(api(&mut desktop, ApiCommand::Revoke(own))).contains("desktop"));
}

#[test]
fn reissue_rotates_the_secret_and_revokes_the_old_token() {
    let env = Env::new();
    let (config, store) = with_memory_store(&env);
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    let (old, old_token, old_account) = issue_token(
        &mut desktop,
        "rotating",
        ClientKind::Api,
        &[Scope::PapersRead],
    );
    let mut live = env.session("api", old_token);
    let ApiReply::Issued {
        token,
        credential,
        secret_account,
        stored_in,
    } = api(&mut desktop, ApiCommand::Reissue(old.id)).unwrap()
    else {
        panic!()
    };
    assert_eq!(stored_in, Some(SecretBackend::Memory));
    assert_eq!(token.replaces, Some(old.id));
    assert_eq!(
        (token.label.as_str(), &token.scopes),
        ("rotating", &old.scopes)
    );
    assert_eq!(
        live.call(Command::Status).unwrap().unwrap_err(),
        WireError::Denied(Denial::Revoked)
    );
    assert_eq!(store.get(&old_account).unwrap(), None);
    assert_eq!(
        store.get(&secret_account).unwrap(),
        Some(credential.clone())
    );
    assert!(query(&mut env.session("api", credential)).is_ok());
}

#[test]
fn read_only_profiles_cannot_widen_themselves() {
    let env = Env::new();
    let (config, _) = with_memory_store(&env);
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    let (info, token, _) = issue_token(
        &mut desktop,
        "assistant",
        ClientKind::Mcp,
        &profiles::mcp_read_only().into_iter().collect::<Vec<_>>(),
    );
    // Claiming desktop and every capability only narrows.
    let mut connector = env.connector_with("desktop", token);
    connector.requested = Some(Capability::ALL.into_iter().collect());
    let mut mcp = Session::open(&connector).unwrap();
    assert!(!mcp.welcome.capabilities.contains(&Capability::LibraryWrite));
    forbidden(
        mcp.call(Command::Api(ApiCommand::SetScopes {
            grant: info.id,
            scopes: profiles::mcp_write(),
        })),
        Operation::GrantUpdate,
    );
    forbidden(
        mcp.call(Command::Api(ApiCommand::IssueToken(IssueToken {
            label: "self".into(),
            kind: ClientKind::Mcp,
            scopes: profiles::mcp_write(),
            expires_at_ms: None,
        }))),
        Operation::GrantIssue,
    );
    forbidden(
        mcp.call(Command::Api(ApiCommand::Elevate {
            grant: info.id,
            kind: BulkKind::Move,
            duration_ms: 60_000,
        })),
        Operation::GrantElevate,
    );
    forbidden(
        mcp.call(Command::Grants(GrantCommand::Issue {
            label: "x".into(),
            capabilities: [Capability::LibraryWrite].into(),
            expires_at_ms: None,
        })),
        Operation::GrantIssue,
    );
    forbidden(
        mcp.call(Command::Api(ApiCommand::SaveSettings(Default::default()))),
        Operation::ApiSettingsWrite,
    );
    forbidden(
        mcp.call(Command::Api(ApiCommand::Audit { limit: 10 })),
        Operation::AuditRead,
    );
    forbidden(
        mcp.call(Command::Export(ExportRequest {
            selection: ExportSelection::All,
            format: Format::Bibtex,
            path: env.temp.path().join("leak.bib"),
        })),
        Operation::Export,
    );
    assert!(create(&mut mcp, "AI write").is_err());
    // An owner session narrowed below the requested scopes cannot widen a token either.
    let mut narrowed = {
        let mut c = env.connector("desktop");
        c.requested = Some(
            [Capability::GrantAdmin, Capability::MetadataRead]
                .into_iter()
                .collect::<CapabilitySet>(),
        );
        Session::open(&c).unwrap()
    };
    assert_eq!(
        api(
            &mut narrowed,
            ApiCommand::SetScopes {
                grant: info.id,
                scopes: profiles::mcp_write(),
            },
        )
        .unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::GrantIssue))
    );
    // Native-host and desktop grants are never issued from scopes.
    assert!(
        invalid(api(
            &mut desktop,
            ApiCommand::IssueToken(IssueToken {
                label: "host".into(),
                kind: ClientKind::NativeHost,
                scopes: scopes(&[Scope::PapersRead]),
                expires_at_ms: None,
            }),
        ))
        .contains("only API, MCP and CLI")
    );
}

#[test]
fn bulk_operations_need_a_short_explicit_elevation() {
    let env = Env::new();
    let (config, _) = with_memory_store(&env);
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    let papers: Vec<_> = (0..13)
        .map(|i| create(&mut desktop, &format!("bulk {i}")).unwrap())
        .collect();
    let (info, token, _) = issue_token(
        &mut desktop,
        "cleanup",
        ClientKind::Api,
        &[Scope::PapersRead, Scope::PapersDelete],
    );
    let mut client = env.session("api", token);
    let trash = |s: &mut Session, i: usize| {
        s.call(Command::Lifecycle {
            paper: papers[i].id,
            deleted: true,
        })
        .unwrap()
    };
    for i in 0..10 {
        assert!(trash(&mut client, i).is_ok(), "paper {i}");
    }
    assert_eq!(
        trash(&mut client, 10).unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::BulkTrash))
    );
    // Refused elevations: too long, nothing to elevate, permanent, issued as a scope.
    assert!(
        invalid(api(
            &mut desktop,
            ApiCommand::Elevate {
                grant: info.id,
                kind: BulkKind::Delete,
                duration_ms: 16 * 60_000
            }
        ))
        .contains("15 minutes")
    );
    let (reader, _, _) = issue_token(
        &mut desktop,
        "reader",
        ClientKind::Api,
        &[Scope::PapersRead],
    );
    assert!(
        invalid(api(
            &mut desktop,
            ApiCommand::Elevate {
                grant: reader.id,
                kind: BulkKind::Delete,
                duration_ms: 60_000
            }
        ))
        .contains("neither")
    );
    assert!(
        invalid(api(
            &mut desktop,
            ApiCommand::IssueToken(IssueToken {
                label: "bulk".into(),
                kind: ClientKind::Api,
                scopes: scopes(&[Scope::PapersDelete, Scope::BulkElevated]),
                expires_at_ms: None,
            }),
        ))
        .contains("time-limited")
    );
    match desktop
        .call(Command::Grants(GrantCommand::Issue {
            label: "permanent bulk".into(),
            capabilities: [Capability::LibraryDelete, Capability::BulkElevated].into(),
            expires_at_ms: None,
        }))
        .unwrap()
    {
        Err(WireError::Service(ServiceError::InvalidInput(r))) => {
            assert!(r.contains("limited time"))
        }
        other => panic!("{other:?}"),
    }
    // An elevation for one kind does not lift the other: this token cannot even be
    // elevated for moves (no papers:write), and a delete elevation is needed here.
    assert!(
        invalid(api(
            &mut desktop,
            ApiCommand::Elevate {
                grant: info.id,
                kind: BulkKind::Move,
                duration_ms: 60_000
            }
        ))
        .contains("neither")
    );
    // Only the owner approves elevations: a session narrowed below ElevationAdmin cannot.
    let mut narrowed = {
        let mut c = env.connector("desktop");
        c.requested = Some(
            cm_core::policy::presets::desktop()
                .into_iter()
                .filter(|c| *c != Capability::ElevationAdmin)
                .collect(),
        );
        Session::open(&c).unwrap()
    };
    assert_eq!(
        api(
            &mut narrowed,
            ApiCommand::Elevate {
                grant: info.id,
                kind: BulkKind::Delete,
                duration_ms: 60_000,
            },
        )
        .unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::GrantElevate))
    );
    // Explicit, short elevation lifts the limit for the live session...
    let ApiReply::Updated(elevated) = api(
        &mut desktop,
        ApiCommand::Elevate {
            grant: info.id,
            kind: BulkKind::Delete,
            duration_ms: 60_000,
        },
    )
    .unwrap() else {
        panic!()
    };
    assert!(elevated.elevated_until_ms.is_some());
    assert_eq!(elevated.elevated_kind, Some(BulkKind::Delete));
    assert!(trash(&mut client, 10).is_ok());
    assert!(trash(&mut client, 11).is_ok());
    // ...and ending it restores the limit immediately.
    api(&mut desktop, ApiCommand::EndElevation(info.id)).unwrap();
    assert_eq!(
        trash(&mut client, 12).unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::BulkTrash))
    );
    // The audit log records who/what/result without content.
    let ApiReply::Audit(entries) = api(&mut desktop, ApiCommand::Audit { limit: 100 }).unwrap()
    else {
        panic!()
    };
    let denied = entries
        .iter()
        .find(|e| e.operation == Operation::BulkTrash && e.outcome == AuditOutcome::Denied)
        .unwrap();
    assert_eq!(denied.grant, Some(info.id));
    assert_eq!(denied.kind, Some(ClientKind::Api));
    assert_eq!(denied.reason.as_deref(), Some("forbidden"));
    assert!(
        entries
            .iter()
            .any(|e| e.operation == Operation::GrantElevate
                && e.target == Some(info.id)
                && e.outcome == AuditOutcome::Succeeded)
    );
}

/// Codex review (PR #69): a token holding both `papers:delete` and `papers:write` can be
/// elevated for either kind, and a move elevation (what the settings UI now sends when the
/// owner chooses "Bulk move") lifts the move limit only.
#[test]
fn a_delete_and_write_token_can_be_elevated_for_bulk_moves() {
    let env = Env::new();
    let (config, _) = with_memory_store(&env);
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    let papers: Vec<_> = (0..12)
        .map(|i| create(&mut desktop, &format!("move {i}")).unwrap())
        .collect();
    let collection = match desktop
        .call(Command::Organization(
            OrganizationRequest::CreateCollection(CollectionInput::named("Bulk")),
        ))
        .unwrap()
        .unwrap()
    {
        Reply::Organization(o) => o.collections[0].id,
        other => panic!("{other:?}"),
    };
    let (info, token, _) = issue_token(
        &mut desktop,
        "organizer",
        ClientKind::Api,
        &[Scope::PapersRead, Scope::PapersWrite, Scope::PapersDelete],
    );
    assert_eq!(
        elevatable_kinds(&info.capabilities),
        vec![BulkKind::Delete, BulkKind::Move]
    );
    let mut client = env.session("api", token);
    let mut member = false;
    let mut toggle = |s: &mut Session| {
        member = !member;
        s.call(Command::Organization(
            OrganizationRequest::CollectionMember(papers[0].id, collection, member),
        ))
        .unwrap()
        .map(|_| ())
    };
    for i in 0..cm_core::policy::elevation::MOVE_BURST {
        assert!(toggle(&mut client).is_ok(), "move {i}");
    }
    assert_eq!(
        toggle(&mut client).unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::BulkMove))
    );
    let ApiReply::Updated(elevated) = api(
        &mut desktop,
        ApiCommand::Elevate {
            grant: info.id,
            kind: BulkKind::Move,
            duration_ms: 60_000,
        },
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(elevated.elevated_kind, Some(BulkKind::Move));
    for i in 0..5 {
        assert!(toggle(&mut client).is_ok(), "elevated move {i}");
    }
    // The move elevation does not lift the delete limit.
    for paper in &papers[1..=cm_core::policy::elevation::DESTRUCTIVE_BURST] {
        client
            .call(Command::Lifecycle {
                paper: paper.id,
                deleted: true,
            })
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        client
            .call(Command::Lifecycle {
                paper: papers[11].id,
                deleted: true,
            })
            .unwrap()
            .unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::BulkTrash))
    );
}

#[test]
fn export_tokens_write_only_inside_allowed_folders() {
    let env = Env::new();
    let (config, _) = with_memory_store(&env);
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    create(&mut desktop, "Exported").unwrap();
    let (_, token, _) = issue_token(
        &mut desktop,
        "exporter",
        ClientKind::Api,
        &[Scope::PapersRead, Scope::ExportsWrite],
    );
    let mut client = env.session("api", token);
    let allowed = env.temp.path().canonicalize().unwrap().join("exports");
    std::fs::create_dir(&allowed).unwrap();
    let export = |s: &mut Session, path: std::path::PathBuf| {
        s.call(Command::Export(ExportRequest {
            selection: ExportSelection::All,
            format: Format::Bibtex,
            path,
        }))
        .unwrap()
    };
    assert_eq!(
        export(&mut client, allowed.join("a.bib")).unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::Export))
    );
    // Settings validation: a folder must exist and be reached without symlinks.
    let link = env.temp.path().canonicalize().unwrap().join("link");
    std::os::unix::fs::symlink(&allowed, &link).unwrap();
    let mut settings = cm_protocol::api::ApiSettings {
        export_dirs: vec![link],
        ..Default::default()
    };
    assert!(
        invalid(api(
            &mut desktop,
            ApiCommand::SaveSettings(settings.clone())
        ))
        .contains("symlinks")
    );
    settings.export_dirs = vec![allowed.clone()];
    api(&mut desktop, ApiCommand::SaveSettings(settings)).unwrap();
    assert!(export(&mut client, allowed.join("a.bib")).is_ok());
    assert!(allowed.join("a.bib").is_file());
    assert_eq!(
        export(&mut client, env.temp.path().join("outside.bib")).unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::Export))
    );
    assert!(
        export(&mut client, allowed.join("../escape.bib")).is_err(),
        "dot components are refused"
    );
    // Only the format's extension.
    assert_eq!(
        export(&mut client, allowed.join("a.sh")).unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::Export))
    );
    // Home, the library, its private state and its ancestors are never export folders.
    let canonical_library = env.library.canonicalize().unwrap();
    for refused in [
        canonical_library.clone(),
        canonical_library.join("daemon"),
        canonical_library.parent().unwrap().to_path_buf(),
    ] {
        assert!(
            invalid(api(
                &mut desktop,
                ApiCommand::SaveSettings(cm_protocol::api::ApiSettings {
                    export_dirs: vec![refused.clone()],
                    ..Default::default()
                }),
            ))
            .contains("home folder, the library"),
            "{}",
            refused.display()
        );
    }
    // API settings are owner-only: a session narrowed below ApiAdmin cannot save them.
    let mut narrowed = {
        let mut c = env.connector("desktop");
        c.requested = Some(
            cm_core::policy::presets::desktop()
                .into_iter()
                .filter(|c| *c != Capability::ApiAdmin)
                .collect(),
        );
        Session::open(&c).unwrap()
    };
    assert_eq!(
        api(&mut narrowed, ApiCommand::SaveSettings(Default::default())).unwrap_err(),
        WireError::Denied(Denial::Forbidden(Operation::ApiSettingsWrite))
    );
    assert!(!env.temp.path().join("escape.bib").exists());
}

#[test]
fn api_settings_and_tokens_persist_across_restart() {
    let env = Env::new();
    let (config, _) = with_memory_store(&env);
    let daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    let ApiReply::Overview(overview) = api(&mut desktop, ApiCommand::Overview).unwrap() else {
        panic!()
    };
    assert!(!overview.settings.enabled, "API is off by default");
    assert_eq!(overview.settings.bind, BindAddress::LoopbackV4);
    assert_eq!(overview.secret_backend, SecretBackend::Memory);
    assert!(
        invalid(api(
            &mut desktop,
            ApiCommand::SaveSettings(cm_protocol::api::ApiSettings {
                port: 80,
                ..Default::default()
            }),
        ))
        .contains("1024")
    );
    let wanted = cm_protocol::api::ApiSettings {
        enabled: true,
        bind: BindAddress::LoopbackV6,
        port: 40123,
        export_dirs: vec![],
    };
    api(&mut desktop, ApiCommand::SaveSettings(wanted.clone())).unwrap();
    let (info, token, _) = issue_token(&mut desktop, "kept", ClientKind::Mcp, &[Scope::PapersRead]);
    assert!(query(&mut env.session("mcp", token.clone())).is_ok());
    drop(desktop);
    daemon.shutdown();

    // Simulate a pre-#26 owner grant: the bootstrap upgrade grants new capabilities in place.
    {
        let directory = cm_storage::LibraryDirectory::prepare(&env.library).unwrap();
        let db = cm_storage::LibraryDb::open(&directory).unwrap();
        let owner = db
            .daemon_grants()
            .all()
            .unwrap()
            .into_iter()
            .find(|g| g.grant.label == "desktop-bootstrap")
            .unwrap();
        let old: CapabilitySet = Capability::ALL[..12].iter().copied().collect();
        assert!(
            db.daemon_grants()
                .set_access(owner.grant.id, &old, None, None)
                .unwrap()
        );
    }

    let (config, _) = with_memory_store(&env);
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    assert_eq!(
        desktop.welcome.capabilities,
        cm_core::policy::presets::desktop()
    );
    let ApiReply::Overview(after) = api(&mut desktop, ApiCommand::Overview).unwrap() else {
        panic!()
    };
    assert_eq!(after.settings, wanted);
    let ApiReply::Tokens(tokens) = api(&mut desktop, ApiCommand::ListTokens).unwrap() else {
        panic!()
    };
    let kept = tokens.iter().find(|t| t.id == info.id).unwrap();
    assert!(kept.last_used_at_ms.is_some(), "last use persisted at exit");
    assert!(query(&mut env.session("mcp", token)).is_ok());
}

#[test]
fn cli_bootstrap_prints_a_token_once_and_only_where_allowed() {
    let env = Env::new();
    let (config, store) = with_memory_store(&env);
    let _daemon = start(config).unwrap();
    let run = |extra: &[&str]| {
        let mut command = Process::new(env!("CARGO_BIN_EXE_papersd"));
        command
            .args(["token", "issue", "--library"])
            .arg(&env.library)
            .arg("--runtime-dir")
            .arg(&env.runtime)
            .args([
                "--label",
                "terminal",
                "--kind",
                "cli",
                "--scope",
                "papers:read",
            ])
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.output().unwrap()
    };
    // stdout is a pipe here: refused before anything is issued.
    let refused = run(&[]);
    assert_eq!(refused.status.code(), Some(cm_daemon::admin::EXIT_REFUSED));
    assert!(refused.stdout.is_empty());
    assert!(store.is_empty());
    let printed = run(&["--allow-non-tty"]);
    assert_eq!(printed.status.code(), Some(0), "{printed:?}");
    let text = String::from_utf8(printed.stdout).unwrap();
    let secret = text.trim();
    assert_eq!(text.lines().count(), 1);
    assert_eq!(secret.len(), 64);
    let stderr = String::from_utf8(printed.stderr).unwrap();
    assert!(!stderr.contains(secret), "secret only on stdout");
    assert_eq!(store.len(), 1);
    // A token that cannot be delivered (stdout write fails) is revoked, exit 1.
    // /dev/full (writes fail with ENOSPC) exists only on Linux.
    #[cfg(target_os = "linux")]
    {
        let mut command = Process::new(env!("CARGO_BIN_EXE_papersd"));
        command
            .args(["token", "issue", "--library"])
            .arg(&env.library)
            .arg("--runtime-dir")
            .arg(&env.runtime)
            .args([
                "--label",
                "undelivered",
                "--kind",
                "cli",
                "--scope",
                "papers:read",
            ])
            .arg("--allow-non-tty")
            .stdin(Stdio::null())
            .stdout(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/full")
                    .unwrap(),
            )
            .stderr(Stdio::piped());
        let full = command.output().unwrap();
        assert_eq!(
            full.status.code(),
            Some(cm_daemon::admin::EXIT_ERROR),
            "{full:?}"
        );
        assert!(String::from_utf8_lossy(&full.stderr).contains("it was revoked"));
        assert_eq!(store.len(), 1, "the undelivered token's copy was removed");
    }
    let mut cli = env.session("cli", Credential(secret.into()));
    assert!(query(&mut cli).is_ok());
    assert!(create(&mut cli, "read only").is_err());
    // list and settings through the same bootstrap.
    let listed = Process::new(env!("CARGO_BIN_EXE_papersd"))
        .args(["token", "list", "--library"])
        .arg(&env.library)
        .arg("--runtime-dir")
        .arg(&env.runtime)
        .output()
        .unwrap();
    let listing = String::from_utf8(listed.stdout).unwrap();
    assert!(listing.contains("terminal") && listing.contains("papers:read"));
    #[cfg(target_os = "linux")]
    assert!(
        listing
            .lines()
            .any(|l| l.contains("undelivered") && l.contains("revoked"))
    );
    assert!(!listing.contains(secret));
    let set = Process::new(env!("CARGO_BIN_EXE_papersd"))
        .args(["api", "set", "--library"])
        .arg(&env.library)
        .arg("--runtime-dir")
        .arg(&env.runtime)
        .args(["--bind", "0.0.0.0"])
        .output()
        .unwrap();
    assert_eq!(set.status.code(), Some(cm_daemon::admin::EXIT_USAGE));
    let set = Process::new(env!("CARGO_BIN_EXE_papersd"))
        .args(["api", "set", "--library"])
        .arg(&env.library)
        .arg("--runtime-dir")
        .arg(&env.runtime)
        .args(["--enable", "--port", "40555"])
        .output()
        .unwrap();
    assert_eq!(set.status.code(), Some(0));
    assert!(
        String::from_utf8(set.stdout)
            .unwrap()
            .contains("enabled=true bind=127.0.0.1 port=40555")
    );
}

#[test]
fn delegated_grants_never_get_owner_powers_or_unlimited_lifetimes() {
    let env = Env::new();
    let (config, _) = with_memory_store(&env);
    let _daemon = start(config).unwrap();
    let mut desktop = env.desktop();
    let issue_raw = |s: &mut Session, caps: CapabilitySet, expires_at_ms: Option<i64>| {
        s.call(Command::Grants(GrantCommand::Issue {
            label: "delegate".into(),
            capabilities: caps,
            expires_at_ms,
        }))
        .unwrap()
    };
    for owner_only in [
        Capability::GrantAdmin,
        Capability::ApiAdmin,
        Capability::ElevationAdmin,
    ] {
        match issue_raw(
            &mut desktop,
            [Capability::LibraryRead, owner_only].into(),
            None,
        ) {
            Err(WireError::Service(ServiceError::InvalidInput(r))) => {
                assert!(r.contains("cannot be delegated"), "{owner_only:?}: {r}")
            }
            other => panic!("{owner_only:?}: {other:?}"),
        }
    }
    let now = cm_daemon::ipc::auth::now_ms();
    let day = 86_400_000;
    // No expiry requested: the policy default applies (write capability: 30 days).
    match issue_raw(&mut desktop, [Capability::LibraryWrite].into(), None).unwrap() {
        Reply::Grants(cm_protocol::ipc::GrantReply::Issued { grant, .. }) => {
            let at = grant.expires_at_ms.expect("delegates always expire");
            assert!(at > now + 29 * day && at <= now + 31 * day, "{at}");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        issue_raw(
            &mut desktop,
            [Capability::LibraryWrite].into(),
            Some(now + 91 * day)
        ),
        Err(WireError::Service(ServiceError::InvalidInput(r))) if r.contains("maximum lifetime")
    ));
    assert!(
        issue_raw(
            &mut desktop,
            [Capability::LibraryRead].into(),
            Some(now + 300 * day)
        )
        .is_ok()
    );
}
