//! #29 end to end: the real `papers-mcp` executable driven over stdio JSON-RPC, against a
//! real `papersd` started on demand for an isolated temporary library (no GUI, no REST).
//!
//! papersd must exist next to papers-mcp: `cargo test --workspace` builds it (the daemon's
//! own integration tests need it). For `cargo test -p cm-mcp` alone, build it first with
//! `cargo build -p cm-daemon --bin papersd`, or point `CM_TEST_PAPERSD` at one.
//!
//! The papersd under test has no PDF runtime, so PDFs are stored but their page text is never
//! extracted: full-text reads are checked for their pending state, source block and bounds;
//! page-text content is covered by the daemon's #18 tests and the manual PDFium run.
use cm_protocol::{
    annotations::{Anchor, AnnotationKind, AnnotationRequest, NewAnnotation, Source},
    attachments::{PdfReply, Rect},
    client::{
        ClientError, Session,
        local::{CredentialSource, DaemonPaths, Launch, LocalConnector},
    },
    domain::{AttachmentId, Contributor, Paper, PaperId},
    exchange::{ExportRequest, ExportSelection, Format, ImportRequestUi},
    import::{ImportDestination, ImportRequest, PaperDraft},
    ipc::{
        Command, Credential, Denial, HandshakeRejection, Operation, PdfCommand, Reply, WireError,
    },
    jobs::{JobCommand, JobReply, JobState, JobSubmit, kinds},
    library::{LibraryReply, LibraryRequest, NewPaper},
    library_query::LibraryQuery,
    organization::{CollectionInput, OrganizationRequest, TagInput},
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command as Process, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(60);

fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}
fn mcp_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_papers-mcp"))
}
fn papersd_bin() -> PathBuf {
    let path = std::env::var_os("CM_TEST_PAPERSD")
        .map(PathBuf::from)
        .unwrap_or_else(|| mcp_bin().with_file_name("papersd"));
    assert!(
        path.is_file(),
        "papersd not found at {}: run `cargo test --workspace`, or build it first with \
         `cargo build -p cm-daemon --bin papersd` (or set CM_TEST_PAPERSD)",
        path.display()
    );
    path
}

fn pid(raw: u32) -> rustix::process::Pid {
    rustix::process::Pid::from_raw(raw as i32).expect("non-zero pid")
}
fn signal(raw: u32, sig: rustix::process::Signal) {
    rustix::process::kill_process(pid(raw), sig).expect("signal papersd");
}
fn alive(raw: u32) -> bool {
    rustix::process::test_kill_process(pid(raw)).is_ok()
}
fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    condition()
}

