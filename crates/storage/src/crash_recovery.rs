//! Private unit-test subprocesses only. No product switch, binary, API or environment mutation.
use crate::{
    LibraryDb, LibraryDirectory,
    attachments::files,
    backup::{restore_library, validate_bundle},
};
use cm_core::{
    attachments::{AttachmentLibrary, ImportAttachment},
    papers::{NewPaper, PaperLibrary, PaperList},
    ports::{AttachmentRepository, PaperRepository},
};
use cm_domain::{Attachment, Bibliography, Contributor, ExternalId, Paper};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const ENTRY: &str = "crash_recovery::owned_child_entry";
const WAIT: Duration = Duration::from_secs(10);
struct Context {
    root: PathBuf,
    nonce: String,
    stage: String,
}
static CHILD: OnceLock<Context> = OnceLock::new();

/// In ordinary unit tests this is inert. Only the exact guarded child entry initializes CHILD.
pub(crate) fn checkpoint(stage: &str, database: Option<&str>) {
    let Some(context) = CHILD.get() else { return };
    if context.stage != stage {
        return;
    }
    assert_eq!(
        database.map(Path::new),
        Some(context.root.join("library/library.sqlite").as_path())
    );
    rendezvous(context, &context.nonce, stage);
}
fn rendezvous(context: &Context, nonce: &str, stage: &str) {
    let mut stream = UnixStream::connect(context.root.join("s")).unwrap();
    stream.set_write_timeout(Some(WAIT)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream
        .write_all(format!("{nonce}:{stage}\n").as_bytes())
        .unwrap();
    // Parent never releases this read. EOF/timeout is a failed harness, never successful crash proof.
    let mut byte = [0];
    let result = stream.read_exact(&mut byte);
    panic!("child was released without SIGKILL: {result:?}");
}

fn wait_for_parent_kill() -> ! {
    let deadline = Instant::now() + Duration::from_secs(30);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        // park_timeout may return spuriously; only the absolute deadline ends this wait.
        thread::park_timeout(remaining);
    }
    panic!("parent did not kill child");
}

/// Helper-only libtest entry: a no-op outside an explicitly owned invocation, not a crash case.
#[test]
fn owned_child_entry() {
    let Some(root) = std::env::var_os("CM_50_ROOT") else {
        return;
    };
    let args: Vec<_> = std::env::args().collect();
    assert!(args.iter().any(|a| a == "--exact") && args.iter().any(|a| a == ENTRY));
    let root = PathBuf::from(root);
    let nonce = std::env::var("CM_50_NONCE").unwrap();
    let stage = std::env::var("CM_50_STAGE").unwrap();
    assert_eq!(Uuid::parse_str(&nonce).unwrap().to_string(), nonce);
    assert_eq!(root.canonicalize().unwrap(), root);
    assert!(
        root.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with(&format!("cm-crash-{nonce}-"))
    );
    LibraryDirectory::prepare(&root).unwrap(); // exact 0700, UID, ancestor/symlink checks
    crate::db::paths::check_file_if_present(&root.join("nonce")).unwrap();
    assert_eq!(fs::read_to_string(root.join("nonce")).unwrap(), nonce);
    let context = Context { root, nonce, stage };
    match context.stage.as_str() {
        "early-exit" => return,
        "no-ready" => wait_for_parent_kill(),
        "silent-ready" => {
            let _stream = UnixStream::connect(context.root.join("s")).unwrap();
            wait_for_parent_kill();
        }
        "wrong-nonce" => rendezvous(&context, "wrong", &context.stage),
        "wrong-stage" => rendezvous(&context, &context.nonce, "wrong"),
        "metadata-transaction"
        | "metadata-committed"
        | "attachment-staged"
        | "attachment-published"
        | "attachment-transaction"
        | "attachment-committed" => {}
        _ => panic!("unknown owned child case"),
    }
    CHILD
        .set(context)
        .unwrap_or_else(|_| panic!("child initialized twice"));
    let context = CHILD.get().unwrap();
    let directory = LibraryDirectory::prepare(context.root.join("library")).unwrap();
    let mut db = LibraryDb::open(&directory).unwrap();
    db.connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    let paper: Paper =
        serde_json::from_slice(&fs::read(context.root.join("paper.json")).unwrap()).unwrap();
    if context.stage.starts_with("metadata-") {
        db.papers().update(&edited(&paper)).unwrap();
    } else {
        let source = match std::env::var("CM_50_SOURCE").unwrap().as_str() {
            "new" => "new.pdf",
            "shared" => "original.pdf",
            _ => panic!("unknown source"),
        };
        // Real public adapter route, always ingest(..., Cut::None).
        db.managed_attachments(&directory)
            .unwrap()
            .import(request(&context.root, &paper, source))
            .unwrap();
    }
    panic!("product operation missed checkpoint");
}

