//! The real `papers-native-host` executable driven exactly like a browser (argv + native
//! frames on stdin/stdout) against a protocol-level fake daemon.
//!
//! `harness = false`: when started as `papersd --library ... --runtime-dir ...` (the host's
//! lazy start of its sibling), this binary *is* the fake daemon. That proves the host starts
//! `papersd` from its own directory by absolute path, in its own process, and exits while
//! the daemon keeps running. The real #25 daemon round trip is the browser smoke
//! (scripts/native-host/browser_smoke.py).
use cm_native_host::{
    caller::Allowlist,
    cli::{DEV_CHROME_EXTENSION_ID, DEV_FIREFOX_EXTENSION_ID},
    config::{CONFIG_VERSION, HostConfig, Store, base_dir_for, euid},
    daemon::capture_capabilities,
    manifest::Browser,
};
use cm_protocol::{
    capture::{CaptureCommand, CaptureCommitted, CapturePreview, CaptureReply, CaptureResolution},
    client::local::DaemonPaths,
    import::{ImportOutcome, ImportPreview, PaperDraft, PreviewId},
    ipc::{
        ClientFrame, Command, Credential, Denial, GrantCommand, GrantId, GrantInfo, GrantReply,
        MAX_HANDSHAKE_FRAME, MAX_REQUEST_FRAME, MAX_RESPONSE_FRAME, Operation, Reply, ServerFrame,
        Welcome, WireError, read_frame, write_frame,
    },
    organization::{Organization, OrganizationRequest},
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const SECRET: &str = "host-test-credential";
const DESKTOP_SECRET: &str = "owner-test-credential";
/// Makes the in-process fake daemon refuse `GrantCommand::Revoke` (revocation failure).
static FAIL_REVOKE: AtomicBool = AtomicBool::new(false);
static NEXT_GRANT: AtomicU64 = AtomicU64::new(1);
const PAGE_MARKER: &str = "PAGE-CONTENT-MUST-NOT-BE-LOGGED";

// ---------------------------------------------------------------- fake daemon

type Log = Arc<Mutex<Vec<String>>>;

fn reply_for(command: Command) -> Result<Reply, WireError> {
    match command {
        Command::Organization(OrganizationRequest::List) => {
            Ok(Reply::Organization(Organization::default()))
        }
        Command::Capture(CaptureCommand::Preview(request)) => {
            if request.page_url.contains("denied") {
                return Err(WireError::Denied(Denial::Forbidden(
                    Operation::CaptureSubmit,
                )));
            }
            Ok(Reply::Capture(CaptureReply::Preview(Box::new(
                CapturePreview {
                    preview: ImportPreview {
                        id: PreviewId("preview-1".into()),
                        candidates: Vec::new(),
                        warnings: Vec::new(),
                    },
                    resolution: CaptureResolution {
                        page_url: request.page_url.clone(),
                        draft: PaperDraft::titled("Fixture"),
                        identifiers: Vec::new(),
                        fields: Vec::new(),
                        tiers_used: Vec::new(),
                        provider_outcomes: Vec::new(),
                        provisional: false,
                        pdf_candidates: Vec::new(),
                    },
                },
            ))))
        }
        Command::Capture(CaptureCommand::Commit(commit)) => {
            if commit.commit.preview_id.0 != "preview-1" {
                return Err(WireError::Service(
                    cm_protocol::library::ServiceError::Conflict,
                ));
            }
            Ok(Reply::Capture(CaptureReply::Committed(Box::new(
                CaptureCommitted {
                    outcome: ImportOutcome::default(),
                    paper: None,
                    pdf_job: None,
                    pdf_error: None,
                },
            ))))
        }
        Command::Capture(CaptureCommand::Cancel(_)) => Ok(Reply::Capture(CaptureReply::Cancelled)),
        Command::Grants(GrantCommand::Issue {
            label,
            capabilities,
            expires_at_ms,
        }) => {
            let n = NEXT_GRANT.fetch_add(1, Ordering::SeqCst);
            Ok(Reply::Grants(GrantReply::Issued {
                grant: GrantInfo {
                    id: grant_id(n),
                    label,
                    capabilities,
                    created_at_ms: 0,
                    expires_at_ms,
                    revoked: false,
                },
                credential: Credential(format!("issued-{n}")),
            }))
        }
        Command::Grants(GrantCommand::Revoke(id)) if !FAIL_REVOKE.load(Ordering::SeqCst) => {
            Ok(Reply::Grants(GrantReply::Revoked(id)))
        }
        Command::Grants(GrantCommand::Revoke(_)) => Err(WireError::Unavailable),
        _ => Err(WireError::Denied(Denial::Forbidden(Operation::Export))),
    }
}

fn serve_connection(mut stream: std::os::unix::net::UnixStream, log: Log) {
    let Ok(Some(ClientFrame::Hello(hello))) =
        read_frame::<_, ClientFrame>(&mut stream, MAX_HANDSHAKE_FRAME)
    else {
        return;
    };
    let capture = hello.credential == Credential(SECRET.into())
        && hello.requested == Some(capture_capabilities());
    let owner = hello.credential == Credential(DESKTOP_SECRET.into()) && hello.requested.is_none();
    let accepted = hello.client_kind == "native-host" && (capture || owner);
    log.lock()
        .unwrap()
        .push(format!("hello accepted={accepted}"));
    if !accepted {
        let _ = write_frame(
            &mut stream,
            &ServerFrame::Rejected(cm_protocol::ipc::HandshakeRejection::Unauthenticated),
            MAX_RESPONSE_FRAME,
        );
        return;
    }
    let welcome = Welcome {
        protocol_version: cm_protocol::ipc::PROTOCOL_VERSION,
        daemon_version: "fake".into(),
        session: 1,
        grant: cm_protocol::ipc::GrantId(uuid::Uuid::nil()),
        capabilities: capture_capabilities(),
        max_request_frame: MAX_REQUEST_FRAME,
        max_in_flight: 1,
    };
    if write_frame(
        &mut stream,
        &ServerFrame::Welcome(welcome),
        MAX_RESPONSE_FRAME,
    )
    .is_err()
    {
        return;
    }
    while let Ok(Some(ClientFrame::Request { id, command })) =
        read_frame::<_, ClientFrame>(&mut stream, MAX_REQUEST_FRAME)
    {
        let entry = match &command {
            Command::Grants(GrantCommand::Revoke(id)) => format!("GrantRevoke {id}"),
            other => format!("{:?}", other.operation()),
        };
        log.lock().unwrap().push(entry);
        let frame = ServerFrame::Response {
            id,
            result: reply_for(command),
        };
        if write_frame(&mut stream, &frame, MAX_RESPONSE_FRAME).is_err() {
            return;
        }
    }
}

/// Serves until no connection arrives for `idle`.
fn fake_daemon(listener: UnixListener, log: Log, idle: Duration) {
    listener.set_nonblocking(true).unwrap();
    let mut last = Instant::now();
    while last.elapsed() < idle {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                let log = log.clone();
                thread::spawn(move || serve_connection(stream, log));
                last = Instant::now();
            }
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// Spawned-papersd mode: `papersd --library L --runtime-dir R`.
fn run_as_papersd(args: &[OsString]) {
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
            .expect("flag")
    };
    let (library, runtime) = (value("--library"), value("--runtime-dir"));
    let paths = DaemonPaths::new(&library, Some(&runtime), euid()).unwrap();
    let listener = UnixListener::bind(&paths.socket).unwrap();
    let log: Log = Arc::default();
    let marker = runtime.join("fake-papersd.log");
    fs::write(
        &marker,
        format!("started pid={} args={args:?}\n", std::process::id()),
    )
    .unwrap();
    fake_daemon(listener, log.clone(), Duration::from_secs(3));
    let _ = fs::remove_file(&paths.socket);
    let mut file = fs::OpenOptions::new().append(true).open(&marker).unwrap();
    writeln!(file, "served {:?}", log.lock().unwrap()).unwrap();
    writeln!(file, "exited").unwrap();
}

// ---------------------------------------------------------------- fixture

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    host: PathBuf,
    library: PathBuf,
    runtime: PathBuf,
    store: Store,
}
impl Fixture {
    /// A private copy of the host in `<root>/bin` (optionally with a sibling papersd).
    fn new(with_papersd: bool) -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let root = temp.path().canonicalize().unwrap();
        let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        let host = bin.join("papers-native-host");
        fs::copy(env!("CARGO_BIN_EXE_papers-native-host"), &host).unwrap();
        if with_papersd {
            fs::copy(std::env::current_exe().unwrap(), bin.join("papersd")).unwrap();
        }
        let runtime = root.join("run");
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::new(&config_base(&root));
        let root_for_config = root.clone();
        let fixture = Self {
            library: root.join("library"),
            store,
            host,
            runtime,
            root,
            _temp: temp,
        };
        fixture
            .store
            .save(&HostConfig {
                version: CONFIG_VERSION,
                library: fixture.library.clone(),
                runtime_dir: Some(fixture.runtime.clone()),
                host_path: fixture.host.clone(),
                allowed: Allowlist {
                    chrome: [DEV_CHROME_EXTENSION_ID.to_owned()].into(),
                    firefox: [DEV_FIREFOX_EXTENSION_ID.to_owned()].into(),
                },
                manifests: vec![cm_native_host::config::ManifestRecord {
                    browser: cm_native_host::manifest::Browser::Firefox,
                    path: firefox_manifest(&root_for_config),
                }],
                grant: None,
                revoke_pending: Vec::new(),
                unregistered: false,
            })
            .unwrap();
        fixture
            .store
            .save_credential(&Credential(SECRET.into()))
            .unwrap();
        fixture
    }
    fn socket(&self) -> PathBuf {
        DaemonPaths::new(&self.library, Some(&self.runtime), euid())
            .unwrap()
            .socket
    }
    fn in_process_daemon(&self) -> Log {
        let listener = UnixListener::bind(self.socket()).unwrap();
        let log: Log = Arc::default();
        let served = log.clone();
        thread::spawn(move || fake_daemon(listener, served, Duration::from_secs(30)));
        log
    }
    /// Runs the host with browser-style argv; returns (exit code, responses, stderr).
    fn run(&self, args: &[&str], input: Vec<u8>) -> (i32, Vec<Value>, String) {
        let mut child = Process::new(&self.host)
            .args(args)
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let writer = thread::spawn(move || {
            let _ = stdin.write_all(&input);
            // Dropping stdin is the browser closing the port.
        });
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "host did not exit"
            );
            thread::sleep(Duration::from_millis(10));
        };
        writer.join().unwrap();
        let mut out = Vec::new();
        child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
        let mut err = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut err)
            .unwrap();
        let mut responses = Vec::new();
        let mut cursor = out.as_slice();
        while let Some(body) = cm_native_host::frame::read_message(&mut cursor, u32::MAX).unwrap() {
            responses.push(serde_json::from_slice(&body).unwrap());
        }
        (status.code().unwrap_or(-1), responses, err)
    }
}

