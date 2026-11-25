//! #28 subprocess tests: the real `papers` binary against an isolated temp library and a
//! private runtime directory. papersd is NOT running beforehand; the CLI starts it lazily from
//! next to itself (`target/<profile>/papersd`, built by `cargo test --workspace` or
//! `cargo build -p cm-daemon --bin papersd`). Every daemon is stopped when its test ends.
//! No network: DOI lookups are covered with a scripted daemon in the unit tests.
#![cfg(unix)]
use cm_protocol::{
    annotations::{Anchor, AnnotationKind, AnnotationRequest, NewAnnotation, Source},
    client::{
        Session,
        local::{DaemonPaths, LocalConnector},
    },
    domain::AttachmentId,
    ipc::{Capability, Command, GrantCommand, GrantReply, Reply},
};
use serde_json::Value;
use std::{
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
};

const PAPERS: &str = env!("CARGO_BIN_EXE_papers");

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}
impl Run {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout is not one JSON document ({e}): {}\nstderr: {}",
                self.stdout, self.stderr
            )
        })
    }
    /// Asserts the exit code and the `papers.v1` envelope, returning `data`.
    fn expect(&self, code: i32) -> Value {
        assert_eq!(
            self.code, code,
            "exit code; stdout: {}\nstderr: {}",
            self.stdout, self.stderr
        );
        let v = self.json();
        assert_eq!(v["schema"], "papers.v1");
        assert_eq!(v["ok"], code == 0, "{v}");
        if code != 0 {
            assert_eq!(v["error"]["exit_code"], code);
            assert!(
                self.stderr.contains("papers: error:"),
                "diagnostic on stderr: {}",
                self.stderr
            );
        }
        v["data"].clone()
    }
}