// No stdout pipes or reader threads. All waits have deadlines; only this Command's Child is killed.
struct OwnedChild {
    child: Child,
    // Keep even a rejected/partial handshake open until Drop has killed and reaped the child.
    channel: Option<UnixStream>,
    receipt: Arc<Mutex<Option<ExitStatus>>>,
}
impl OwnedChild {
    fn reap(&mut self, timeout: Duration) -> io::Result<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                *self.receipt.lock().unwrap() = Some(status);
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "owned child did not exit",
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn kill_reap(&mut self) -> ExitStatus {
        // Child::kill is SIGKILL on Unix. Never use a PID obtained from messages/global matching.
        self.child.kill().unwrap();
        self.reap(WAIT)
            .expect("SIGKILL child must be reaped within deadline")
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.receipt.lock().unwrap().is_some() {
            return;
        }
        let _ = self.child.kill();
        if let Err(error) = self.reap(WAIT) {
            // Do not panic a second time during unwinding. This is a failed run, never passed evidence.
            if thread::panicking() {
                eprintln!("FAILED owned child cleanup: {error}");
            } else {
                panic!("FAILED owned child cleanup: {error}");
            }
        }
    }
}
fn ready(
    listener: &UnixListener,
    child: &mut OwnedChild,
    expected: &str,
    timeout: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let stream = child.channel.insert(stream);
                // One byte at a time prevents an unbounded line allocation; one absolute deadline.
                let mut message = Vec::new();
                loop {
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "ready read timeout",
                        ));
                    }
                    stream.set_read_timeout(Some(
                        deadline
                            .saturating_duration_since(Instant::now())
                            .max(Duration::from_millis(1)),
                    ))?;
                    let mut byte = [0];
                    stream
                        .read_exact(&mut byte)
                        .map_err(|e| io::Error::new(e.kind(), format!("ready read: {e}")))?;
                    message.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                    if message.len() > 128 {
                        return Err(io::Error::other("oversized ready message"));
                    }
                }
                if message != expected.as_bytes() {
                    return Err(io::Error::other("nonce/stage mismatch"));
                }
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
        if let Some(status) = child.child.try_wait()? {
            *child.receipt.lock().unwrap() = Some(status);
            return Err(io::Error::other(format!(
                "child exited before ready: {status}"
            )));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "ready accept timeout",
            ));
        }
        // Poll readiness, never infer a product checkpoint from elapsed time.
        thread::sleep(Duration::from_millis(5));
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    nonce: String,
    directory: LibraryDirectory,
    old: Paper,
    other: Paper,
    attachments: Vec<Attachment>,
    old_relations: Vec<String>,
}
impl Fixture {
    fn new() -> Self {
        let nonce = Uuid::now_v7().to_string();
        // Short canonical path keeps sockaddr_un within macOS's length limit.
        let temp = tempfile::Builder::new()
            .prefix(&format!("cm-crash-{nonce}-"))
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")
            .unwrap();
        let root = temp.path().canonicalize().unwrap();
        LibraryDirectory::prepare(&root).unwrap();
        files::create_file(&root.join("nonce"))
            .unwrap()
            .write_all(nonce.as_bytes())
            .unwrap();
        let directory = LibraryDirectory::prepare(root.join("library")).unwrap();
        let mut db = LibraryDb::open(&directory).unwrap();
        let mut draft = NewPaper::titled("Old metadata 日本語");
        draft.bibliography = bibliography("old");
        draft.external_ids = raw_ids("old");
        let old = db.papers().create(draft).unwrap();
        let mut draft = NewPaper::titled("Unrelated preexisting paper");
        draft.bibliography = bibliography("other");
        draft.external_ids = raw_ids("other");
        let other = db.papers().create(draft).unwrap();
        files::create_file(&root.join("paper.json"))
            .unwrap()
            .write_all(&serde_json::to_vec(&old).unwrap())
            .unwrap();
        files::create_file(&root.join("original.pdf"))
            .unwrap()
            .write_all(&vec![b'o'; files::BUFFER_SIZE * 3 + 17])
            .unwrap();
        files::create_file(&root.join("new.pdf"))
            .unwrap()
            .write_all(&vec![b'n'; files::BUFFER_SIZE * 2 + 31])
            .unwrap();
        let mut attachments = vec![];
        for paper in [&old, &other] {
            attachments.push(
                db.managed_attachments(&directory)
                    .unwrap()
                    .import(request(&root, paper, "original.pdf"))
                    .unwrap(),
            );
        }
        assert_eq!(attachments[0].storage_key, attachments[1].storage_key);
        let old_relations = relations(&db);
        drop(db); // child is sole writer; no inherited connection/rollback cleanup
        Self {
            _temp: temp,
            root,
            nonce,
            directory,
            old,
            other,
            attachments,
            old_relations,
        }
    }
    fn spawn(&self, stage: &str, shared: bool) -> (UnixListener, OwnedChild) {
        let listener = UnixListener::bind(self.root.join("s")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", ENTRY, "--nocapture", "--test-threads=1"])
            .env("CM_50_ROOT", &self.root)
            .env("CM_50_NONCE", &self.nonce)
            .env("CM_50_STAGE", stage)
            .env("CM_50_SOURCE", if shared { "shared" } else { "new" })
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        (
            listener,
            OwnedChild {
                child,
                channel: None,
                receipt: Arc::new(Mutex::new(None)),
            },
        )
    }
    fn kill_at(&self, stage: &str, shared: bool) {
        let (listener, mut child) = self.spawn(stage, shared);
        ready(
            &listener,
            &mut child,
            &format!("{}:{stage}\n", self.nonce),
            WAIT,
        )
        .unwrap();
        let status = child.kill_reap();
        assert_eq!(status.signal(), Some(9));
        assert_eq!(status.code(), None);
        assert_eq!(child.child.try_wait().unwrap(), Some(status));
        eprintln!(
            "CRASH ready={stage} nonce_verified=true source={} signal=9 code=None reaped=true",
            if shared { "shared" } else { "new" }
        );
    }
    fn verify_existing(&self, db: &mut LibraryDb, expected: &Paper) {
        assert_eq!(db.papers().list(&PaperList::default()).unwrap().len(), 2);
        assert_eq!(
            db.papers().get(self.old.id).unwrap(),
            Some(expected.clone())
        );
        assert_eq!(
            db.papers().get(self.other.id).unwrap(),
            Some(self.other.clone())
        );
        // Exact raw person UUIDs/roles/order for the unrelated paper must never change.
        let current = relations(db);
        for row in self
            .old_relations
            .iter()
            .filter(|r| r.contains(&self.other.id.0.to_string()))
        {
            assert!(current.contains(row));
        }
        for attachment in &self.attachments {
            assert_eq!(
                db.attachments().get(attachment.id).unwrap(),
                Some(attachment.clone())
            );
            assert_eq!(
                files::hash(
                    fs::File::open(self.directory.path().join(attachment.storage_key.as_str()))
                        .unwrap()
                )
                .unwrap(),
                (attachment.sha256, attachment.byte_size)
            );
        }
        assert_eq!(
            digest(&self.root.join("original.pdf")),
            self.attachments[0].sha256
        );
        assert_eq!(
            digest(&self.root.join("new.pdf")),
            digest_bytes(&vec![b'n'; files::BUFFER_SIZE * 2 + 31])
        );
        crate::integrity::database_check(&db.connection).unwrap();
        let people: i64 = db
            .connection
            .query_row("SELECT count(*) FROM people", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            people as usize,
            expected.bibliography.authors.len() + self.other.bibliography.authors.len()
        );
    }
    fn portable(&self, db: &mut LibraryDb) {
        let expected = db.papers().list(&PaperList::default()).unwrap();
        let refs: Vec<_> = expected
            .iter()
            .flat_map(|p| db.attachments().for_paper(p.id).unwrap())
            .collect();
        let raw_relations = relations(db);
        let bundle_path = self.root.join("bundle");
        db.backup_library(&self.directory, &bundle_path).unwrap();
        let bundle = LibraryDirectory::prepare(&bundle_path).unwrap();
        validate_bundle(&bundle).unwrap();
        let restored = restore_library(&bundle_path, self.root.join("restored")).unwrap();
        let mut recovered = LibraryDb::open(&restored).unwrap();
        assert_eq!(
            recovered.papers().list(&PaperList::default()).unwrap(),
            expected
        );
        assert_eq!(raw_relations, relations(&recovered));
        for paper in &expected {
            assert_eq!(
                recovered.attachments().for_paper(paper.id).unwrap(),
                db.attachments().for_paper(paper.id).unwrap()
            );
        }
        for attachment in &refs {
            assert_eq!(
                files::hash(
                    fs::File::open(restored.path().join(attachment.storage_key.as_str())).unwrap()
                )
                .unwrap(),
                (attachment.sha256, attachment.byte_size)
            );
        }
        assert!(recovered.check_integrity(&restored).unwrap().is_clean());
        eprintln!(
            "CRASH backup=validated restore=fresh references={} hashes=verified raw_relations=exact",
            refs.len()
        );
    }
}
fn digest_bytes(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).into()
}
fn digest(path: &Path) -> [u8; 32] {
    files::hash(fs::File::open(path).unwrap()).unwrap().0
}
fn raw_ids(label: &str) -> Vec<ExternalId> {
    vec![
        ExternalId {
            namespace: "Unknown RAW".into(),
            value: format!("  {label}/原文  "),
        },
        ExternalId {
            namespace: "arXiv".into(),
            value: format!("{label}v2"),
        },
    ]
}
fn bibliography(label: &str) -> Bibliography {
    Bibliography {
        paper_type: Some("journal-article".into()),
        abstract_text: Some(format!("{label} abstract")),
        year: Some(2026),
        publication_date: Some("2026-11".into()),
        journal: Some(format!("{label} journal")),
        volume: Some("8".into()),
        issue: Some("2".into()),
        pages: Some("1–9".into()),
        doi: Some(format!("10.1234/{label}")),
        url: Some(format!("https://example.invalid/{label}")),
        language: Some("ja".into()),
        citation_key: Some(format!("Key{label}")),
        extra_json: Some(serde_json::json!({"generation":label,"raw":[1,"原文",null]})),
        authors: vec![
            Contributor {
                given_name: Some(format!("{label} given")),
                family_name: Some("Family".into()),
                literal_name: None,
                orcid: Some("0000-0000-0000-0001".into()),
                role: "author".into(),
            },
            Contributor {
                given_name: None,
                family_name: None,
                literal_name: Some(format!("{label} 組織")),
                orcid: None,
                role: "editor".into(),
            },
        ],
    }
}
fn edited(paper: &Paper) -> Paper {
    let mut edited = paper.clone();
    edited.title = "New metadata 全項目".into();
    edited.bibliography = bibliography("new");
    edited.bibliography.authors.reverse();
    edited.external_ids = raw_ids("new");
    edited
}
fn request(root: &Path, paper: &Paper, source: &str) -> ImportAttachment {
    ImportAttachment {
        paper_id: paper.id,
        source: root.join(source),
        original_name: source.into(),
        media_type: "application/pdf".into(),
        expected_sha256: Some(digest(&root.join(source))),
    }
}
// Include raw internal person UUIDs as well as all ordered author/external-ID fields.
fn relations(db: &LibraryDb) -> Vec<String> {
    let mut rows = vec![];
    for sql in [
        "SELECT json_array(pp.paper_id,pp.person_id,pp.role,pp.sort_order,p.given_name,p.family_name,p.literal_name,p.orcid) FROM paper_people pp JOIN people p ON p.id=pp.person_id ORDER BY pp.paper_id,pp.sort_order",
        "SELECT json_array(paper_id,namespace,value,sort_order) FROM paper_external_ids ORDER BY paper_id,sort_order",
    ] {
        rows.extend(
            db.connection
                .prepare(sql)
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        );
    }
    rows
}