fn framed(messages: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for m in messages {
        let body = serde_json::to_vec(m).unwrap();
        out.extend_from_slice(&(body.len() as u32).to_ne_bytes());
        out.extend_from_slice(&body);
    }
    out
}

fn chrome_origin() -> String {
    format!("chrome-extension://{DEV_CHROME_EXTENSION_ID}/")
}

fn preview(url: &str) -> Value {
    json!({"v":1,"id":"p","type":"capture.preview","capture":{
        "page_url": url, "title": PAGE_MARKER, "offline": true,
        "html": format!("<html><head><title>{PAGE_MARKER}</title></head></html>"),
        "meta": [{"name":"citation_title","content":PAGE_MARKER}],
        "client_kind": "desktop", "capabilities": ["library_write"]}})
}

// ---------------------------------------------------------------- tests

fn chrome_round_trip_maps_only_capture_commands() {
    let f = Fixture::new(false);
    let log = f.in_process_daemon();
    let (code, r, err) = f.run(
        &[&chrome_origin()],
        framed(&[
            json!({"v":1,"id":"h","type":"hello"}),
            json!({"v":1,"id":"d","type":"destinations"}),
            preview("https://journal.example.org/paper"),
            json!({"v":1,"id":"c","type":"capture.commit","preview_id":"preview-1",
                   "decisions":[{"candidate_index":0,"action":"Create"}],"pdf":null}),
            json!({"v":1,"id":"s","type":"capture.commit","preview_id":"stale",
                   "decisions":[{"candidate_index":0,"action":"Create"}]}),
            json!({"v":1,"id":"x","type":"capture.cancel","preview_id":"preview-1"}),
            preview("https://journal.example.org/denied"),
            json!({"v":1,"id":"g","type":"grants.issue","label":"x"}),
            json!({"v":1,"id":"e","type":"export"}),
        ]),
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(r.len(), 9);
    assert_eq!(r[0]["result"]["paired"], json!(true));
    assert_eq!(r[0]["result"]["browser"], "chrome");
    assert_eq!(r[0]["result"]["protocol"], 1);
    assert_eq!(r[1]["type"], "destinations");
    assert_eq!(r[1]["result"], json!({"collections": [], "tags": []}));
    assert_eq!(r[2]["result"]["preview"]["id"], "preview-1");
    assert_eq!(r[3]["type"], "capture.commit");
    assert_eq!(r[3]["ok"], true);
    assert_eq!(r[4]["error"]["code"], "conflict");
    assert_eq!(r[4]["error"]["retryable"], true);
    assert_eq!(r[5]["result"], json!({"cancelled": true}));
    assert_eq!(r[6]["error"]["code"], "denied");
    assert_eq!(r[6]["error"]["detail"]["operation"], "capture_submit");
    assert_eq!(r[7]["error"]["code"], "unknown_type");
    assert_eq!(r[8]["error"]["code"], "unknown_type");
    // hello and unknown types never reach the daemon; each request is one session.
    let log = log.lock().unwrap().clone();
    let ops: Vec<&str> = log
        .iter()
        .filter(|l| !l.starts_with("hello"))
        .map(String::as_str)
        .collect();
    assert_eq!(
        ops,
        [
            "OrganizationList",
            "CaptureSubmit",
            "CaptureSave",
            "CaptureSave",
            "CaptureCancel",
            "CaptureSubmit"
        ]
    );
    assert!(
        log.iter()
            .filter(|l| l.starts_with("hello"))
            .all(|l| l == "hello accepted=true")
    );
    // Diagnostics only: no page content, URL or credential on stderr.
    for secret in [PAGE_MARKER, "journal.example.org", SECRET] {
        assert!(!err.contains(secret), "stderr leaked {secret}: {err}");
    }
    assert!(err.contains("capture.preview: ok"), "{err}");
}

fn firefox_argv_is_accepted_and_unknown_callers_are_rejected() {
    let f = Fixture::new(false);
    let log = f.in_process_daemon();
    let manifest = firefox_manifest(&f.root);
    let manifest = manifest.to_str().unwrap();
    let hello = framed(&[
        json!({"v":1,"id":1,"type":"hello"}),
        json!({"v":1,"type":"destinations"}),
    ]);
    let (code, r, err) = f.run(&[manifest, DEV_FIREFOX_EXTENSION_ID], hello.clone());
    assert_eq!(code, 0, "{err}");
    assert_eq!(r[0]["id"], 1);
    assert_eq!(r[0]["result"]["browser"], "firefox");
    assert_eq!(r[1]["ok"], true);

    let other_chrome = format!("chrome-extension://{}/", "a".repeat(32));
    for args in [
        vec![other_chrome.as_str()],
        vec![manifest, "evil@example.org"],
        // An allowed ID launched through a manifest `register` did not write.
        vec![
            "/tmp/other/dev.citation_manager.native_host.json",
            DEV_FIREFOX_EXTENSION_ID,
        ],
    ] {
        let (code, r, err) = f.run(&args, hello.clone());
        assert_eq!(code, 3, "{args:?}: {err}");
        assert!(r.is_empty(), "rejected callers get no protocol output");
        assert!(
            err.contains("not allowed") || err.contains("not registered"),
            "{err}"
        );
    }
    for args in [
        vec![],
        vec!["--library", "/x"],
        vec!["chrome-extension://short/"],
    ] {
        let (code, r, _) = f.run(&args, hello.clone());
        assert_eq!((code, r.len()), (2, 0), "{args:?}");
    }
    // Only the accepted Firefox session reached the daemon.
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with("hello"))
            .count(),
        1
    );
}

