//! #25 security ADR tests: default deny, spoofed kind/scope, native host limits,
//! MCP read-only with separately approved write, revocation/expiry, preview grant binding.
mod support;
use cm_daemon::ipc::start;
use cm_protocol::{
    client::{ClientError, Session, handshake},
    exchange::{ExportRequest, Format, ImportReply, ImportRequestUi},
    import::{ImportCommit, ImportDestination, ImportRequest, PaperDraft},
    ipc::{
        Capability, CapabilitySet, Command, Denial, GrantCommand, GrantReply, HandshakeRejection,
        Operation, PdfCommand, Reply, WireError,
    },
    jobs::{JobCommand, JobReply, JobSubmit, kinds},
    library::{LibraryRequest, NewPaper, PaperList},
    library_query::LibraryQuery,
    organization::OrganizationRequest,
};
use support::*;

fn set(caps: &[Capability]) -> CapabilitySet {
    caps.iter().copied().collect()
}
fn denied(result: Result<Result<Reply, WireError>, ClientError>, op: Operation) {
    assert_eq!(
        result.unwrap().unwrap_err(),
        WireError::Denied(Denial::Forbidden(op))
    );
}
/// #20 annotated PDF export writes a caller-named file: LibraryRead + LocalFileWrite;
/// replacing an existing one additionally needs the desktop-only LocalFileReplace.
fn pdf_export(path: std::path::PathBuf, replace_existing: bool) -> Command {
    Command::Pdf(PdfCommand::Export {
        attachment: cm_protocol::domain::AttachmentId(uuid::Uuid::now_v7()),
        destination: path,
        replace_existing,
        allow_omissions: true,
    })
}
fn create(session: &mut Session, title: &str) -> cm_protocol::domain::Paper {
    let mut paper = NewPaper::titled(title);
    paper.bibliography.doi = Some(format!("10.1000/{}", title.len()));
    match ok(session.call(Command::Library(LibraryRequest::Create(Box::new(paper))))) {
        Reply::Library(cm_protocol::library::LibraryReply::Paper(p)) => *p,
        other => panic!("{other:?}"),
    }
}
fn preview(session: &mut Session, draft: PaperDraft) -> Result<Reply, WireError> {
    session
        .call(Command::Import(ImportRequestUi::Preview(ImportRequest {
            candidates: vec![draft],
            destination: ImportDestination::default(),
        })))
        .unwrap()
}

#[test]
fn native_host_grant_is_minimal_even_when_claiming_desktop_and_all_scopes() {
    let env = Env::new();
    let _daemon = start(env.config()).unwrap();
    let mut desktop = env.desktop();
    let existing = create(&mut desktop, "Existing paper");
    let host = issue(
        &mut desktop,
        "browser host",
        cm_core::policy::presets::native_host(),
    );
    // Spoof: kind "desktop" and every scope requested. Requested scopes only narrow.
    let mut connector = env.connector_with("desktop", host);
    connector.requested = Some(Capability::ALL.into_iter().collect());
    let mut spoof = Session::open(&connector).unwrap();
    assert_eq!(
        spoof.welcome.capabilities,
        cm_core::policy::presets::native_host()
    );
    denied(
        spoof.call(Command::Lifecycle {
            paper: existing.id,
            deleted: true,
        }),
        Operation::PaperTrash,
    );
    denied(
        spoof.call(Command::Library(LibraryRequest::Get(existing.id))),
        Operation::PaperGet,
    );
    denied(
        spoof.call(Command::Query(LibraryQuery::default())),
        Operation::LibraryQuery,
    );
    denied(
        spoof.call(Command::Export(ExportRequest {
            selection: cm_protocol::exchange::ExportSelection::All,
            format: Format::Bibtex,
            path: env.temp.path().join("leak.bib"),
        })),
        Operation::Export,
    );
    denied(
        spoof.call(Command::Import(ImportRequestUi::Parse {
            path: "/etc/passwd".into(),
            format: Format::Ris,
        })),
        Operation::ImportParseFile,
    );
    denied(
        spoof.call(Command::Organization(OrganizationRequest::DeleteTag(
            cm_protocol::domain::TagId(uuid::Uuid::now_v7()),
        ))),
        Operation::OrganizationDelete,
    );
    denied(
        spoof.call(Command::Grants(GrantCommand::List)),
        Operation::GrantList,
    );
    denied(
        spoof.call(pdf_export(env.temp.path().join("leak.pdf"), false)),
        Operation::PdfExport,
    );
    assert!(!env.temp.path().join("leak.bib").exists());
    assert!(!env.temp.path().join("leak.pdf").exists());
    // Allowed: destination names/IDs and a non-persisting capture preview whose duplicate
    // matches (existing paper contents) are hidden.
    assert!(matches!(
        ok(spoof.call(Command::Organization(OrganizationRequest::List))),
        Reply::Organization(_)
    ));
    let mut draft = PaperDraft::titled("Existing paper");
    draft.bibliography.doi = existing.bibliography.doi.clone();
    let Reply::Import(ImportReply::Preview(p)) = preview(&mut spoof, draft).unwrap() else {
        panic!("expected preview");
    };
    assert!(p.candidates[0].matches.is_empty());
    assert!(p.candidates[0].existing_match.is_none());
    assert!(p.warnings.iter().any(|w| w.contains("hidden")));
    // ...and it cannot commit it.
    denied(
        spoof.call(Command::Import(ImportRequestUi::Commit(ImportCommit {
            preview_id: p.id,
            decisions: vec![],
        }))),
        Operation::ImportCommit,
    );
}