/// Isolated library + private runtime directory (short /tmp path for the socket).
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    library: PathBuf,
    runtime: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("cm-mcp-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = temp.path().canonicalize().unwrap();
        Self {
            library: root.join("lib"),
            runtime: root.join("run"),
            root,
            _temp: temp,
        }
    }
    fn paths(&self) -> DaemonPaths {
        DaemonPaths::new(&self.library, Some(&self.runtime), euid()).unwrap()
    }
    fn common_args(&self) -> Vec<String> {
        vec![
            "--library".into(),
            self.library.display().to_string(),
            "--runtime-dir".into(),
            self.runtime.display().to_string(),
            "--papersd".into(),
            papersd_bin().display().to_string(),
            "--daemon-idle-timeout-ms".into(),
            "5000".into(),
        ]
    }
    fn cli(&self, command: &str, extra: &[&str]) -> Value {
        let output = Process::new(mcp_bin())
            .arg(command)
            .args(self.common_args())
            .args(extra)
            .env_remove("PAPERS_MCP_CREDENTIAL_FILE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "papers-mcp {command} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    /// `papers-mcp grant [flags]`: issues an MCP grant into the default credential file.
    fn grant(&self, flags: &[&str]) -> String {
        let out = self.cli("grant", flags);
        out["grant_id"].as_str().unwrap().to_owned()
    }
    fn credential(&self) -> Credential {
        let text = std::fs::read_to_string(self.paths().state_dir.join("mcp.credential")).unwrap();
        Credential(text.trim().to_owned())
    }
    fn connector(&self, kind: &str, credential: CredentialSource) -> LocalConnector {
        let mut c = LocalConnector::new(self.paths(), euid(), kind);
        c.credential = credential;
        c.launch = Some(Launch {
            papersd: papersd_bin(),
            extra_args: vec!["--idle-timeout-ms".into(), "5000".into()],
            timeout: Duration::from_secs(20),
        });
        c
    }
    /// The library owner (desktop bootstrap credential, GrantAdmin) for seeding and checks.
    fn owner(&self) -> Session {
        Session::open(&self.connector("desktop", CredentialSource::DesktopBootstrap)).unwrap()
    }
    /// A direct IPC session presenting the MCP credential, like papers-mcp does.
    fn direct(&self, credential: Credential) -> Result<Session, ClientError> {
        Session::open(&self.connector("mcp", CredentialSource::Explicit(credential)))
    }
    fn daemon_pid(&self) -> u32 {
        match self.owner().call(Command::Status).unwrap().unwrap() {
            Reply::Status(status) => status.pid,
            other => panic!("{other:?}"),
        }
    }
    fn kill_daemon(&self) -> u32 {
        let pid = self.daemon_pid();
        signal(pid, rustix::process::Signal::KILL);
        assert!(wait_until(Duration::from_secs(10), || !alive(pid)));
        pid
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Never leave a daemon behind for a deleted temp library.
        let mut c = LocalConnector::new(self.paths(), euid(), "desktop");
        c.credential = CredentialSource::DesktopBootstrap;
        if let Ok(mut s) = Session::open(&c)
            && let Ok(Ok(Reply::Status(status))) = s.call(Command::Status)
        {
            let _ = rustix::process::kill_process(pid(status.pid), rustix::process::Signal::KILL);
        }
    }
}

fn ok(result: Result<Result<Reply, WireError>, ClientError>) -> Reply {
    result.unwrap().unwrap()
}

struct Seeded {
    paper: Paper,
    attachment: AttachmentId,
}

fn create_paper(owner: &mut Session, paper: NewPaper) -> Paper {
    match ok(owner.call(Command::Library(LibraryRequest::Create(Box::new(paper))))) {
        Reply::Library(LibraryReply::Paper(p)) => *p,
        other => panic!("{other:?}"),
    }
}

fn import_pdf(fx: &Fixture, owner: &mut Session, paper: PaperId, name: &str) -> AttachmentId {
    let path = fx.root.join(name);
    std::fs::write(&path, format!("%PDF-1.4 fixture {name}")).unwrap();
    let job = match ok(owner.call(Command::Jobs(JobCommand::Submit(JobSubmit {
        kind: kinds::ATTACHMENT_IMPORT.into(),
        idempotency_key: name.into(),
        payload: json!({"paper": paper.0, "path": path, "original_name": name,
                        "media_type": "application/pdf"}),
        max_attempts: None,
        deadline_at_ms: None,
    })))) {
        Reply::Jobs(JobReply::Job(job)) => job.id,
        other => panic!("{other:?}"),
    };
    assert!(wait_until(TIMEOUT, || {
        matches!(ok(owner.call(Command::Jobs(JobCommand::Get(job)))),
            Reply::Jobs(JobReply::Job(j)) if j.state == JobState::Succeeded)
    }));
    match ok(owner.call(Command::Pdf(PdfCommand::List(paper)))) {
        Reply::Pdf(PdfReply::Attachments(list)) => list[0].0.id,
        other => panic!("{other:?}"),
    }
}

fn human_highlight(owner: &mut Session, attachment: AttachmentId, quote: &str, comment: &str) {
    let rect = Rect {
        left: 72.0,
        bottom: 700.0,
        right: 300.0,
        top: 712.0,
    };
    ok(
        owner.call(Command::Annotations(AnnotationRequest::Create(Box::new(
            NewAnnotation {
                attachment_id: attachment,
                kind: AnnotationKind::Highlight,
                source: Source::Human,
                anchor: Anchor::Resolved {
                    page_index: 0,
                    page_label: Some("1".into()),
                    quads: vec![rect.quad()],
                },
                quote: quote.into(),
                context_before: None,
                context_after: None,
                color: Some("yellow".into()),
                comment: Some(comment.into()),
            },
        )))),
    );
}

/// One paper with DOI/authors/year, a stored PDF, a human highlight, collection and tag.
fn seed(fx: &Fixture) -> Seeded {
    let mut owner = fx.owner();
    let mut new = NewPaper::titled("Interfacial stability of solid electrolytes");
    new.bibliography.doi = Some("10.5555/mcp.a".into());
    new.bibliography.year = Some(2024);
    new.bibliography.journal = Some("Journal of Examples".into());
    new.bibliography.authors = vec![Contributor {
        given_name: Some("Jane".into()),
        family_name: Some("Doe".into()),
        literal_name: None,
        orcid: None,
        role: "author".into(),
    }];
    let paper = create_paper(&mut owner, new);
    let mut other = NewPaper::titled("Unrelated catalysis study");
    other.bibliography.year = Some(2019);
    create_paper(&mut owner, other);
    let attachment = import_pdf(fx, &mut owner, paper.id, "a.pdf");
    human_highlight(
        &mut owner,
        attachment,
        "dendrite growth at the interface",
        "key claim",
    );
    let org = |owner: &mut Session, r| match ok(owner.call(Command::Organization(r))) {
        Reply::Organization(o) => o,
        other => panic!("{other:?}"),
    };
    let reading = org(
        &mut owner,
        OrganizationRequest::CreateCollection(CollectionInput::named("Reading")),
    )
    .collections[0]
        .id;
    org(
        &mut owner,
        OrganizationRequest::CollectionMember(paper.id, reading, true),
    );
    let anode = org(
        &mut owner,
        OrganizationRequest::CreateTag(TagInput::named("anode")),
    )
    .tags[0]
        .id;
    org(
        &mut owner,
        OrganizationRequest::TagMember(paper.id, anode, true),
    );
    Seeded { paper, attachment }
}

/// A running papers-mcp with line-oriented access to its stdout.
struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    stderr: Option<thread::JoinHandle<String>>,
    /// Every stdout message seen so far (all must be JSON-RPC 2.0).
    seen: Vec<Value>,
    next_id: u64,
}
impl Mcp {
    fn spawn(fx: &Fixture, extra: &[&str]) -> Self {
        let mut child = Process::new(mcp_bin())
            .args(fx.common_args())
            .args(extra)
            .env("PAPERS_MCP_LOG", "debug")
            .env_remove("PAPERS_MCP_CREDENTIAL_FILE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        let mut stderr = child.stderr.take().unwrap();
        let stderr = thread::spawn(move || {
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut stderr, &mut text);
            text
        });
        Self {
            stdin: child.stdin.take(),
            child,
            lines,
            stderr: Some(stderr),
            seen: Vec::new(),
            next_id: 1000,
        }
    }
    fn send_raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }
    fn send(&mut self, message: Value) {
        self.send_raw(&message.to_string());
    }
    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }
    /// Waits for the message answering `id`; other messages are kept in `seen`.
    fn wait_for(&mut self, id: &Value, timeout: Duration) -> Option<Value> {
        if let Some(found) = self.seen.iter().find(|m| &m["id"] == id) {
            return Some(found.clone());
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            let line = self.lines.recv_timeout(left).ok()?;
            let message: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout carried a non-JSON line ({e}): {line}"));
            assert_eq!(message["jsonrpc"], "2.0", "not JSON-RPC 2.0: {line}");
            self.seen.push(message.clone());
            if &message["id"] == id {
                return Some(message);
            }
        }
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = json!(self.next_id);
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.wait_for(&id, TIMEOUT)
            .unwrap_or_else(|| panic!("no response to {method}"))
    }
    fn initialize(&mut self) -> Value {
        let response = self.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "cm-mcp-test", "version": "0"}}),
        );
        self.notify("notifications/initialized", json!({}));
        response["result"].clone()
    }
    /// tools/call result (asserting it is a result, not a protocol error).
    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let result = response["result"].clone();
        assert!(result.is_object(), "{name}: {response}");
        // Structured content is mirrored as one text block.
        let text = result["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            text.contains(&result["structuredContent"].to_string()[..2]),
            "{name}"
        );
        result
    }
    fn ok(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.tool(name, arguments);
        assert_ne!(result["isError"], true, "{name} failed: {result}");
        result["structuredContent"].clone()
    }
    fn err(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.tool(name, arguments.clone());
        assert_eq!(
            result["isError"], true,
            "{name} {arguments} should fail: {result}"
        );
        result["structuredContent"]["error"].clone()
    }
    /// Closes stdin (EOF) and waits for a clean exit; returns stderr.
    fn finish(mut self) -> (ExitStatus, String) {
        drop(self.stdin.take());
        let started = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "papers-mcp did not exit on stdin EOF"
            );
            thread::sleep(Duration::from_millis(20));
        };
        // Remaining stdout lines must still be protocol messages.
        while let Ok(line) = self.lines.recv_timeout(Duration::from_millis(200)) {
            let message: Value = serde_json::from_str(&line).expect("protocol-only stdout");
            assert_eq!(message["jsonrpc"], "2.0");
        }
        let stderr = self.stderr.take().unwrap().join().unwrap();
        (status, stderr)
    }
}
impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn operation_name(op: Operation) -> Value {
    serde_json::to_value(op).unwrap()
}
fn denied(result: Result<Result<Reply, WireError>, ClientError>) -> Operation {
    match result.unwrap() {
        Err(WireError::Denied(Denial::Forbidden(op))) => op,
        other => panic!("expected Forbidden, got {other:?}"),
    }
}