fn oversized_and_invalid_frames() {
    let f = Fixture::new(false);
    let mut input = framed(&[json!({"v":1,"id":"ok","type":"hello"})]);
    input.extend_from_slice(b"\x03\0\0\0{x}");
    input.extend_from_slice(&(cm_native_host::frame::MAX_INCOMING + 1).to_ne_bytes());
    input.extend_from_slice(&[b' '; 1024]);
    let (code, r, _) = f.run(&[&chrome_origin()], input);
    assert_eq!(code, 4);
    assert_eq!(r.len(), 3);
    assert_eq!(r[0]["ok"], true);
    assert_eq!(r[1]["error"]["code"], "bad_message");
    assert_eq!(r[2]["error"]["code"], "too_large");
}

fn unpaired_and_unreachable_daemon_errors() {
    let f = Fixture::new(false);
    // No daemon and no sibling papersd: a retryable error, promptly.
    let started = Instant::now();
    let (code, r, _) = f.run(
        &[&chrome_origin()],
        framed(&[json!({"v":1,"type":"destinations"})]),
    );
    assert_eq!(code, 0);
    assert_eq!(r[0]["error"]["code"], "daemon_unavailable");
    assert_eq!(r[0]["error"]["retryable"], true);
    assert!(started.elapsed() < Duration::from_secs(10));
    fs::remove_file(f.store.credential_path()).unwrap();
    let (_, r, _) = f.run(
        &[&chrome_origin()],
        framed(&[
            json!({"v":1,"type":"hello"}),
            json!({"v":1,"type":"destinations"}),
        ]),
    );
    assert_eq!(r[0]["result"]["paired"], false);
    assert_eq!(r[1]["error"]["code"], "not_paired");
    // A wrong credential is reported as needing a new pairing.
    f.store
        .save_credential(&Credential("stale".into()))
        .unwrap();
    let _log = f.in_process_daemon();
    let (_, r, _) = f.run(
        &[&chrome_origin()],
        framed(&[json!({"v":1,"type":"destinations"})]),
    );
    assert_eq!(r[0]["error"]["code"], "unauthorized");
}