#[test]
fn mcp_is_read_only_until_a_separate_write_grant_is_approved() {
    let env = Env::new();
    let _daemon = start(env.config()).unwrap();
    let mut desktop = env.desktop();
    create(&mut desktop, "Readable");
    let read_only = issue(
        &mut desktop,
        "mcp",
        cm_core::policy::presets::mcp_read_only(),
    );
    let mut mcp = env.session("mcp", read_only);
    assert!(matches!(
        ok(mcp.call(Command::Query(LibraryQuery::default()))),
        Reply::Page(p) if p.papers.len() == 1
    ));
    // Reading the library is not enough to write an annotated PDF to a local path.
    denied(
        mcp.call(pdf_export(env.temp.path().join("mcp.pdf"), false)),
        Operation::PdfExport,
    );
    denied(
        mcp.call(Command::Library(LibraryRequest::Create(Box::new(
            NewPaper::titled("AI write"),
        )))),
        Operation::PaperCreate,
    );
    // No self-elevation: a non-admin grant cannot mint (even narrower) grants.
    denied(
        mcp.call(Command::Grants(GrantCommand::Issue {
            label: "elevate".into(),
            capabilities: set(&[Capability::LibraryWrite]),
            expires_at_ms: None,
        })),
        Operation::GrantIssue,
    );
    denied(
        mcp.call(Command::Jobs(JobCommand::Submit(JobSubmit {
            kind: kinds::DIAGNOSTIC_WAIT.into(),
            idempotency_key: "x".into(),
            payload: serde_json::json!({"millis": 0}),
            max_attempts: None,
            deadline_at_ms: None,
        }))),
        Operation::JobSubmit,
    );
    // Write needs a separately approved grant (issued by the GrantAdmin GUI path).
    let write = issue(
        &mut desktop,
        "mcp write (approved)",
        set(&[Capability::LibraryRead, Capability::LibraryWrite]),
    );
    let mut mcp_write = env.session("mcp", write);
    create(&mut mcp_write, "Approved AI write");
    // A grant can never exceed its issuer, and an empty grant is rejected.
    let mut narrowed_admin = {
        let mut c = env.connector("desktop");
        c.requested = Some(set(&[Capability::GrantAdmin, Capability::LibraryRead]));
        Session::open(&c).unwrap()
    };
    denied(
        narrowed_admin.call(Command::Grants(GrantCommand::Issue {
            label: "x".into(),
            capabilities: set(&[Capability::LibraryWrite]),
            expires_at_ms: None,
        })),
        Operation::GrantIssue,
    );
}