#[test]
fn sigkill_metadata_transaction_recovers_whole_old_generation() {
    metadata_case("metadata-transaction");
}
#[test]
fn sigkill_metadata_commit_retains_whole_new_generation() {
    metadata_case("metadata-committed");
}
fn metadata_case(stage: &str) {
    let fixture = Fixture::new();
    fixture.kill_at(stage, false);
    let mut db = LibraryDb::open(&fixture.directory).unwrap();
    let mut expected = fixture.old.clone();
    if stage == "metadata-committed" {
        expected = edited(&expected);
        let actual = db.papers().get(expected.id).unwrap().unwrap();
        assert!(actual.updated_at.is_some());
        expected.updated_at = actual.updated_at;
    } else {
        assert_eq!(relations(&db), fixture.old_relations);
    }
    fixture.verify_existing(&mut db, &expected);
    assert!(db.check_integrity(&fixture.directory).unwrap().is_clean());
    fixture.portable(&mut db);
}

#[test]
fn sigkill_attachment_staged_reports_complete_partial() {
    attachment_case("attachment-staged", false);
}
#[test]
fn sigkill_attachment_published_reports_orphan_and_reuses_it() {
    attachment_case("attachment-published", false);
}
#[test]
fn sigkill_attachment_transaction_rolls_back_reference() {
    attachment_case("attachment-transaction", false);
}
#[test]
fn sigkill_attachment_committed_inspects_and_retains_result_without_retry() {
    attachment_case("attachment-committed", false);
}
#[test]
fn sigkill_shared_blob_transaction_preserves_refs_and_explicit_retry() {
    attachment_case("attachment-transaction", true);
}
#[test]
fn sigkill_shared_blob_commit_inspects_without_duplicate() {
    attachment_case("attachment-committed", true);
}
fn attachment_case(stage: &str, shared: bool) {
    let fixture = Fixture::new();
    fixture.kill_at(stage, shared);
    let mut db = LibraryDb::open(&fixture.directory).unwrap();
    fixture.verify_existing(&mut db, &fixture.old);
    assert_eq!(relations(&db), fixture.old_relations);
    let committed = stage == "attachment-committed";
    let staged = stage == "attachment-staged";
    // Conservative inspect-before-retry: no low-level import idempotency is assumed.
    let before = db.attachments().for_paper(fixture.old.id).unwrap();
    assert_eq!(before.len(), if committed { 2 } else { 1 });
    assert_eq!(before[0], fixture.attachments[0]);
    assert_eq!(
        db.attachments().for_paper(fixture.other.id).unwrap(),
        vec![fixture.attachments[1].clone()]
    );
    let report = db.check_integrity(&fixture.directory).unwrap();
    assert!(
        report.dangling.is_empty()
            && report.corrupt.is_empty()
            && report.unsafe_entries.is_empty()
            && report.backup_issues.is_empty()
    );
    assert_eq!(report.partials.len(), usize::from(staged));
    assert_eq!(
        report.orphans.len(),
        usize::from(!staged && !committed && !shared)
    );
    let remaining: Vec<_> = report
        .partials
        .iter()
        .chain(&report.orphans)
        .map(|p| {
            (
                p.clone(),
                digest(p),
                fs::metadata(p).unwrap().permissions().mode(),
            )
        })
        .collect();
    for (path, hash, _) in &remaining {
        assert_eq!(*hash, digest(&fixture.root.join("new.pdf")));
        if staged {
            assert_eq!(
                fs::metadata(path).unwrap().len(),
                (files::BUFFER_SIZE * 2 + 31) as u64
            );
        }
    }
    // A second inventory must preserve names, bytes and permissions; no cleanup/repair.
    let again = db.check_integrity(&fixture.directory).unwrap();
    assert_eq!(report.partials, again.partials);
    assert_eq!(report.orphans, again.orphans);
    for (path, hash, mode) in &remaining {
        assert_eq!(digest(path), *hash);
        assert_eq!(fs::metadata(path).unwrap().permissions().mode(), *mode);
    }
    eprintln!(
        "CRASH stage={stage} shared={shared} before_refs={} partials={} orphans={} readonly=unchanged",
        before.len(),
        report.partials.len(),
        report.orphans.len()
    );
    // Back up the actual preserved state BEFORE an operator decides to retry.
    fixture.portable(&mut db);
    let source = if shared { "original.pdf" } else { "new.pdf" };
    if committed {
        let result = &before[1];
        let expected_request = request(&fixture.root, &fixture.old, source);
        assert_ne!(result.id, before[0].id);
        assert_eq!(result.paper_id, fixture.old.id);
        assert_eq!(result.sha256, expected_request.expected_sha256.unwrap());
        assert_eq!(result.storage_key.as_str(), files::key(&result.sha256));
        assert_eq!(
            result.byte_size,
            fs::metadata(expected_request.source).unwrap().len()
        );
        assert_eq!(result.original_name, source);
        assert_eq!(result.media_type, "application/pdf");
        // Retain the committed result; deliberately do NOT call import again.
        assert_eq!(db.attachments().for_paper(fixture.old.id).unwrap(), before);
        eprintln!("CRASH retry=skipped inspected_committed_result=true duplicate_refs=0");
    } else {
        let blob_path = fixture
            .directory
            .path()
            .join(files::key(&digest(&fixture.root.join(source))));
        use std::os::unix::fs::MetadataExt;
        let published_identity = fs::metadata(&blob_path).ok().map(|m| (m.dev(), m.ino()));
        let added = db
            .managed_attachments(&fixture.directory)
            .unwrap()
            .import(request(&fixture.root, &fixture.old, source))
            .unwrap();
        let after = db.attachments().for_paper(fixture.old.id).unwrap();
        assert_eq!(after, vec![before[0].clone(), added.clone()]);
        if let Some(identity) = published_identity {
            let m = fs::metadata(&blob_path).unwrap();
            assert_eq!((m.dev(), m.ino()), identity);
        }
        assert_eq!(added.sha256, digest(&fixture.root.join(source)));
        eprintln!(
            "CRASH retry=explicit previously_committed=false matching_blob_reused={}",
            published_identity.is_some()
        );
    }
    fixture.verify_existing(&mut db, &fixture.old);
    let after = db.check_integrity(&fixture.directory).unwrap();
    assert!(after.orphans.is_empty() && after.dangling.is_empty() && after.corrupt.is_empty());
    assert_eq!(after.partials, report.partials);
    drop(db);
    let db = LibraryDb::open(&fixture.directory).unwrap();
    assert_eq!(db.attachments().for_paper(fixture.old.id).unwrap().len(), 2);
}

