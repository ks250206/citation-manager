//! #30 longitudinal recovery with the real `papersd` executable, across the IPC interface
//! and the offline storage tools an operator uses (integrity scan, backup, restore):
//!
//! 1. SIGKILL during a stream of acknowledged IPC writes and an attachment job: every
//!    acknowledged write survives, the reopened library passes integrity checks, and an
//!    orphaned blob planted in the store is reported (never deleted) by the scan.
//! 2. A migration-history failure (applied checksum no longer matches the embedded
//!    migration) makes papersd refuse to start without touching the database; the operator
//!    restores the last portable backup into a new library that papersd serves again.
//! 3. A damaged full-text index found by an IPC search makes the daemon schedule one
//!    `search.rebuild` job; afterwards the same search answers identically and the source
//!    records are unchanged.
//!
//! The database is only modified by byte edits of a stopped library (no SQL driver outside
//! storage); `sqlite_file` walks the documented SQLite file format to find the records.
mod support;
use cm_protocol::{
    client::Session,
    domain::PaperId,
    ipc::{Command, Reply, WireError},
    jobs::{JobCommand, JobId, JobReply, JobState, JobStatus, JobSubmit, kinds},
    library::{LibraryReply, LibraryRequest, NewPaper},
    library_query::LibraryQuery,
    search::{SearchReply, SearchRequest},
};
use cm_storage::{
    LibraryDb, LibraryDirectory,
    backup::{restore_library, validate_bundle},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    process::Child,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::*;

fn create(session: &mut Session, title: &str) -> Result<PaperId, String> {
    match session.call(Command::Library(LibraryRequest::Create(Box::new(
        NewPaper::titled(title),
    )))) {
        Ok(Ok(Reply::Library(LibraryReply::Paper(p)))) => Ok(p.id),
        other => Err(format!("{other:?}")),
    }
}
fn titles(session: &mut Session) -> Vec<String> {
    let mut query = LibraryQuery {
        limit: 500,
        ..Default::default()
    };
    let mut out = Vec::new();
    loop {
        match ok(session.call(Command::Query(query.clone()))) {
            Reply::Page(page) => {
                out.extend(page.papers.iter().map(|p| p.title.clone()));
                match page.next {
                    Some(next) => query.after = Some(next),
                    None => break,
                }
            }
            other => panic!("{other:?}"),
        }
    }
    out.sort();
    out
}
fn job(session: &mut Session, id: JobId) -> JobStatus {
    match ok(session.call(Command::Jobs(JobCommand::Get(id)))) {
        Reply::Jobs(JobReply::Job(job)) => *job,
        other => panic!("{other:?}"),
    }
}
fn settle(session: &mut Session, id: JobId) -> JobStatus {
    let mut last = None;
    assert!(wait_until(Duration::from_secs(30), || {
        let status = job(session, id);
        let done = !status.state.is_active();
        last = Some(status);
        done
    }));
    last.unwrap()
}
fn import_pdf(env: &Env, session: &mut Session, paper: PaperId, name: &str) -> JobId {
    let path = env.temp.path().canonicalize().unwrap().join(name);
    std::fs::write(&path, format!("%PDF-1.7\n% synthetic {name}\n%%EOF\n")).unwrap();
    match ok(session.call(Command::Jobs(JobCommand::Submit(JobSubmit {
        kind: kinds::ATTACHMENT_IMPORT.into(),
        idempotency_key: name.into(),
        payload: json!({"paper": paper.0, "path": path, "original_name": name,
            "media_type": "application/pdf"}),
        max_attempts: None,
        deadline_at_ms: None,
    })))) {
        Reply::Jobs(JobReply::Job(job)) => job.id,
        other => panic!("{other:?}"),
    }
}
fn connect(env: &Env) -> Session {
    let mut session = None;
    assert!(wait_until(Duration::from_secs(20), || {
        session = Session::open(&env.connector("desktop")).ok();
        session.is_some()
    }));
    session.unwrap()
}
fn wait_exit(child: &mut Child) -> i32 {
    let mut code = None;
    assert!(wait_until(Duration::from_secs(30), || {
        match child.try_wait().unwrap() {
            Some(status) => {
                code = Some(status.code().unwrap_or(-1));
                true
            }
            None => false,
        }
    }));
    code.unwrap()
}
/// Opens and closes the library once so SQLite checkpoints the WAL into the main file
/// (the last connection's close), making the database file the complete state.
fn checkpoint(library: &Path) -> LibraryDirectory {
    let directory = LibraryDirectory::prepare(library).unwrap();
    drop(LibraryDb::open(&directory).unwrap());
    let wal = library.join("library.sqlite-wal");
    assert!(
        std::fs::metadata(&wal)
            .map(|m| m.len() == 0)
            .unwrap_or(true),
        "WAL still holds frames"
    );
    directory
}
fn digest(path: &Path) -> String {
    format!("{:x}", Sha256::digest(std::fs::read(path).unwrap()))
}

#[test]
fn sigkill_during_ipc_writes_then_integrity_orphans_backup_and_restore() {
    let env = Env::new();
    let mut daemon = env.spawn_papersd(60_000);
    let mut session = connect(&env);
    // A committed attachment before the crash (job + immutable blob).
    let owner = create(&mut session, "Recovery owner").unwrap();
    let attach = import_pdf(&env, &mut session, owner, "before-kill.pdf");
    assert_eq!(settle(&mut session, attach).state, JobState::Succeeded);
    // A writer streams creates; each acknowledged reply is counted before the next call.
    let acked = Arc::new(AtomicUsize::new(0));
    let writer = {
        let acked = acked.clone();
        let connector = env.connector("desktop");
        std::thread::spawn(move || {
            let mut session = Session::open(&connector).unwrap();
            for i in 0..100_000 {
                if create(&mut session, &format!("kill-{i:05}")).is_err() {
                    return;
                }
                acked.store(i + 1, Ordering::SeqCst);
            }
        })
    };
    assert!(wait_until(Duration::from_secs(30), || acked
        .load(Ordering::SeqCst)
        >= 25));
    daemon.kill().unwrap(); // SIGKILL while the writer is mid-stream.
    daemon.wait().unwrap();
    writer.join().unwrap();
    drop(session);
    let acknowledged = acked.load(Ordering::SeqCst);

    // Restart: every acknowledged write is durable; at most the one in-flight create
    // (committed but unacknowledged) is additionally present. Nothing is half-written.
    let mut daemon = env.spawn_papersd(800);
    let mut session = connect(&env);
    let all = titles(&mut session);
    let killed: Vec<&String> = all.iter().filter(|t| t.starts_with("kill-")).collect();
    assert!(
        killed.len() == acknowledged || killed.len() == acknowledged + 1,
        "{} stored vs {acknowledged} acknowledged",
        killed.len()
    );
    for (i, title) in killed.iter().enumerate() {
        assert_eq!(**title, format!("kill-{i:05}"), "contiguous, no gaps");
    }
    assert!(all.contains(&"Recovery owner".to_owned()));
    // The pre-crash job is still succeeded (not re-run) after the restart.
    assert_eq!(job(&mut session, attach).state, JobState::Succeeded);
    drop(session);
    assert_eq!(wait_exit(&mut daemon), 0, "idle exit");

    // Offline operator tools on the stopped library: clean integrity, then a planted
    // orphan blob is reported and left in place.
    let directory = checkpoint(&env.library);
    let db = LibraryDb::open(&directory).unwrap();
    let report = db.check_integrity(&directory).unwrap();
    assert!(report.is_clean(), "{report:?}");
    let orphan_bytes = b"%PDF-1.7\n% orphan never referenced\n%%EOF\n";
    let hash = format!("{:x}", Sha256::digest(orphan_bytes));
    let orphan = env
        .library
        .join("attachments")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(format!("{hash}.pdf"));
    // Shard directories are created private like the store's own (0700).
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
        .create(orphan.parent().unwrap())
        .unwrap();
    std::fs::write(&orphan, orphan_bytes).unwrap();
    std::fs::set_permissions(&orphan, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let report = db.check_integrity(&directory).unwrap();
    assert_eq!(report.orphans, vec![orphan.clone()], "{report:?}");
    assert!(report.dangling.is_empty() && report.corrupt.is_empty());
    assert!(orphan.exists(), "the scan never deletes");

    // Portable backup of the recovered state, validated and restored into a new library.
    let elsewhere = tempfile::tempdir().unwrap();
    let elsewhere_root = elsewhere.path().canonicalize().unwrap().join("private");
    LibraryDirectory::prepare(&elsewhere_root).unwrap();
    let bundle = elsewhere_root.join("bundle");
    let manifest = db.backup_library(&directory, &bundle).unwrap();
    drop(db);
    assert_eq!(manifest.attachments.len(), 1, "only referenced blobs");
    validate_bundle(&LibraryDirectory::prepare(&bundle).unwrap()).unwrap();
    let restored_path = elsewhere_root.join("restored");
    restore_library(&bundle, &restored_path).unwrap();
    assert!(
        !restored_path
            .join("attachments")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(format!("{hash}.pdf"))
            .exists(),
        "orphans are not part of the portable state"
    );
    let restored = Env {
        library: restored_path,
        // Beside, not inside, `private`: macOS caps Unix socket paths at 104 bytes.
        runtime: elsewhere.path().canonicalize().unwrap().join("run"),
        temp: elsewhere,
    };
    let mut daemon = restored.spawn_papersd(800);
    let mut session = connect(&restored);
    assert_eq!(
        titles(&mut session),
        all,
        "restored library serves the same records"
    );
    drop(session);
    assert_eq!(wait_exit(&mut daemon), 0);
    let restored_dir = checkpoint(&restored.library);
    let db = LibraryDb::open(&restored_dir).unwrap();
    let report = db.check_integrity(&restored_dir).unwrap();
    assert!(
        report.dangling.is_empty()
            && report.corrupt.is_empty()
            && report.orphans.is_empty()
            && report.partials.is_empty(),
        "{report:?}"
    );
}

#[test]
fn migration_history_failure_refuses_start_and_backup_restore_recovers() {
    let env = Env::new();
    let mut daemon = env.spawn_papersd(800);
    let mut session = connect(&env);
    create(&mut session, "Survives a refused migration").unwrap();
    let before = titles(&mut session);
    drop(session);
    assert_eq!(wait_exit(&mut daemon), 0);
    let directory = checkpoint(&env.library);
    // The operator's portable backup, taken while the library was healthy.
    let elsewhere = tempfile::tempdir().unwrap();
    let elsewhere_root = elsewhere.path().canonicalize().unwrap().join("private");
    LibraryDirectory::prepare(&elsewhere_root).unwrap();
    let bundle = elsewhere_root.join("bundle");
    let db = LibraryDb::open(&directory).unwrap();
    if let Err(e) = db.backup_library(&directory, &bundle) {
        panic!("backup: {e:?}");
    }
    drop(db);
    // Damage the newest applied migration's recorded checksum (one hex digit), as a failed
    // or foreign migration history would look. The embedded registry is the reference.
    let manifest: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../storage/migrations/manifest.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let newest = manifest.keys().max().unwrap();
    let checksum = manifest[newest].as_str().unwrap().as_bytes().to_vec();
    let db_path = env.library.join("library.sqlite");
    let mut bytes = std::fs::read(&db_path).unwrap();
    let at: Vec<usize> = bytes
        .windows(checksum.len())
        .enumerate()
        .filter(|(_, w)| *w == checksum.as_slice())
        .map(|(i, _)| i)
        .collect();
    assert_eq!(at.len(), 1, "applied checksum recorded once");
    bytes[at[0]] = if bytes[at[0]] == b'0' { b'1' } else { b'0' };
    std::fs::write(&db_path, &bytes).unwrap();
    let tampered = digest(&db_path);
    let backups_before = std::fs::read_dir(env.library.join("backups"))
        .unwrap()
        .count();

    // papersd refuses to open (exit 1), changes nothing and leaves no socket to connect to.
    let mut refused = env.spawn_papersd(800);
    assert_eq!(wait_exit(&mut refused), 1);
    assert!(Session::open(&env.connector("desktop")).is_err());
    assert_eq!(digest(&db_path), tampered, "refused start never writes");
    assert_eq!(
        std::fs::read_dir(env.library.join("backups"))
            .unwrap()
            .count(),
        backups_before
    );
    // Recovery: restore the validated bundle into a new library and serve it.
    validate_bundle(&LibraryDirectory::prepare(&bundle).unwrap()).unwrap();
    let restored = Env {
        library: elsewhere_root.join("restored"),
        // Beside, not inside, `private`: macOS caps Unix socket paths at 104 bytes.
        runtime: elsewhere.path().canonicalize().unwrap().join("run"),
        temp: elsewhere,
    };
    restore_library(&bundle, &restored.library).unwrap();
    let mut daemon = restored.spawn_papersd(800);
    let mut session = connect(&restored);
    assert_eq!(titles(&mut session), before);
    drop(session);
    assert_eq!(wait_exit(&mut daemon), 0);
}

fn search(session: &mut Session, query: &str) -> Result<Vec<(PaperId, String)>, WireError> {
    match session
        .call(Command::Search(Box::new(SearchRequest::new(query))))
        .unwrap()?
    {
        Reply::Search(SearchReply::Results(page)) => Ok(page
            .hits
            .iter()
            .map(|h| (h.paper_id, h.paper_title.clone()))
            .collect()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn damaged_search_index_found_over_ipc_is_rebuilt_by_a_daemon_job() {
    let env = Env::new();
    let mut daemon = env.spawn_papersd(800);
    let mut session = connect(&env);
    for title in [
        "Quokkaline interphase study",
        "Quokkaline anode notes",
        "Unrelated 固体電解質 record",
    ] {
        create(&mut session, title).unwrap();
    }
    let healthy = search(&mut session, "quokkaline").unwrap();
    assert_eq!(healthy.len(), 2);
    let healthy_ja = search(&mut session, "固体電解質").unwrap();
    assert_eq!(healthy_ja.len(), 1);
    let records = titles(&mut session);
    drop(session);
    assert_eq!(wait_exit(&mut daemon), 0);

    // Corrupt the FTS5 structure record (rowid 10) of both indexes in the stopped DB.
    checkpoint(&env.library);
    let db_path = env.library.join("library.sqlite");
    let mut bytes = std::fs::read(&db_path).unwrap();
    for table in ["search_fts_words_data", "search_fts_trigram_data"] {
        let blob = sqlite_file::blob_of_row(&bytes, table, 10).expect(table);
        bytes[blob].fill(0xff);
    }
    std::fs::write(&db_path, &bytes).unwrap();

    let mut daemon = env.spawn_papersd(800);
    let mut session = connect(&env);
    // The damaged index is reported (not an empty result) and one rebuild is scheduled.
    let error = search(&mut session, "quokkaline").unwrap_err();
    let WireError::Service(cm_core::ServiceError::InvalidInput(message)) = &error else {
        panic!("{error:?}")
    };
    assert!(message.contains("rebuild job"), "{message}");
    let rebuilds = |session: &mut Session| -> Vec<JobStatus> {
        match ok(session.call(Command::Jobs(JobCommand::List { active_only: false }))) {
            Reply::Jobs(JobReply::Jobs(jobs)) => jobs
                .into_iter()
                .filter(|j| j.kind == kinds::SEARCH_REBUILD)
                .collect(),
            other => panic!("{other:?}"),
        }
    };
    assert!(wait_until(Duration::from_secs(30), || {
        let jobs = rebuilds(&mut session);
        jobs.len() == 1 && jobs[0].state == JobState::Succeeded
    }));
    // Same answers afterwards; source records untouched.
    assert_eq!(search(&mut session, "quokkaline").unwrap(), healthy);
    assert_eq!(search(&mut session, "固体電解質").unwrap(), healthy_ja);
    assert_eq!(titles(&mut session), records);
    assert_eq!(rebuilds(&mut session).len(), 1, "exactly one rebuild");
    drop(session);
    assert_eq!(wait_exit(&mut daemon), 0);
    let directory = checkpoint(&env.library);
    let db = LibraryDb::open(&directory).unwrap();
    assert!(db.search_index().verify().unwrap().ok());
}

/// Minimal read-only walker of the SQLite file format (https://sqlite.org/fileformat2.html):
/// finds a table's root page in `sqlite_schema` and the byte range of one row's BLOB
/// column (local part) in a table b-tree. Test-only; never used on a live database.
mod sqlite_file {
    use std::ops::Range;

    fn varint(b: &[u8], mut at: usize) -> (u64, usize) {
        let mut value = 0u64;
        for i in 0..9 {
            let byte = b[at];
            at += 1;
            if i == 8 {
                return (value << 8 | u64::from(byte), at);
            }
            value = value << 7 | u64::from(byte & 0x7f);
            if byte & 0x80 == 0 {
                break;
            }
        }
        (value, at)
    }
    fn be(b: &[u8]) -> u64 {
        b.iter().fold(0, |v, x| v << 8 | u64::from(*x))
    }
    struct File<'a> {
        bytes: &'a [u8],
        page: usize,
        usable: usize,
    }
    impl File<'_> {
        /// Calls `visit(rowid, payload offset, local payload length)` for every leaf cell.
        fn walk(&self, root: usize, visit: &mut dyn FnMut(i64, usize, usize)) {
            let start = (root - 1) * self.page;
            let header = if root == 1 { start + 100 } else { start };
            let kind = self.bytes[header];
            let cells = be(&self.bytes[header + 3..header + 5]) as usize;
            let pointers = header + if kind == 0x05 { 12 } else { 8 };
            for i in 0..cells {
                let cell = start + be(&self.bytes[pointers + 2 * i..pointers + 2 * i + 2]) as usize;
                match kind {
                    0x05 => self.walk(be(&self.bytes[cell..cell + 4]) as usize, visit),
                    0x0d => {
                        let (size, at) = varint(self.bytes, cell);
                        let (rowid, at) = varint(self.bytes, at);
                        visit(rowid as i64, at, self.local(size as usize));
                    }
                    other => panic!("unexpected page type {other:#x}"),
                }
            }
            if kind == 0x05 {
                self.walk(be(&self.bytes[header + 8..header + 12]) as usize, visit);
            }
        }
        fn local(&self, size: usize) -> usize {
            let x = self.usable - 35;
            if size <= x {
                return size;
            }
            let m = ((self.usable - 12) * 32 / 255) - 23;
            let k = m + ((size - m) % (self.usable - 4));
            if k <= x { k } else { m }
        }
        /// Serial types and body offset of the record at `at`.
        fn record(&self, at: usize) -> (Vec<u64>, usize) {
            let (header, mut cursor) = varint(self.bytes, at);
            let end = at + header as usize;
            let mut types = Vec::new();
            while cursor < end {
                let (t, next) = varint(self.bytes, cursor);
                types.push(t);
                cursor = next;
            }
            (types, end)
        }
    }
    fn width(serial: u64) -> usize {
        match serial {
            0 | 8 | 9 => 0,
            1..=4 => serial as usize,
            5 => 6,
            6 | 7 => 8,
            n if n >= 12 => ((n - 12) / 2) as usize,
            _ => panic!("reserved serial type"),
        }
    }
    pub fn blob_of_row(bytes: &[u8], table: &str, rowid: i64) -> Option<Range<usize>> {
        let page = match be(&bytes[16..18]) {
            1 => 65536,
            n => n as usize,
        };
        let file = File {
            bytes,
            page,
            usable: page - bytes[20] as usize,
        };
        // sqlite_schema(type, name, tbl_name, rootpage, sql)
        let mut root = None;
        file.walk(1, &mut |_, at, _| {
            let (types, mut body) = file.record(at);
            let mut columns = Vec::new();
            for t in &types[..4] {
                columns.push(&bytes[body..body + width(*t)]);
                body += width(*t);
            }
            if columns[0] == b"table" && columns[1] == table.as_bytes() {
                root = Some(be(columns[3]) as usize);
            }
        });
        let mut found = None;
        file.walk(root?, &mut |id, at, local| {
            if id == rowid {
                // (id INTEGER PRIMARY KEY, block BLOB): id is the rowid alias (NULL here).
                let (types, body) = file.record(at);
                assert_eq!(types.len(), 2);
                let length = width(types[1]).min(at + local - body);
                found = Some(body + width(types[0])..body + width(types[0]) + length);
            }
        });
        found
    }
}