struct Env {
    _temp: tempfile::TempDir,
    _run: tempfile::TempDir,
    root: PathBuf,
    library: PathBuf,
    runtime: PathBuf,
}
impl Env {
    fn new() -> Self {
        let papersd = Path::new(PAPERS).with_file_name("papersd");
        assert!(
            papersd.is_file(),
            "{} missing: run `cargo build -p cm-daemon --bin papersd` (cargo test --workspace builds it)",
            papersd.display()
        );
        let temp = tempfile::tempdir().unwrap();
        // Unix socket paths are short: keep the runtime directory directly under /tmp.
        let run = tempfile::Builder::new()
            .prefix("cmcli")
            .tempdir_in("/tmp")
            .unwrap();
        let root = temp.path().canonicalize().unwrap();
        Self {
            library: root.join("library"),
            runtime: run.path().canonicalize().unwrap().join("run"),
            root,
            _temp: temp,
            _run: run,
        }
    }
    fn papers(&self, args: &[&str]) -> Run {
        self.papers_with(args, &[])
    }
    fn papers_with(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
        self.run(&args, env)
    }
    /// Only the Linux-only non-UTF-8 path test needs raw OS-string arguments.
    #[cfg(target_os = "linux")]
    fn papers_os(&self, args: &[&std::ffi::OsStr]) -> Run {
        self.run(args, &[])
    }
    fn run(&self, args: &[&std::ffi::OsStr], env: &[(&str, &str)]) -> Run {
        let mut command = Process::new(PAPERS);
        command
            .args(args)
            .current_dir(&self.root)
            .env_remove("PAPERS_CREDENTIAL_FILE")
            .env_remove("XDG_RUNTIME_DIR")
            .env("PAPERS_LIBRARY", &self.library)
            .env("PAPERS_RUNTIME_DIR", &self.runtime)
            .env("PAPERS_DAEMON_IDLE_TIMEOUT_MS", "20000")
            .stdin(Stdio::null());
        for (k, v) in env {
            command.env(k, v);
        }
        let output = command.output().unwrap();
        Run {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }
    fn uid(&self) -> u32 {
        std::fs::metadata(&self.root).unwrap().uid()
    }
    /// Owner session straight over the protocol (test setup only, e.g. issuing grants).
    fn owner(&self) -> Session {
        let paths = DaemonPaths::new(&self.library, Some(&self.runtime), self.uid()).unwrap();
        Session::open(&LocalConnector::new(paths, self.uid(), "test-owner")).unwrap()
    }
    fn paper_count(&self) -> usize {
        self.papers(&["list", "--limit", "500"]).expect(0)["count"]
            .as_u64()
            .unwrap() as usize
    }
}
impl Env {
    /// `papers server stop`, which verifies the daemon's pid through socket peer credentials.
    /// That is Linux-only for now (#34): elsewhere the command must refuse with exit 7, and
    /// the test stops papersd by the pid `server status` reports instead.
    fn stop_daemon(&self) {
        if cfg!(target_os = "linux") {
            assert_eq!(self.papers(&["server", "stop"]).expect(0)["stopped"], true);
            return;
        }
        let status = self.papers(&["server", "status"]).expect(0);
        let refused = self.papers(&["server", "stop"]);
        refused.expect(7);
        if let Some(pid) = status["pid"].as_u64() {
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .status();
        }
        for _ in 0..100 {
            if self.papers(&["server", "status"]).expect(0)["running"] == false {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("papersd did not stop");
    }
}
impl Drop for Env {
    fn drop(&mut self) {
        if cfg!(target_os = "linux") {
            let _ = self.papers(&["server", "stop", "--force", "--no-start"]);
        } else if let Ok(status) =
            serde_json::from_str::<Value>(&self.papers(&["server", "status"]).stdout)
            && let Some(pid) = status["data"]["pid"].as_u64()
        {
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .status();
        }
    }
}

fn ok(
    result: Result<Result<Reply, cm_protocol::ipc::WireError>, cm_protocol::client::ClientError>,
) -> Reply {
    result.unwrap().unwrap()
}

/// Issues a grant through an owner session and stores its credential with `mode`.
fn issue(env: &Env, name: &str, capabilities: &[Capability], mode: u32) -> PathBuf {
    let mut owner = env.owner();
    let credential = match ok(owner.call(Command::Grants(GrantCommand::Issue {
        label: name.into(),
        capabilities: capabilities.iter().copied().collect(),
        expires_at_ms: None,
    }))) {
        Reply::Grants(GrantReply::Issued { credential, .. }) => credential,
        other => panic!("{other:?}"),
    };
    let file = env.root.join(name);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&file)
        .and_then(|mut f| std::io::Write::write_all(&mut f, credential.0.as_bytes()))
        .unwrap();
    file
}

/// Two entries with the same DOI: with `--batch 1` they land in different preview batches.
const SAME_DOI_BIB: &str = r#"@article{first,
  title = {First copy},
  year = {2020},
  doi = {10.5555/same.doi}
}
@article{second,
  title = {Second copy with another title},
  year = {2021},
  doi = {10.5555/SAME.DOI}
}
"#;

#[test]
fn duplicates_across_batches_are_decided_before_anything_is_committed() {
    let env = Env::new();
    env.write("same.bib", SAME_DOI_BIB);
    let plan = env
        .papers(&["import", "same.bib", "--batch", "1", "--dry-run"])
        .expect(0);
    assert_eq!(plan["plan"]["create"], 1);
    assert_eq!(plan["plan"]["needs_decision"], 1);
    assert_eq!(plan["rows"][1]["decision"]["code"], "duplicate");
    // Without a policy nothing at all is committed, not even the first batch.
    let refused = env.papers(&["import", "same.bib", "--batch", "1", "--yes"]);
    refused.expect(4);
    assert_eq!(refused.json()["data"]["saved"], false);
    assert_eq!(env.paper_count(), 0);
    let skipped = env
        .papers(&[
            "import",
            "same.bib",
            "--batch",
            "1",
            "--on-duplicate",
            "skip",
            "--yes",
        ])
        .expect(0);
    assert_eq!(skipped["plan"]["skip_duplicate"], 1);
    assert_eq!(skipped["result"]["created"].as_array().unwrap().len(), 1);
    assert_eq!(env.paper_count(), 1);
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_paths_never_panic_the_json_output() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let env = Env::new();
    // Inputs/outputs: the JSON protocol cannot carry them, so they are refused (exit 2) with a
    // normal envelope instead of a panic (exit 101).
    let bib = OsStr::from_bytes(b"refs-\xff.bib");
    std::fs::write(env.root.join(bib), BIB).unwrap();
    let pdf = OsStr::from_bytes(b"paper-\xff.pdf");
    std::fs::write(env.root.join(pdf), "%PDF-1.4 bytes\n").unwrap();
    let out = OsStr::from_bytes(b"out-\xff.bib");
    for args in [
        vec![OsStr::new("import"), bib, OsStr::new("--dry-run")],
        vec![
            OsStr::new("add"),
            OsStr::new("file"),
            pdf,
            OsStr::new("--yes"),
        ],
        vec![OsStr::new("export"), OsStr::new("-o"), out],
    ] {
        let run = env.papers_os(&args);
        run.expect(2);
        assert_eq!(run.json()["error"]["code"], "invalid_input");
        assert!(run.stderr.contains("UTF-8"), "{}", run.stderr);
    }
    // A non-UTF-8 library path is reported lossily in JSON.
    let library = env.root.join(OsStr::from_bytes(b"lib-\xff"));
    let library = library.as_os_str().to_str().map(str::to_owned);
    assert!(library.is_none());
    let output = Process::new(PAPERS)
        .args(["server", "status"])
        .env(
            "PAPERS_LIBRARY",
            env.root.join(OsStr::from_bytes(b"lib-\xff")),
        )
        .env("PAPERS_RUNTIME_DIR", &env.runtime)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(v["data"]["running"], false);
    assert!(
        v["data"]["library"]
            .as_str()
            .unwrap()
            .ends_with("lib-\u{fffd}")
    );
}

const BIB: &str = r#"@article{alpha2024,
  title = {Lithium magnesium interface dynamics},
  author = {Kanno, Ryoji and Smith, Jane},
  journal = {Journal of Batteries},
  year = {2024},
  doi = {10.1234/alpha.2024}
}
@inproceedings{beta2023,
  title = {Anode coatings revisited},
  author = {Doe, John},
  booktitle = {Proceedings of Energy},
  year = {2023},
  doi = {10.1234/beta.2023}
}
"#;