#[test]
fn local_file_write_grant_cannot_replace_existing_files_with_pdf_export() {
    use cm_protocol::attachments::{PdfExportFailure, PdfReply};
    let env = Env::new();
    let _daemon = start(env.config()).unwrap();
    let mut desktop = env.desktop();
    // An approved MCP write grant that may also create local files (e.g. exports).
    let write = issue(
        &mut desktop,
        "mcp write + files (approved)",
        set(&[
            Capability::LibraryRead,
            Capability::LibraryWrite,
            Capability::LocalFileRead,
            Capability::LocalFileWrite,
        ]),
    );
    let mut mcp = env.session("mcp", write);
    // Canonical: on macOS the temp dir is under /var -> /private/var, and destination folders
    // reached through a symlink are (rightly) refused before the checks under test.
    let dir = env.temp.path().canonicalize().unwrap();
    let victims = [
        (dir.join("victim.pdf"), b"%PDF user's own".to_vec()),
        (dir.join(".profile"), b"export PATH=...".to_vec()),
    ];
    for (path, bytes) in &victims {
        std::fs::write(path, bytes).unwrap();
        // Replacing is a separate, desktop-only authority: refused before any work.
        denied(
            mcp.call(pdf_export(path.clone(), true)),
            Operation::PdfExportReplace,
        );
        // Without replace, an existing file is refused by the daemon, never overwritten.
        match ok(mcp.call(pdf_export(path.clone(), false))) {
            Reply::Pdf(PdfReply::Exported(Err(PdfExportFailure::Destination(reason)))) => {
                assert!(reason.contains("already exists"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(&std::fs::read(path).unwrap(), bytes);
    }
    // The desktop grant holds the replace authority, but only for .pdf files.
    match ok(desktop.call(pdf_export(victims[1].0.clone(), true))) {
        Reply::Pdf(PdfReply::Exported(Err(PdfExportFailure::Destination(reason)))) => {
            assert!(reason.contains(".pdf"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
    match ok(desktop.call(pdf_export(victims[0].0.clone(), true))) {
        // Authorized and destination accepted; the (unknown) attachment then fails.
        Reply::Pdf(PdfReply::Exported(Err(PdfExportFailure::MissingOrCorrupt))) => {}
        other => panic!("{other:?}"),
    }
    for (path, bytes) in &victims {
        assert_eq!(&std::fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn revoked_and_expired_grants_are_refused_for_live_and_new_sessions() {
    let env = Env::new();
    let _daemon = start(env.config()).unwrap();
    let mut desktop = env.desktop();
    let credential = issue(
        &mut desktop,
        "cli",
        set(&[Capability::LibraryRead, Capability::JobsRead]),
    );
    let mut cli = env.session("cli", credential.clone());
    let grant = cli.welcome.grant;
    assert!(
        ok(cli.call(Command::Library(LibraryRequest::List(PaperList::default())))).is_library()
    );
    assert!(matches!(
        ok(desktop.call(Command::Grants(GrantCommand::Revoke(grant)))),
        Reply::Grants(GrantReply::Revoked(g)) if g == grant
    ));
    assert_eq!(
        cli.call(Command::Status).unwrap().unwrap_err(),
        WireError::Denied(Denial::Revoked)
    );
    let mut stream = std::os::unix::net::UnixStream::connect(env.paths().socket).unwrap();
    assert!(matches!(
        handshake(
            &mut stream,
            env.connector_with("cli", credential).hello().unwrap()
        ),
        Err(ClientError::Rejected(HandshakeRejection::Unauthenticated))
    ));
    match ok(desktop.call(Command::Grants(GrantCommand::List))) {
        Reply::Grants(GrantReply::Grants(list)) => {
            assert!(list.iter().any(|g| g.id == grant && g.revoked));
        }
        other => panic!("{other:?}"),
    }
    // The bootstrap label is reserved: an issued grant can never pose as the owner's.
    for label in ["desktop-bootstrap", " Desktop-Bootstrap "] {
        assert!(matches!(
            desktop
                .call(Command::Grants(GrantCommand::Issue {
                    label: label.into(),
                    capabilities: set(&[Capability::LibraryRead]),
                    expires_at_ms: None,
                }))
                .unwrap(),
            Err(WireError::Service(cm_protocol::library::ServiceError::InvalidInput(r)))
                if r.contains("reserved")
        ));
    }
    // Expiry applies to an established session too.
    let soon = cm_daemon::ipc::auth::now_ms() + 400;
    let expiring = match ok(desktop.call(Command::Grants(GrantCommand::Issue {
        label: "short".into(),
        capabilities: set(&[Capability::LibraryRead]),
        expires_at_ms: Some(soon),
    }))) {
        Reply::Grants(GrantReply::Issued { credential, .. }) => credential,
        other => panic!("{other:?}"),
    };
    let mut short = env.session("cli", expiring);
    assert!(short.call(Command::Status).unwrap().is_ok());
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(
        short.call(Command::Status).unwrap().unwrap_err(),
        WireError::Denied(Denial::Expired)
    );
}

#[test]
fn previews_are_bound_to_their_grant_and_jobs_to_their_owner() {
    let env = Env::new();
    let _daemon = start(env.config()).unwrap();
    let mut desktop = env.desktop();
    // Issue first: any committed library write (a grant too) invalidates open previews (#33).
    let writer = issue(
        &mut desktop,
        "other writer",
        set(&[
            Capability::LibraryRead,
            Capability::LibraryWrite,
            Capability::JobsRead,
            Capability::JobsControl,
        ]),
    );
    let mut other = env.session("cli", writer);
    let Reply::Import(ImportReply::Preview(mine)) =
        preview(&mut desktop, PaperDraft::titled("Desktop preview")).unwrap()
    else {
        panic!("expected preview");
    };
    denied(
        other.call(Command::Import(ImportRequestUi::Commit(ImportCommit {
            preview_id: mine.id.clone(),
            decisions: vec![],
        }))),
        Operation::ImportCommit,
    );
    denied(
        other.call(Command::Import(ImportRequestUi::Cancel(mine.id.clone()))),
        Operation::ImportCancel,
    );
    // The owner can still commit its own preview.
    let decisions = vec![cm_protocol::import::ImportDecision {
        candidate_index: 0,
        action: cm_protocol::import::ImportAction::Create,
    }];
    assert!(matches!(
        ok(desktop.call(Command::Import(ImportRequestUi::Commit(ImportCommit {
            preview_id: mine.id,
            decisions,
        })))),
        Reply::Import(ImportReply::Committed(o)) if o.created.len() == 1
    ));
    // Jobs: the submitting grant sees its own job; another non-admin grant does not.
    let Reply::Jobs(JobReply::Job(job)) =
        ok(desktop.call(Command::Jobs(JobCommand::Submit(JobSubmit {
            kind: kinds::DIAGNOSTIC_WAIT.into(),
            idempotency_key: "owned".into(),
            payload: serde_json::json!({"millis": 0}),
            max_attempts: None,
            deadline_at_ms: None,
        }))))
    else {
        panic!("expected job");
    };
    assert_eq!(
        other
            .call(Command::Jobs(JobCommand::Get(job.id)))
            .unwrap()
            .unwrap_err(),
        WireError::Service(cm_protocol::library::ServiceError::NotFound)
    );
    assert_eq!(
        other
            .call(Command::Jobs(JobCommand::Cancel(job.id)))
            .unwrap()
            .unwrap_err(),
        WireError::Service(cm_protocol::library::ServiceError::NotFound)
    );
    // attachment.import needs LocalFileRead in addition to JobsControl.
    denied(
        other.call(Command::Jobs(JobCommand::Submit(JobSubmit {
            kind: kinds::ATTACHMENT_IMPORT.into(),
            idempotency_key: "file".into(),
            payload: serde_json::json!({
                "paper": uuid::Uuid::now_v7(), "path": "/etc/hosts",
                "original_name": null, "media_type": null
            }),
            max_attempts: None,
            deadline_at_ms: None,
        }))),
        Operation::JobSubmit,
    );
}

trait IsLibrary {
    fn is_library(&self) -> bool;
}
impl IsLibrary for Reply {
    fn is_library(&self) -> bool {
        matches!(self, Reply::Library(_))
    }
}