#[test]
fn read_tools_over_stdio_with_a_lazily_started_daemon() {
    let fx = Fixture::new();
    // The owner bootstrap path itself starts papersd on demand.
    assert!(!fx.paths().socket.exists());
    let grant = fx.cli("grant", &[]);
    assert_eq!(grant["capabilities"], json!(["library_read", "jobs_read"]));
    let seeded = seed(&fx);
    let paper = seeded.paper.id.0.to_string();
    let attachment = seeded.attachment.0.to_string();
    // Stop the daemon: papers-mcp must start it lazily on its first tool call.
    let killed = fx.kill_daemon();

    let mut mcp = Mcp::spawn(&fx, &[]);
    let init = mcp.initialize();
    assert_eq!(init["serverInfo"]["name"], "citation-manager");
    assert!(init["capabilities"]["tools"].is_object());
    assert!(
        init["instructions"]
            .as_str()
            .unwrap()
            .contains("Read-only by default")
    );

    let tools = mcp.request("tools/list", json!({}))["result"]["tools"].clone();
    let names: Vec<&str> = tools
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, cm_mcp::tools::TOOL_NAMES);
    for tool in tools.as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert_eq!(tool["inputSchema"]["additionalProperties"], false, "{tool}");
        let read_only = names[..4].contains(&tool["name"].as_str().unwrap());
        assert_eq!(tool["annotations"]["readOnlyHint"], read_only, "{tool}");
    }

    // search -> bibliographic hit with a complete source block.
    let found = mcp.ok("search_papers", json!({"query": "interfacial"}));
    let hit = &found["results"][0];
    assert_eq!(hit["kind"], "paper");
    let source = &hit["source"];
    assert_eq!(source["paper_id"], paper.as_str());
    assert_eq!(source["doi"], "10.5555/mcp.a");
    assert_eq!(
        source["title"],
        "Interfacial stability of solid electrolytes"
    );
    assert_eq!(source["authors"], json!(["Doe, Jane"]));
    assert_eq!(source["year"], 2024);
    assert_eq!(source["uri"], format!("paper://{paper}"));
    assert!(
        fx.daemon_pid() != killed,
        "a new papersd was started on demand"
    );

    // Annotation hits carry the stored attachment/page location.
    let found = mcp.ok(
        "search_papers",
        json!({"query": "dendrite", "sources": ["annotations"]}),
    );
    let hit = &found["results"][0];
    assert_eq!(hit["kind"], "annotation");
    assert_eq!(hit["source"]["attachment_id"], attachment.as_str());
    assert_eq!(hit["source"]["page_index"], 0);
    assert_eq!(hit["source"]["page_label"], "1");
    assert!(hit["text"].as_str().unwrap().contains("dendrite"));
    assert!(hit["uri"].as_str().unwrap().contains("?page=1&annotation="));

    // Filters only (tags, years, collection) compose into the search DSL.
    for filters in [
        json!({"tags": ["anode"]}),
        json!({"year_from": 2020, "year_to": 2030}),
        json!({"collection": "Reading"}),
    ] {
        let found = mcp.ok("search_papers", filters.clone());
        let ids: Vec<&Value> = found["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| &r["source"]["paper_id"])
            .collect();
        assert!(
            !ids.is_empty() && ids.iter().all(|id| *id == paper.as_str()),
            "{filters}: {found}"
        );
    }

    let detail = mcp.ok("get_paper", json!({"paper_id": paper}));
    assert_eq!(detail["source"]["paper_id"], paper.as_str());
    assert_eq!(detail["paper"]["journal"], "Journal of Examples");
    assert_eq!(
        detail["attachments"][0]["attachment_id"],
        attachment.as_str()
    );
    assert_eq!(detail["attachments"][0]["status"], "present");
    assert_eq!(detail["collections"][0]["name"], "Reading");
    assert_eq!(detail["tags"][0]["name"], "anode");

    // No PDF runtime in this papersd: the page text is pending, never invented.
    let text = mcp.ok(
        "get_paper_fulltext",
        json!({"paper_id": paper, "from_page_index": 0, "page_count": 2, "max_chars": 500}),
    );
    assert_eq!(text["source"]["attachment_id"], attachment.as_str());
    assert_eq!(text["pages"], json!([]));
    assert_eq!(text["text_extraction"]["state"], "pending");
    assert!(text["note"].as_str().unwrap().contains("extraction"));

    let notes = mcp.ok("get_paper_annotations", json!({"paper_id": paper}));
    assert_eq!(notes["total"], 1);
    let note = &notes["annotations"][0];
    assert_eq!(note["origin"], "human");
    assert_eq!(note["ai_confirmed"], Value::Null);
    assert_eq!(note["text"], "dendrite growth at the interface");
    assert_eq!(note["comment"], "key claim");
    assert_eq!(note["source"]["page_index"], 0);
    assert_eq!(note["source"]["page_label"], "1");
    assert_eq!(note["source"]["authors"], json!(["Doe, Jane"]));

    let (status, stderr) = mcp.finish();
    assert!(status.success(), "{stderr}");
    // Diagnostics go to stderr only.
    assert!(stderr.contains("serving MCP on stdio"), "{stderr}");
    assert!(!stderr.contains("\"jsonrpc\""), "{stderr}");
}