fn lazy_starts_sibling_papersd_and_exits_before_it() {
    let f = Fixture::new(true);
    let marker = f.runtime.join("fake-papersd.log");
    assert!(!f.socket().exists());
    let (code, r, err) = f.run(
        &[&chrome_origin()],
        framed(&[
            json!({"v":1,"id":"d","type":"destinations"}),
            preview("https://e.org/p"),
        ]),
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(r[0]["ok"], true, "{r:?}");
    assert_eq!(r[1]["ok"], true, "{r:?}");
    // The host has exited; the daemon it started is still serving.
    let log = fs::read_to_string(&marker).unwrap();
    assert!(log.starts_with("started"), "{log}");
    assert!(
        log.contains(&format!("{:?}", f.library.as_os_str())),
        "{log}"
    );
    assert!(
        !log.contains("exited"),
        "daemon must outlive the host: {log}"
    );
    assert!(f.socket().exists());
    let started = Instant::now();
    while !fs::read_to_string(&marker).unwrap().contains("exited") {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "fake daemon never idled out"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let log = fs::read_to_string(&marker).unwrap();
    assert!(
        log.contains("OrganizationList") && log.contains("CaptureSubmit"),
        "{log}"
    );
}

fn cli(host: &Path, root: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Process::new(host)
        .args(args)
        .env_clear()
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn register_refresh_after_move_and_unregister() {
    let f = Fixture::new(true);
    f.store.remove().unwrap();
    let manifests = f.root.join("profile/NativeMessagingHosts");
    let firefox = firefox_manifest(&f.root).parent().unwrap().to_owned();
    let chromium = format!("chromium={}", manifests.display());
    let library = f.library.to_str().unwrap().to_owned();
    // Nothing to refresh before registration; registration needs explicit IDs.
    assert_eq!(cli(&f.host, &f.root, &["refresh"]).0, 0);
    let (code, _, err) = cli(
        &f.host,
        &f.root,
        &["register", "--library", &library, "--browser", &chromium],
    );
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("--development-extensions"), "{err}");
    let (code, out, err) = cli(
        &f.host,
        &f.root,
        &[
            "register",
            "--library",
            &library,
            "--browser",
            &chromium,
            "--browser",
            "firefox",
            "--development-extensions",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("not paired yet"), "{out}");
    assert!(
        err.contains("development smoke-test extension IDs"),
        "{err}"
    );
    let chrome_manifest = manifests.join("dev.citation_manager.native_host.json");
    let firefox_manifest = firefox.join("dev.citation_manager.native_host.json");
    let read = |p: &Path| serde_json::from_slice::<Value>(&fs::read(p).unwrap()).unwrap();
    let doc = read(&chrome_manifest);
    assert_eq!(doc["path"], f.host.to_str().unwrap());
    assert_eq!(doc["allowed_origins"], json!([chrome_origin()]));
    assert_eq!(
        read(&firefox_manifest)["allowed_extensions"],
        json!([DEV_FIREFOX_EXTENSION_ID])
    );
    assert_eq!(cli(&f.host, &f.root, &["status"]).0, 0);

    // The app is moved: the old path no longer exists and browsers would fail to start it.
    let moved = f.root.join("moved-bin");
    fs::rename(f.host.parent().unwrap(), &moved).unwrap();
    let host = moved.join("papers-native-host");
    let (code, status, _) = cli(&host, &f.root, &["status"]);
    assert_eq!(code, 1, "stale registration must be reported: {status}");
    let (code, out, err) = cli(&host, &f.root, &["refresh"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("re-registered"), "{out}");
    assert_eq!(read(&chrome_manifest)["path"], host.to_str().unwrap());
    assert_eq!(read(&firefox_manifest)["path"], host.to_str().unwrap());
    // IDs survive the refresh; a second refresh is a no-op.
    assert_eq!(
        read(&chrome_manifest)["allowed_origins"],
        json!([chrome_origin()])
    );
    assert_eq!(
        cli(&host, &f.root, &["refresh"]),
        (0, String::new(), String::new())
    );
    assert_eq!(cli(&host, &f.root, &["status"]).0, 0);

    // A host without its sibling papersd is not published.
    fs::remove_file(moved.join("papersd")).unwrap();
    let (code, _, err) = cli(&host, &f.root, &["register", "--development-extensions"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("papersd must be installed next to the host"),
        "{err}"
    );

    let (code, out, err) = cli(&host, &f.root, &["unregister"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("removed"), "{out}");
    assert!(!chrome_manifest.exists() && !firefox_manifest.exists());
    assert!(f.store.load().is_err());
}

/// The per-user Firefox manifest for the test HOME, as the host resolves it on this OS.
fn firefox_manifest(root: &Path) -> PathBuf {
    Browser::Firefox
        .user_dirs(&root.join("home"))
        .0
        .join("dev.citation_manager.native_host.json")
}

/// Firefox's profile root for the test HOME (`.mozilla` / `Library/.../Mozilla`).
fn firefox_profile(root: &Path) -> PathBuf {
    Browser::Firefox.user_dirs(&root.join("home")).1
}

/// The configuration directory the host binary reads for the test environment
/// (`HOME=<root>/home`, `XDG_CONFIG_HOME=<root>/config`) on this platform.
fn config_base(root: &Path) -> PathBuf {
    base_dir_for(
        Some(root.join("home").into_os_string()),
        Some(root.join("config").into_os_string()),
    )
    .unwrap()
}

fn grant_id(n: u64) -> GrantId {
    GrantId(uuid::Uuid::from_u128(u128::from(n)))
}

impl Fixture {
    /// The owner's bootstrap credential where `LocalConnector` expects it.
    fn owner_credential(&self) {
        let state = self.library.join("daemon");
        fs::create_dir_all(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let file = state.join("desktop.credential");
        fs::write(&file, DESKTOP_SECRET).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn config(&self) -> HostConfig {
        self.store.load().unwrap()
    }
    fn log_has(log: &Log, entry: &str) -> bool {
        log.lock().unwrap().iter().any(|l| l == entry)
    }
}

/// `pair` never leaves an issued grant unrecorded, and `unregister` keeps the ids of grants
/// it could not revoke (instead of deleting the only record of a live grant).
fn grants_are_never_orphaned() {
    let f = Fixture::new(false);
    f.owner_credential();
    f.store.remove_credential().unwrap();
    let log = f.in_process_daemon();
    let first = NEXT_GRANT.load(Ordering::SeqCst);
    let (code, out, err) = cli(&f.host, &f.root, &["pair"]);
    assert_eq!(code, 0, "{out}{err}");
    let g1 = grant_id(first);
    assert_eq!(f.config().grant, Some(g1));
    assert_eq!(f.store.credential().unwrap().0, format!("issued-{first}"));

    // Storing the new secret fails after the grant was issued: it is revoked again and
    // the previous pairing stays intact.
    f.store.remove_credential().unwrap();
    fs::create_dir(f.store.credential_path()).unwrap();
    let (code, _, err) = cli(&f.host, &f.root, &["pair", "--force"]);
    assert_eq!(code, 1, "{err}");
    let g2 = grant_id(first + 1);
    assert!(
        Fixture::log_has(&log, &format!("GrantRevoke {}", g2.0)),
        "{log:?}"
    );
    let config = f.config();
    assert_eq!(
        (config.grant, config.revoke_pending.clone()),
        (Some(g1), vec![])
    );
    fs::remove_dir(f.store.credential_path()).unwrap();

    // Re-pairing revokes the replaced grant.
    let (code, _, err) = cli(&f.host, &f.root, &["pair", "--force"]);
    assert_eq!(code, 0, "{err}");
    let g3 = grant_id(first + 2);
    assert!(Fixture::log_has(&log, &format!("GrantRevoke {}", g1.0)));
    assert_eq!(f.config().grant, Some(g3));

    // A replaced grant whose revocation fails is kept as pending, not forgotten.
    FAIL_REVOKE.store(true, Ordering::SeqCst);
    let (code, _, err) = cli(&f.host, &f.root, &["pair", "--force"]);
    assert_eq!(code, 1, "{err}");
    let g4 = grant_id(first + 3);
    let config = f.config();
    assert_eq!(
        (config.grant, config.revoke_pending.clone()),
        (Some(g4), vec![g3])
    );

    // Revocation fails during unregister: config stays (unregistered) with both ids.
    let (code, _, err) = cli(&f.host, &f.root, &["unregister"]);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("run `papers-native-host unregister` again"),
        "{err}"
    );
    let config = f.config();
    assert!(config.unregistered);
    assert_eq!(config.grant, None);
    assert_eq!(config.revoke_pending, vec![g3, g4]);
    assert!(
        f.store.credential().is_err(),
        "the secret is not needed to revoke"
    );
    let (code, r, err) = f.run(
        &[&chrome_origin()],
        framed(&[json!({"v":1,"type":"hello"})]),
    );
    assert_eq!(
        (code, r.len()),
        (3, 0),
        "unregistered host must refuse browsers: {err}"
    );
    let (code, status, _) = cli(&f.host, &f.root, &["status"]);
    assert_eq!(code, 1);
    assert!(status.contains("revoke_pending"), "{status}");

    // Retry once the daemon can revoke: everything is removed.
    FAIL_REVOKE.store(false, Ordering::SeqCst);
    let (code, out, err) = cli(&f.host, &f.root, &["unregister"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(&format!("revoked grant {}", g3.0)), "{out}");
    assert!(out.contains(&format!("revoked grant {}", g4.0)), "{out}");
    assert_eq!(f.store.load_optional(), Ok(None));
}

/// `--keep-grant` really keeps the grant (config + credential) and contacts no daemon;
/// `register` restores browser access with it.
fn keep_grant_keeps_the_credential() {
    let f = Fixture::new(true);
    let mut config = f.config();
    config.grant = Some(grant_id(999));
    f.store.save(&config).unwrap();
    let (code, out, err) = cli(&f.host, &f.root, &["unregister", "--keep-grant"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("kept"), "{out}");
    assert!(
        !f.runtime.join("fake-papersd.log").exists(),
        "no daemon was contacted"
    );
    assert!(f.config().unregistered);
    assert_eq!(f.config().grant, Some(grant_id(999)));
    assert_eq!(f.store.credential().unwrap().0, SECRET);
    let hello = framed(&[json!({"v":1,"type":"hello"})]);
    assert_eq!(f.run(&[&chrome_origin()], hello.clone()).0, 3);
    let (code, _, err) = cli(&f.host, &f.root, &["register", "--development-extensions"]);
    assert_eq!(code, 0, "{err}");
    assert!(!f.config().unregistered);
    let (code, r, _) = f.run(&[&chrome_origin()], hello);
    assert_eq!((code, r[0]["result"]["paired"].clone()), (0, json!(true)));
}

/// A corrupt or unsafe config is an error, never treated as "not registered" (which would
/// overwrite it or delete it with the grant id inside).
fn corrupt_config_fails_clearly() {
    let f = Fixture::new(true);
    fs::write(f.store.config_path(), b"{ not json").unwrap();
    let library = f.library.to_str().unwrap().to_owned();
    for args in [
        vec![
            "register",
            "--library",
            library.as_str(),
            "--development-extensions",
        ],
        vec!["refresh"],
        vec!["unregister"],
        vec!["pair"],
    ] {
        let (code, _, err) = cli(&f.host, &f.root, &args);
        assert_eq!(code, 1, "{args:?}");
        assert!(err.contains("fix or remove"), "{args:?}: {err}");
        assert_eq!(fs::read(f.store.config_path()).unwrap(), b"{ not json");
    }
    assert_eq!(f.store.credential().unwrap().0, SECRET);
}

/// Manifests only carry valid UTF-8 paths.
fn non_utf8_host_path_is_rejected() {
    use std::os::unix::ffi::OsStrExt;
    if cfg!(target_os = "macos") {
        // APFS/HFS+ refuse non-UTF-8 file names, so such a host path cannot occur there.
        return;
    }
    let f = Fixture::new(true);
    let odd = f.root.join(std::ffi::OsStr::from_bytes(b"bin-\xff"));
    fs::rename(f.host.parent().unwrap(), &odd).unwrap();
    let (code, _, err) = cli(
        &odd.join("papers-native-host"),
        &f.root,
        &["register", "--development-extensions"],
    );
    assert_eq!(code, 1);
    assert!(err.contains("not valid UTF-8"), "{err}");
}

/// No manifest with an empty allowlist; `refresh` does not recreate a deleted profile.
fn manifests_without_ids_or_profile_are_skipped() {
    let f = Fixture::new(true);
    f.store.remove().unwrap();
    let chromium_dir = f.root.join("chromium-profile/NativeMessagingHosts");
    let chromium = format!("chromium={}", chromium_dir.display());
    let library = f.library.to_str().unwrap().to_owned();
    let (code, _, err) = cli(
        &f.host,
        &f.root,
        &[
            "register",
            "--library",
            &library,
            "--browser",
            &chromium,
            "--browser",
            "firefox",
            "--firefox-extension-id",
            DEV_FIREFOX_EXTENSION_ID,
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("no chromium extension ID allowed"), "{err}");
    assert!(
        !err.contains("development smoke-test"),
        "no dev warning without dev IDs"
    );
    assert!(
        !chromium_dir
            .join("dev.citation_manager.native_host.json")
            .exists()
    );
    assert!(firefox_manifest(&f.root).exists());

    let moved = f.root.join("moved-bin");
    fs::rename(f.host.parent().unwrap(), &moved).unwrap();
    fs::remove_dir_all(firefox_profile(&f.root)).unwrap();
    let (code, _, err) = cli(&moved.join("papers-native-host"), &f.root, &["refresh"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("browser profile no longer exists"), "{err}");
    assert!(
        !firefox_profile(&f.root).exists(),
        "refresh must not recreate profiles"
    );
    assert!(!chromium_dir.exists());
    assert_eq!(f.config().host_path, moved.join("papers-native-host"));
}

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.iter().any(|a| a == "--library") {
        run_as_papersd(&args);
        return;
    }
    let filter = args
        .iter()
        .find_map(|a| a.to_str().filter(|a| !a.starts_with('-')));
    let tests: [(&str, fn()); 11] = [
        (
            "chrome_round_trip_maps_only_capture_commands",
            chrome_round_trip_maps_only_capture_commands,
        ),
        (
            "firefox_argv_is_accepted_and_unknown_callers_are_rejected",
            firefox_argv_is_accepted_and_unknown_callers_are_rejected,
        ),
        ("oversized_and_invalid_frames", oversized_and_invalid_frames),
        (
            "unpaired_and_unreachable_daemon_errors",
            unpaired_and_unreachable_daemon_errors,
        ),
        (
            "lazy_starts_sibling_papersd_and_exits_before_it",
            lazy_starts_sibling_papersd_and_exits_before_it,
        ),
        (
            "register_refresh_after_move_and_unregister",
            register_refresh_after_move_and_unregister,
        ),
        ("grants_are_never_orphaned", grants_are_never_orphaned),
        (
            "keep_grant_keeps_the_credential",
            keep_grant_keeps_the_credential,
        ),
        ("corrupt_config_fails_clearly", corrupt_config_fails_clearly),
        (
            "non_utf8_host_path_is_rejected",
            non_utf8_host_path_is_rejected,
        ),
        (
            "manifests_without_ids_or_profile_are_skipped",
            manifests_without_ids_or_profile_are_skipped,
        ),
    ];
    let mut ran = 0;
    for (name, test) in tests {
        if filter.is_some_and(|f| !name.contains(f)) {
            continue;
        }
        print!("test {name} ... ");
        test();
        println!("ok");
        ran += 1;
    }
    println!("\ntest result: ok. {ran} passed; 0 failed");
}