#[test]
fn harness_failures_reap_owned_children_without_reader_threads() {
    for stage in [
        "early-exit",
        "no-ready",
        "silent-ready",
        "wrong-nonce",
        "wrong-stage",
    ] {
        let fixture = Fixture::new();
        let (listener, mut child) = fixture.spawn(stage, false);
        let receipt = child.receipt.clone();
        let start = Instant::now();
        let timeout = if stage == "no-ready" {
            Duration::from_millis(300)
        } else {
            WAIT
        };
        let error = ready(
            &listener,
            &mut child,
            &format!("{}:{stage}\n", fixture.nonce),
            timeout,
        )
        .unwrap_err();
        match stage {
            "early-exit" => assert!(error.to_string().contains("child exited before ready")),
            "no-ready" => assert_eq!(error.kind(), io::ErrorKind::TimedOut),
            "silent-ready" => assert!(error.to_string().contains("ready read")),
            _ => assert_eq!(error.to_string(), "nonce/stage mismatch"),
        }
        drop(child); // exercise the actual failure-path RAII cleanup
        let status = receipt
            .lock()
            .unwrap()
            .expect("failure cleanup must reap owned child");
        assert!(start.elapsed() < WAIT * 2 + Duration::from_secs(1));
        if stage == "early-exit" {
            assert!(status.success());
        } else {
            assert_eq!(status.signal(), Some(9));
        }
        eprintln!("HARNESS stage={stage} status={status} reaped=true reader_threads=0");
    }
}