#[test]
fn file_import_search_show_fulltext_annotations_export_without_gui() {
    let env = Env::new();

    // Nothing runs yet; status never starts the daemon.
    let status = env.papers(&["server", "status"]).expect(0);
    assert_eq!(status["running"], false);
    // A read lazily starts papersd.
    assert_eq!(env.paper_count(), 0);
    assert_eq!(env.papers(&["server", "status"]).expect(0)["running"], true);

    // add file: provisional title from the filename, unresolved metadata shown explicitly.
    env.write("Solid_State_Notes.pdf", "%PDF-1.4 cli fixture\n");
    let dry = env
        .papers(&["add", "file", "Solid_State_Notes.pdf", "--dry-run"])
        .expect(0);
    assert_eq!(dry["mode"], "new");
    assert_eq!(dry["provisional_title"], true);
    assert_eq!(dry["saved"], false);
    assert!(
        dry["unresolved"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m.as_str().unwrap().contains("provisional title")),
        "{dry}"
    );
    assert_eq!(
        dry["preview"]["candidates"][0]["title"],
        "Solid_State_Notes"
    );
    // Non-interactive without --yes: explicit refusal, nothing saved.
    let refused = env.papers(&["add", "file", "Solid_State_Notes.pdf"]);
    assert_eq!(refused.expect(4)["saved"], false);
    assert_eq!(refused.json()["error"]["code"], "confirmation_required");
    assert!(refused.stderr.contains("--yes"));
    assert_eq!(env.paper_count(), 0);
    let added = env
        .papers(&["add", "file", "Solid_State_Notes.pdf", "--yes"])
        .expect(0);
    assert_eq!(added["saved"], true);
    let paper = added["result"]["paper_id"].as_str().unwrap().to_owned();
    let attachment = added["result"]["attachment_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(env.paper_count(), 1);

    // Explicit attach to the existing paper never edits its metadata.
    env.write("supplement.pdf", "%PDF-1.4 supplement\n");
    let attached = env
        .papers(&[
            "add",
            "file",
            "supplement.pdf",
            "--attach-to",
            &paper,
            "--yes",
        ])
        .expect(0);
    assert_eq!(attached["mode"], "attach");
    assert!(attached["attachment_id"].is_string());

    let shown = env.papers(&["show", &paper]).expect(0);
    assert_eq!(shown["paper"]["title"], "Solid_State_Notes");
    assert_eq!(shown["attachments"].as_array().unwrap().len(), 2);
    assert!(
        shown["attachments"][0]["uri"]
            .as_str()
            .unwrap()
            .starts_with(&format!("paper://{paper}/attachment/"))
    );
    assert_eq!(
        env.papers(&["show", "00000000-0000-7000-8000-000000000001"])
            .expect(3),
        Value::Null
    );

    // Full text: bounded read; this build has no PDF runtime, so extraction stays pending.
    let text = env.papers(&[
        "fulltext",
        &paper,
        "--page",
        "1",
        "--attachment",
        &attachment,
    ]);
    let read = text.expect(0);
    assert_eq!(read["attachment_id"], attachment.as_str());
    assert_eq!(read["limits"]["max_pages"], 1);
    assert!(read["extraction"]["state"].is_string());
    assert_eq!(env.papers(&["fulltext", &paper, "--pages", "1-60"]).code, 2);

    // Annotations made elsewhere (here: an owner session) are listed with 1-based pages.
    let attachment_id: AttachmentId =
        serde_json::from_value(Value::String(attachment.clone())).unwrap();
    let mut owner = env.owner();
    ok(
        owner.call(Command::Annotations(AnnotationRequest::Create(Box::new(
            NewAnnotation {
                attachment_id,
                kind: AnnotationKind::Note,
                source: Source::Human,
                anchor: Anchor::Resolved {
                    page_index: 2,
                    page_label: Some("iii".into()),
                    quads: vec![],
                },
                quote: String::new(),
                context_before: None,
                context_after: None,
                color: None,
                comment: Some("check the interface model".into()),
            },
        )))),
    );
    drop(owner);
    let notes = env.papers(&["annotations", &paper]).expect(0);
    assert_eq!(notes["total"], 1);
    assert_eq!(notes["annotations"][0]["page_number"], 3);
    assert_eq!(notes["annotations"][0]["page_label"], "iii");
    assert_eq!(
        notes["annotations"][0]["comment"],
        "check the interface model"
    );

    // BibTeX import: preview, explicit commit, then exact duplicates need a decision.
    env.write("refs.bib", BIB);
    let plan = env.papers(&["import", "refs.bib", "--dry-run"]).expect(0);
    assert_eq!(plan["records"], 2);
    assert_eq!(plan["plan"]["create"], 2);
    assert_eq!(
        env.papers(&["import", "refs.bib"]).expect(4)["saved"],
        false
    );
    let imported = env.papers(&["import", "refs.bib", "--yes"]).expect(0);
    assert_eq!(imported["result"]["created"].as_array().unwrap().len(), 2);
    assert_eq!(env.paper_count(), 3);
    let again = env.papers(&["import", "refs.bib", "--yes"]);
    again.expect(4);
    assert_eq!(again.json()["error"]["code"], "duplicate");
    assert_eq!(again.json()["data"]["rows"][0]["matches"][0]["kind"], "doi");
    let skipped = env
        .papers(&["import", "refs.bib", "--on-duplicate", "skip", "--yes"])
        .expect(0);
    assert_eq!(skipped["plan"]["skip_duplicate"], 2);
    assert_eq!(env.paper_count(), 3);
    // Even --on-duplicate create cannot create an exact DOI duplicate.
    env.papers(&["import", "refs.bib", "--on-duplicate", "create", "--yes"])
        .expect(4);

    // Search and list.
    let hits = env
        .papers(&["search", "magnesium", "--limit", "5"])
        .expect(0);
    assert_eq!(
        hits["hits"][0]["title"],
        "Lithium magnesium interface dynamics"
    );
    assert_eq!(hits["hits"][0]["source"], "bibliographic");
    assert_eq!(
        env.papers(&["search", "magnesium", "--limit", "101"]).code,
        2
    );
    let first = env
        .papers(&["list", "--limit", "1", "--sort", "year", "--desc"])
        .expect(0);
    assert_eq!(first["papers"][0]["year"], 2024);
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let second = env
        .papers(&[
            "list", "--limit", "1", "--sort", "year", "--desc", "--cursor", &cursor,
        ])
        .expect(0);
    assert_eq!(second["papers"][0]["year"], 2023);

    // Export to stdout is the bibliography itself; the summary goes to stderr.
    let raw = env.papers(&["export", "--format", "bibtex"]);
    assert_eq!(raw.code, 0, "{}", raw.stderr);
    assert!(raw.stdout.contains("Lithium magnesium interface dynamics"));
    assert!(raw.stderr.contains("exported 3 records"));
    // Safe file export: new file, no clobber without --force, never inside the library.
    let out = env.root.join("out.bib");
    let written = env
        .papers(&["export", "-o", out.to_str().unwrap()])
        .expect(0);
    assert_eq!(written["records"], 3);
    assert!(
        std::fs::read_to_string(&out)
            .unwrap()
            .contains("Anode coatings")
    );
    env.papers(&["export", "-o", out.to_str().unwrap()])
        .expect(4);
    assert_eq!(
        env.papers(&["export", "-o", out.to_str().unwrap(), "--force"])
            .expect(0)["replaced"],
        true
    );
    std::os::unix::fs::symlink(&out, env.root.join("link.bib")).unwrap();
    env.papers(&["export", "-o", "link.bib", "--force"])
        .expect(4);
    let inside = env.library.join("leak.bib");
    env.papers(&["export", "-o", inside.to_str().unwrap()])
        .expect(5);
    assert!(!inside.exists());
    let alpha = hits["hits"][0]["paper_id"].as_str().unwrap().to_owned();
    env.papers(&[
        "export", "--format", "ris", "--ids", &alpha, "-o", "sel.ris",
    ])
    .expect(0);
    let ris = std::fs::read_to_string(env.root.join("sel.ris")).unwrap();
    assert!(ris.contains("TY  -") && ris.contains("Lithium magnesium"));
    // No scratch directories are left behind.
    assert!(std::fs::read_dir(&env.root).unwrap().flatten().all(|e| {
        !e.file_name()
            .to_string_lossy()
            .starts_with(".papers-export")
    }));

    // Text mode: human output on stdout, no JSON.
    let human = env.papers(&["--text", "show", &paper]);
    assert_eq!(human.code, 0);
    assert!(human.stdout.starts_with(&paper));

    // Stop: the daemon exits and status reports it without restarting it.
    env.stop_daemon();
    assert_eq!(
        env.papers(&["server", "status"]).expect(0)["running"],
        false
    );
}

#[test]
fn read_only_sessions_and_grants_are_refused_writes() {
    let env = Env::new();
    env.write("paper.pdf", "%PDF-1.4 scope\n");
    env.write("refs.bib", BIB);

    // --read-only narrows the session to LibraryRead: the daemon refuses every write (exit 5).
    for args in [
        &["--read-only", "add", "file", "paper.pdf", "--yes"][..],
        &["--read-only", "import", "refs.bib", "--yes"][..],
        &["--read-only", "add", "doi", "10.1234/x", "--yes"][..],
        &["--read-only", "export", "-o", "x.bib"][..],
    ] {
        let run = env.papers(args);
        run.expect(5);
        assert_eq!(run.json()["error"]["code"], "permission_denied", "{args:?}");
    }
    env.papers(&["--read-only", "list"]).expect(0);
    assert_eq!(env.paper_count(), 0);

    // A credential whose daemon grant is read-only (as MCP's default would be).
    let mut owner = env.owner();
    let credential = match ok(owner.call(Command::Grants(GrantCommand::Issue {
        label: "cli read-only test".into(),
        capabilities: [Capability::LibraryRead].into_iter().collect(),
        expires_at_ms: None,
    }))) {
        Reply::Grants(GrantReply::Issued { credential, .. }) => credential,
        other => panic!("{other:?}"),
    };
    drop(owner);
    let file = env.root.join("readonly.credential");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&file)
        .and_then(|mut f| std::io::Write::write_all(&mut f, credential.0.as_bytes()))
        .unwrap();
    let cred = file.to_str().unwrap();
    env.papers(&["--credential-file", cred, "list"]).expect(0);
    for args in [
        &[
            "--credential-file",
            cred,
            "add",
            "file",
            "paper.pdf",
            "--yes",
        ][..],
        &["--credential-file", cred, "import", "refs.bib", "--yes"][..],
        &["--credential-file", cred, "export", "-o", "y.bib"][..],
    ] {
        env.papers(args).expect(5);
    }
    // The same via the environment variable.
    env.papers_with(
        &["add", "file", "paper.pdf", "--yes"],
        &[("PAPERS_CREDENTIAL_FILE", cred)],
    )
    .expect(5);
    assert_eq!(env.paper_count(), 0);

    // server stop needs GrantAdmin, proven by the daemon: refused for the read-only grant
    // and under --read-only, and the daemon keeps running.
    let refused = env.papers(&["--credential-file", cred, "server", "stop", "--force"]);
    refused.expect(5);
    env.papers(&["--read-only", "server", "stop", "--force"])
        .expect(5);
    assert_eq!(env.papers(&["server", "status"]).expect(0)["running"], true);

    // add file --dry-run needs no write capability: a preview-only grant can review a PDF
    // (0400 credential file) but cannot register it.
    let preview_only = issue(
        &env,
        "preview-only.credential",
        &[
            Capability::LocalFileRead,
            Capability::LibraryRead,
            Capability::CapturePreview,
        ],
        0o400,
    );
    let preview_only = preview_only.to_str().unwrap();
    let dry = env
        .papers(&[
            "--credential-file",
            preview_only,
            "add",
            "file",
            "paper.pdf",
            "--dry-run",
        ])
        .expect(0);
    assert_eq!(dry["preview"]["candidates"][0]["title"], "paper");
    env.papers(&[
        "--credential-file",
        preview_only,
        "add",
        "file",
        "paper.pdf",
        "--yes",
    ])
    .expect(5);
    env.papers(&[
        "--credential-file",
        preview_only,
        "import",
        "refs.bib",
        "--dry-run",
    ])
    .expect(0);
    assert_eq!(env.paper_count(), 0);

    // A credential the daemon does not know is unauthenticated.
    let bogus = env.root.join("bogus.credential");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&bogus)
        .and_then(|mut f| std::io::Write::write_all(&mut f, b"00"))
        .unwrap();
    env.papers(&["--credential-file", bogus.to_str().unwrap(), "list"])
        .expect(5);
}