#[test]
fn writes_need_a_separate_grant_revocation_is_live_and_direct_ipc_is_refused_alike() {
    let fx = Fixture::new();
    let read_grant = fx.grant(&[]);
    let seeded = seed(&fx);
    let paper = seeded.paper.id.0.to_string();
    let mut mcp = Mcp::spawn(&fx, &[]);
    mcp.initialize();

    // Read-only by default: every write tool is refused by papersd, naming the operation.
    let unknown_collection = uuid::Uuid::new_v4().to_string();
    let refused = [
        (
            "add_paper_by_doi",
            json!({"doi": "10.5555/new"}),
            Operation::MetadataResolve,
        ),
        (
            "add_paper_from_url",
            json!({"url": "https://example.org/a", "title": "AI write", "confirm": true}),
            Operation::ImportPreview,
        ),
        (
            "update_paper",
            json!({"paper_id": paper, "year": 2030}),
            Operation::PaperUpdate,
        ),
        (
            "create_collection",
            json!({"name": "AI picks"}),
            Operation::OrganizationWrite,
        ),
        (
            "move_paper_to_collection",
            json!({"paper_id": paper, "collection_id": unknown_collection}),
            Operation::OrganizationWrite,
        ),
        (
            "add_tag_to_paper",
            json!({"paper_id": paper, "tags": ["ai"]}),
            Operation::OrganizationWrite,
        ),
        (
            "add_annotation",
            json!({"paper_id": paper, "kind": "note", "page_index": 0, "comment": "AI note"}),
            Operation::AnnotationWrite,
        ),
        (
            "export_citations",
            json!({"paper_ids": [paper]}),
            Operation::Export,
        ),
    ];
    for (tool, args, operation) in &refused {
        let error = mcp.err(tool, args.clone());
        assert_eq!(error["kind"], "permission_denied", "{tool}: {error}");
        assert_eq!(
            error["operation"],
            operation_name(*operation),
            "{tool}: {error}"
        );
        let flag = if *operation == Operation::Export {
            "--export"
        } else {
            "--write"
        };
        assert!(
            error["message"].as_str().unwrap().contains(flag),
            "{tool}: {error}"
        );
    }

    // The same commands sent directly over IPC with the MCP credential are refused by the
    // same grant check (the daemon, not the MCP adapter, is the authority).
    let mut direct = fx.direct(fx.credential()).unwrap();
    let mut changed = seeded.paper.clone();
    changed.bibliography.year = Some(2030);
    let new_note = NewAnnotation {
        attachment_id: seeded.attachment,
        kind: AnnotationKind::Note,
        source: Source::Ai,
        anchor: Anchor::Unresolved { reason: "x".into() },
        quote: String::new(),
        context_before: None,
        context_after: None,
        color: None,
        comment: Some("AI note".into()),
    };
    let commands = [
        Command::Resolve {
            doi: "10.5555/new".into(),
        },
        Command::Import(ImportRequestUi::Preview(ImportRequest {
            candidates: vec![PaperDraft::titled("AI write")],
            destination: ImportDestination::default(),
        })),
        Command::Library(LibraryRequest::Update(Box::new(changed))),
        Command::Organization(OrganizationRequest::CreateCollection(
            CollectionInput::named("AI picks"),
        )),
        Command::Organization(OrganizationRequest::CollectionMember(
            seeded.paper.id,
            cm_protocol::domain::CollectionId(uuid::Uuid::new_v4()),
            true,
        )),
        Command::Organization(OrganizationRequest::CreateTag(TagInput::named("ai"))),
        Command::Annotations(AnnotationRequest::Create(Box::new(new_note))),
        Command::Export(ExportRequest {
            selection: ExportSelection::Ids(vec![seeded.paper.id]),
            format: Format::Bibtex,
            path: fx.root.join("x.bib"),
        }),
    ];
    for ((tool, _, expected), command) in refused.iter().zip(commands) {
        assert_eq!(denied(direct.call(command)), *expected, "{tool}");
    }
    // Nothing was written by any refused attempt.
    match ok(fx.owner().call(Command::Query(LibraryQuery::default()))) {
        Reply::Page(page) => assert_eq!(page.papers.len(), 2),
        other => panic!("{other:?}"),
    }

    // The owner approves a separate write grant; the running server picks it up.
    let write_grant = fx.grant(&["--write"]);
    assert_ne!(write_grant, read_grant);
    let created = mcp.ok(
        "add_paper_from_url",
        json!({"url": "https://example.org/a", "title": "Approved AI write",
               "authors": ["Roe, Richard"], "year": 2025, "confirm": true}),
    );
    assert_eq!(created["status"], "created");
    assert_eq!(created["source"]["title"], "Approved AI write");
    assert_eq!(created["source"]["authors"], json!(["Roe, Richard"]));
    // Duplicate preview/confirmation: the same record again is not added silently.
    let again = mcp.ok(
        "add_paper_from_url",
        json!({"url": "https://example.org/a", "title": "Approved AI write",
               "authors": ["Roe, Richard"], "year": 2025, "confirm": true}),
    );
    assert_eq!(again["status"], "needs_confirmation", "{again}");
    assert_eq!(
        again["duplicates"][0]["source"]["paper_id"],
        created["paper_id"]
    );
    let skipped = mcp.ok(
        "add_paper_from_url",
        json!({"preview_id": again["preview_id"], "decision": "skip"}),
    );
    assert_eq!(skipped["status"], "skipped");
    // The read-only grant the credential file held before was revoked on replacement.
    let grants = fx.cli("grants", &[]);
    let revoked = |id: &str| {
        grants["grants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["id"] == id)
            .map(|g| g["revoked"].clone())
    };
    assert_eq!(revoked(&read_grant), Some(json!(true)));
    assert_eq!(revoked(&write_grant), Some(json!(false)));

    let placed = mcp.ok(
        "add_annotation",
        json!({"paper_id": paper, "kind": "highlight", "page_index": 0,
               "rects": [{"left": 72, "bottom": 600, "right": 200, "top": 612}],
               "quote": "interfacial stability", "comment": "AI: central result"}),
    );
    let annotation = &placed["annotation"];
    assert_eq!(annotation["origin"], "ai");
    assert_eq!(annotation["ai_confirmed"], false);
    assert_eq!(annotation["anchor"]["resolved"], true);
    assert_eq!(annotation["source"]["page_index"], 0);
    let anchored = mcp.ok(
        "add_annotation",
        json!({"paper_id": paper, "kind": "highlight", "page_index": 3,
               "quote": "a text anchor", "context_before": "before "}),
    );
    assert_eq!(anchored["annotation"]["anchor"]["resolved"], false);
    let ai = mcp.ok(
        "get_paper_annotations",
        json!({"paper_id": paper, "origin": "ai"}),
    );
    assert_eq!(ai["total"], 2);

    let updated = mcp.ok(
        "update_paper",
        json!({"paper_id": paper, "year": 2025, "volume": "7"}),
    );
    assert_eq!(updated["source"]["year"], 2025);
    let collection = mcp.ok("create_collection", json!({"name": "AI picks"}));
    assert_eq!(collection["status"], "created");
    assert_eq!(
        mcp.ok("create_collection", json!({"name": "AI  picks"}))["status"],
        "exists"
    );
    let collection_id = collection["collection"]["collection_id"].clone();
    let moved = mcp.ok(
        "move_paper_to_collection",
        json!({"paper_id": paper, "collection_id": collection_id, "mode": "move"}),
    );
    assert_eq!(moved["status"], "moved");
    assert_eq!(moved["collections"].as_array().unwrap().len(), 1);
    let tagged = mcp.ok(
        "add_tag_to_paper",
        json!({"paper_id": paper, "tags": ["AI reviewed", "Anode"]}),
    );
    assert_eq!(tagged["created_tags"], json!(["AI reviewed"]));
    assert_eq!(tagged["tags"].as_array().unwrap().len(), 2);
    // Export needs its own approval even with the write grant.
    let error = mcp.err("export_citations", json!({"paper_ids": [paper]}));
    assert_eq!(error["operation"], "export");

    // An MCP grant can never delete, also when used directly.
    let mut direct = fx.direct(fx.credential()).unwrap();
    assert_eq!(
        denied(direct.call(Command::Lifecycle {
            paper: seeded.paper.id,
            deleted: true
        })),
        Operation::PaperTrash
    );
    // --read-only narrows a session even when the grant allows writes.
    let mut narrowed = Mcp::spawn(&fx, &["--read-only"]);
    narrowed.initialize();
    let error = narrowed.err(
        "add_annotation",
        json!({"paper_id": paper, "kind": "note", "page_index": 0, "comment": "x"}),
    );
    assert_eq!(error["operation"], "annotation_write");
    drop(narrowed);

    // Export with an export grant returns the text (written only to a private temp dir).
    fx.grant(&["--write", "--export"]);
    let exported = mcp.ok(
        "export_citations",
        json!({"paper_ids": [paper], "format": "bibtex"}),
    );
    assert_eq!(exported["records"], 1);
    assert!(
        exported["text"].as_str().unwrap().contains("10.5555/mcp.a"),
        "{exported}"
    );
    let ris = mcp.ok(
        "export_citations",
        json!({"paper_ids": [paper], "format": "ris", "max_chars": 1000}),
    );
    assert!(ris["text"].as_str().unwrap().contains("TY  -"), "{ris}");

    // Revocation applies to the live session at once, for reads too.
    let live_grant = fx.cli("grants", &[])["grants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["revoked"] == false)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut still_open = fx.direct(fx.credential()).unwrap();
    fx.cli("revoke", &["--grant", &live_grant]);
    let error = mcp.err("search_papers", json!({"query": "interfacial"}));
    assert_eq!(error["kind"], "grant_revoked", "{error}");
    let error = mcp.err(
        "add_annotation",
        json!({"paper_id": paper, "kind": "note", "comment": "x"}),
    );
    assert_eq!(error["kind"], "grant_revoked", "{error}");
    // ... and to direct IPC: the open session is denied, a new handshake rejected.
    assert_eq!(
        still_open.call(Command::Status).unwrap().unwrap_err(),
        WireError::Denied(Denial::Revoked)
    );
    assert!(matches!(
        fx.direct(fx.credential()),
        Err(ClientError::Rejected(HandshakeRejection::Unauthenticated))
    ));
    let (status, stderr) = mcp.finish();
    assert!(status.success(), "{stderr}");
}

#[test]
fn malformed_input_and_size_limits_are_tool_errors_and_the_server_continues() {
    let fx = Fixture::new();
    fx.grant(&[]);
    let seeded = seed(&fx);
    {
        let mut owner = fx.owner();
        for i in 0..2 {
            human_highlight(
                &mut owner,
                seeded.attachment,
                &format!("{} long quote number {i}", "x".repeat(40)),
                "c",
            );
        }
    }
    let paper = seeded.paper.id.0.to_string();
    let mut mcp = Mcp::spawn(&fx, &[]);
    mcp.initialize();

    // Malformed JSON: Parse error with id null, then the server keeps serving.
    mcp.send_raw("this is not json {");
    let parse = mcp
        .wait_for(&Value::Null, TIMEOUT)
        .expect("parse error reply");
    assert_eq!(parse["error"]["code"], -32700);
    mcp.send_raw(r#"{"jsonrpc":"2.0","id":"bad-params","method":"tools/call","params":7}"#);
    let invalid = mcp.wait_for(&json!("bad-params"), TIMEOUT).unwrap();
    assert_eq!(invalid["error"]["code"], -32600);
    // An unknown tool is a protocol error (unroutable); bad arguments are tool errors.
    let unknown = mcp.request(
        "tools/call",
        json!({"name": "drop_library", "arguments": {}}),
    );
    assert_eq!(unknown["error"]["code"], -32602, "{unknown}");
    assert!(mcp.request("ping", json!({}))["result"].is_object());

    // Bounds of every tool are enforced before any daemon call: a read-only grant gets
    // invalid_input (not permission_denied) for oversized write requests too.
    let long = |n: usize| "a".repeat(n);
    let ids = |n: usize| {
        (0..n)
            .map(|_| uuid::Uuid::new_v4().to_string())
            .collect::<Vec<_>>()
    };
    let cases = [
        ("search_papers", json!({})),
        ("search_papers", json!({"query": long(1001)})),
        ("search_papers", json!({"query": "x", "limit": 51})),
        ("search_papers", json!({"query": "x", "limit": 0})),
        (
            "search_papers",
            json!({"query": "x", "snippet_chars": 1001}),
        ),
        (
            "search_papers",
            json!({"query": "x", "tags": (0..11).map(|i| i.to_string()).collect::<Vec<_>>()}),
        ),
        ("search_papers", json!({"query": "x", "bogus": true})),
        ("search_papers", json!({"year_from": 2025, "year_to": 2020})),
        (
            "search_papers",
            json!({"query": "x", "cursor": {"scan_after": -1, "offset": 0}}),
        ),
        ("get_paper", json!({"paper_id": "not-a-uuid"})),
        (
            "get_paper",
            json!({"paper_id": paper, "abstract_chars": 20001}),
        ),
        (
            "get_paper_fulltext",
            json!({"paper_id": paper, "page_count": 21}),
        ),
        (
            "get_paper_fulltext",
            json!({"paper_id": paper, "page_count": 0}),
        ),
        (
            "get_paper_fulltext",
            json!({"paper_id": paper, "max_chars": 100001}),
        ),
        (
            "get_paper_fulltext",
            json!({"paper_id": paper, "from_page_index": -1}),
        ),
        (
            "get_paper_annotations",
            json!({"paper_id": paper, "limit": 501}),
        ),
        (
            "get_paper_annotations",
            json!({"paper_id": paper, "max_chars": 200001}),
        ),
        ("add_paper_by_doi", json!({})),
        ("add_paper_by_doi", json!({"doi": long(513)})),
        ("add_paper_by_doi", json!({"preview_id": "p"})),
        (
            "add_paper_from_url",
            json!({"url": "file:///etc/passwd", "title": "x"}),
        ),
        (
            "add_paper_from_url",
            json!({"url": format!("https://e.org/{}", long(2040)), "title": "x"}),
        ),
        (
            "add_paper_from_url",
            json!({"url": "https://e.org", "title": "x", "authors": vec!["a"; 101]}),
        ),
        (
            "update_paper",
            json!({"paper_id": paper, "title": long(2001)}),
        ),
        (
            "update_paper",
            json!({"paper_id": paper, "authors": vec!["a"; 201]}),
        ),
        ("create_collection", json!({"name": ""})),
        ("create_collection", json!({"name": long(257)})),
        (
            "move_paper_to_collection",
            json!({"paper_id": paper, "collection_id": paper, "mode": "copy"}),
        ),
        ("add_tag_to_paper", json!({"paper_id": paper, "tags": []})),
        (
            "add_tag_to_paper",
            json!({"paper_id": paper, "tags": (0..21).map(|i| i.to_string()).collect::<Vec<_>>()}),
        ),
        (
            "add_annotation",
            json!({"paper_id": paper, "kind": "highlight"}),
        ),
        (
            "add_annotation",
            json!({"paper_id": paper, "kind": "note", "comment": "c", "context_before": long(49)}),
        ),
        (
            "add_annotation",
            json!({"paper_id": paper, "kind": "note", "comment": "c", "page_index": 0,
            "rects": vec![json!({"left": 0, "bottom": 0, "right": 1, "top": 1}); 65]}),
        ),
        (
            "add_annotation",
            json!({"paper_id": paper, "kind": "note", "comment": long(8001)}),
        ),
        ("export_citations", json!({"paper_ids": []})),
        ("export_citations", json!({"paper_ids": ids(201)})),
        (
            "export_citations",
            json!({"paper_ids": [paper], "format": "csl"}),
        ),
    ];
    for (tool, args) in cases {
        let error = mcp.err(tool, args.clone());
        assert_eq!(error["kind"], "invalid_input", "{tool} {args}: {error}");
    }
    // An unknown paper is a clean not_found tool error.
    let error = mcp.err(
        "get_paper",
        json!({"paper_id": uuid::Uuid::new_v4().to_string()}),
    );
    assert_eq!(error["kind"], "not_found");

    // Count and character caps on successful reads.
    let page = mcp.ok(
        "get_paper_annotations",
        json!({"paper_id": paper, "limit": 2}),
    );
    assert_eq!(
        (page["returned"].clone(), page["total"].clone()),
        (json!(2), json!(3))
    );
    assert_eq!(page["truncated"], true);
    let page = mcp.ok(
        "get_paper_annotations",
        json!({"paper_id": paper, "max_chars": 10}),
    );
    assert_eq!(
        page["annotations"][0]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        10
    );
    assert_eq!(page["annotations"][0]["truncated"], true);
    assert_eq!(page["truncated"], true);
    let found = mcp.ok(
        "search_papers",
        json!({"query": "quote", "limit": 1, "snippet_chars": 32}),
    );
    assert_eq!(found["returned"], 1);
    assert!(found["total_hits"].as_u64().unwrap() >= 2);
    let next = found["next_cursor"].clone();
    assert!(next.is_object(), "{found}");
    let second = mcp.ok(
        "search_papers",
        json!({"query": "quote", "limit": 1, "cursor": next}),
    );
    assert_eq!(second["returned"], 1);
    assert_ne!(second["results"][0]["uri"], found["results"][0]["uri"]);
    let detail = mcp.ok("get_paper", json!({"paper_id": paper, "abstract_chars": 0}));
    assert_eq!(detail["paper"]["abstract"], Value::Null);

    let (status, stderr) = mcp.finish();
    assert!(status.success(), "{stderr}");
    assert!(stderr.contains("unparsable JSON-RPC input"), "{stderr}");
}

#[test]
fn cancellation_stops_waiting_and_a_restarted_daemon_is_reconnected() {
    let fx = Fixture::new();
    fx.grant(&[]);
    seed(&fx);
    let mut mcp = Mcp::spawn(&fx, &[]);
    mcp.initialize();
    mcp.ok("search_papers", json!({"query": "interfacial"}));
    let first = fx.daemon_pid();

    // Freeze papersd so the request is in flight, then cancel it.
    signal(first, rustix::process::Signal::STOP);
    let id = json!("slow-search");
    mcp.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                    "params": {"name": "search_papers", "arguments": {"query": "interfacial"}}}));
    thread::sleep(Duration::from_millis(300));
    mcp.notify(
        "notifications/cancelled",
        json!({"requestId": id, "reason": "user aborted"}),
    );
    // The server stays responsive while the daemon is frozen.
    assert!(mcp.request("ping", json!({}))["result"].is_object());
    signal(first, rustix::process::Signal::CONT);
    let after = mcp.ok("search_papers", json!({"query": "interfacial"}));
    assert_eq!(after["returned"], 1);
    // The cancelled request never got a response (rmcp suppresses it; the IPC reply that
    // arrives after papersd resumes is dropped by the client).
    assert!(mcp.wait_for(&id, Duration::from_millis(500)).is_none());

    // Service restart between calls: the next call reconnects and starts papersd again.
    fx.kill_daemon();
    let again = mcp.ok("search_papers", json!({"query": "interfacial"}));
    assert_eq!(again["returned"], 1);
    let second = fx.daemon_pid();
    assert_ne!(second, first);

    // Restart while a read is in flight: the lost read is retried once on a new daemon.
    signal(second, rustix::process::Signal::STOP);
    let id = json!("in-flight");
    mcp.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                    "params": {"name": "get_paper_annotations",
                               "arguments": {"paper_id": again["results"][0]["source"]["paper_id"]}}}));
    thread::sleep(Duration::from_millis(300));
    signal(second, rustix::process::Signal::KILL);
    let response = mcp.wait_for(&id, TIMEOUT).expect("retried read answered");
    assert_ne!(response["result"]["isError"], true, "{response}");
    assert_eq!(response["result"]["structuredContent"]["total"], 1);
    assert_ne!(fx.daemon_pid(), second);

    let (status, stderr) = mcp.finish();
    assert!(status.success(), "{stderr}");
    assert!(stderr.contains("retrying the read once"), "{stderr}");
}

#[test]
fn a_missing_credential_is_explained_without_touching_the_library() {
    let fx = Fixture::new();
    let mut mcp = Mcp::spawn(&fx, &[]);
    mcp.initialize();
    let error = mcp.err("search_papers", json!({"query": "x"}));
    assert_eq!(error["kind"], "no_credential");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("papers mcp grant")
    );
    // No daemon was started and no library was created for a client without a credential.
    assert!(!fx.library.exists());
    assert!(!Path::new(&fx.paths().socket).exists());
    let (status, _) = mcp.finish();
    assert!(status.success());
}