#[test]
fn usage_unavailable_features_and_daemon_errors_have_distinct_exit_codes() {
    let env = Env::new();
    // --no-start with no daemon: exit 6, nothing started.
    env.papers(&["--no-start", "list"]).expect(6);
    assert_eq!(
        env.papers(&["server", "status"]).expect(0)["running"],
        false
    );
    // clap usage errors: exit 2, message on stderr, nothing on stdout.
    let bad = env.papers(&["list", "--limit", "0"]);
    assert_eq!(bad.code, 2);
    assert!(bad.stdout.is_empty() && !bad.stderr.is_empty());
    // No library configured.
    let output = Process::new(PAPERS)
        .args(["list"])
        .env_remove("PAPERS_LIBRARY")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    // An invalid DOI is rejected by the daemon before any network lookup.
    let doi = env.papers(&["add", "doi", "not a doi", "--dry-run"]);
    assert_eq!(doi.json()["error"]["code"], "invalid_input");
    doi.expect(2);

    // MCP: stdout stays clean (it belongs to the protocol); exit 7 with a clear message.
    let mcp = env.papers(&["mcp"]);
    assert_eq!(mcp.code, 7);
    assert!(mcp.stdout.is_empty());
    assert!(
        mcp.stderr.contains("MCP is not available"),
        "{}",
        mcp.stderr
    );
    let rest = env.papers(&["server", "start", "--rest"]);
    rest.expect(7);
    assert_eq!(rest.json()["error"]["code"], "feature_unavailable");

    // The earlier daemon-backed checks started papersd: stop it, then start explicitly.
    env.stop_daemon();
    let started = env.papers(&["server", "start"]).expect(0);
    assert_eq!(started["started"], true);
    assert_eq!(env.papers(&["server", "start"]).expect(0)["started"], false);

    // Help with examples and completions for three shells.
    let help = env.papers(&["--help"]);
    assert_eq!(help.code, 0);
    assert!(help.stdout.contains("Examples:") && help.stdout.contains("papers add doi"));
    for shell in ["bash", "zsh", "fish"] {
        let completion = env.papers(&["completions", shell]);
        assert_eq!(completion.code, 0);
        assert!(completion.stdout.contains("papers"), "{shell}");
    }
}
