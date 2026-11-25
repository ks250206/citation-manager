//! #26 bounded audit log: who (grant id + issued kind), what (operation), when, result.
//!
//! Two JSON-lines sinks under `<library>/daemon/`, each rotated to `.1` past its size, so
//! at most four files exist:
//! - `audit-admin.jsonl`: grant management (issue/revoke/scope change), elevation, API
//!   settings and bulk decisions. Ordinary denials never go here, so a client hammering a
//!   denied endpoint cannot evict the record of who changed what.
//! - `audit.jsonl`: everything else (writes, deletes, commits, exports, job control and
//!   denials).
//!
//! Identical denials (grant, operation, reason) within `COALESCE_WINDOW` are written once;
//! the following ones are counted and written as one record with `count` when the window
//! ends (or on `flush`). Records are built from enums and ids only: never request content,
//! query text, paths, labels, titles or secrets. Files, not the library database, because
//! every committed database write invalidates open import previews.
//!
//! A sink file that is unsafe (a symlink, extra hard links, group/other permissions) is
//! moved aside to `<name>.unsafe-<ms>` and a fresh file is started; the problem is kept as
//! a warning that `ApiCommand::Overview` reports instead of silently dropping entries.
use cm_core::policy::{GrantId, Operation};
use cm_protocol::api::{AuditEntry, AuditOutcome};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

/// Default size of each general log file.
pub const DEFAULT_MAX_BYTES: u64 = 512 * 1024;
/// Size of each admin log file.
pub const ADMIN_MAX_BYTES: u64 = 256 * 1024;
/// Most records a single `tail` returns.
pub const MAX_TAIL: usize = 500;
pub const COALESCE_WINDOW: Duration = Duration::from_secs(60);
/// Distinct denial keys tracked before all pending counts are flushed.
const MAX_PENDING: usize = 1024;

/// Operations whose records go to the admin sink.
pub fn is_admin(operation: Operation) -> bool {
    matches!(
        operation,
        Operation::GrantIssue
            | Operation::GrantRevoke
            | Operation::GrantUpdate
            | Operation::GrantElevate
            | Operation::ApiSettingsWrite
            | Operation::BulkTrash
            | Operation::BulkMove
    )
}

struct Sink {
    path: PathBuf,
    max_bytes: u64,
}
impl Sink {
    fn rotated(&self) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".1");
        self.path.with_file_name(name)
    }
    fn open(&self) -> io::Result<File> {
        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&self.path)?;
        let m = file.metadata()?;
        if !m.is_file() || m.nlink() != 1 || m.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe audit log",
            ));
        }
        Ok(file)
    }
    /// Appends one line. An unsafe file is moved aside (reported) and a fresh one started.
    fn append(&self, line: &[u8], warning: &Mutex<Option<String>>) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.is_file() && m.len() + line.len() as u64 > self.max_bytes)
        {
            let _ = fs::rename(&self.path, self.rotated());
        }
        let file = match self.open() {
            Ok(file) => Ok(file),
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                let aside = self
                    .path
                    .with_extension(format!("jsonl.unsafe-{}", crate::ipc::auth::now_ms()));
                let moved = fs::rename(&self.path, &aside);
                *warning.lock().unwrap_or_else(|e| e.into_inner()) = Some(match moved {
                    Ok(()) => format!(
                        "audit log {} had unsafe permissions or links; moved aside to {}",
                        name(&self.path),
                        name(&aside)
                    ),
                    Err(_) => format!(
                        "audit log {} is unsafe and could not be moved",
                        name(&self.path)
                    ),
                });
                self.open()
            }
            Err(e) => Err(e),
        };
        if let Err(e) = file.and_then(|mut f| f.write_all(line)) {
            *warning.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!(
                "audit log {} cannot be written: {}",
                name(&self.path),
                e.kind()
            ));
        }
    }
    fn read(&self) -> Vec<AuditEntry> {
        let mut out = read_lines(&self.rotated());
        out.extend(read_lines(&self.path));
        out
    }
}
fn name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

type DenialKey = (Option<GrantId>, Operation, Option<String>);
struct Pending {
    started: Instant,
    suppressed: u32,
    last: AuditEntry,
}

pub struct AuditLog {
    general: Sink,
    admin: Sink,
    pending: Mutex<HashMap<DenialKey, Pending>>,
    warning: Mutex<Option<String>>,
    lock: Mutex<()>,
}
impl AuditLog {
    /// `path` is the general log; the admin log sits next to it (`audit-admin.jsonl`).
    pub fn new(path: PathBuf, max_bytes: u64) -> Self {
        let admin = path.with_file_name("audit-admin.jsonl");
        Self {
            general: Sink {
                path,
                max_bytes: max_bytes.max(1024),
            },
            admin: Sink {
                path: admin,
                max_bytes: ADMIN_MAX_BYTES.min(max_bytes.max(1024)),
            },
            pending: Mutex::new(HashMap::new()),
            warning: Mutex::new(None),
            lock: Mutex::new(()),
        }
    }
    /// The last problem writing the log, if any (reported in the API overview).
    pub fn warning(&self) -> Option<String> {
        self.warning
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Best effort: auditing never blocks or fails the audited request.
    pub fn record(&self, entry: AuditEntry) {
        self.record_at(entry, Instant::now());
    }
    fn record_at(&self, entry: AuditEntry, now: Instant) {
        if entry.outcome == AuditOutcome::Denied {
            let key = (entry.grant, entry.operation, entry.reason.clone());
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(p) = pending.get_mut(&key)
                && now.saturating_duration_since(p.started) < COALESCE_WINDOW
            {
                p.suppressed = p.suppressed.saturating_add(1);
                p.last = entry;
                return;
            }
            if let Some(p) = pending.remove(&key) {
                self.write_summary(p);
            }
            if pending.len() >= MAX_PENDING {
                for (_, p) in pending.drain() {
                    self.write_summary(p);
                }
            }
            pending.insert(
                key,
                Pending {
                    started: now,
                    suppressed: 0,
                    last: entry.clone(),
                },
            );
        }
        self.write(&entry);
    }
    fn write_summary(&self, p: Pending) {
        if p.suppressed > 0 {
            let mut summary = p.last;
            summary.count = p.suppressed;
            self.write(&summary);
        }
    }
    fn write(&self, entry: &AuditEntry) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(mut line) = serde_json::to_vec(entry) else {
            return;
        };
        line.push(b'\n');
        let sink = if is_admin(entry.operation) {
            &self.admin
        } else {
            &self.general
        };
        sink.append(&line, &self.warning);
    }
    /// Writes the counts of coalesced denials (daemon exit, before reading the tail).
    pub fn flush(&self) {
        let drained: Vec<Pending> = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending
                .values_mut()
                .filter(|p| p.suppressed > 0)
                .map(|p| {
                    let out = Pending {
                        started: p.started,
                        suppressed: p.suppressed,
                        last: p.last.clone(),
                    };
                    p.suppressed = 0;
                    out
                })
                .collect()
        };
        for p in drained {
            self.write_summary(p);
        }
    }
    /// Newest first across both sinks, at most `limit` (capped at `MAX_TAIL`).
    pub fn tail(&self, limit: usize) -> Vec<AuditEntry> {
        self.flush();
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut entries = self.general.read();
        entries.extend(self.admin.read());
        entries.sort_by_key(|e| std::cmp::Reverse(e.at_ms));
        entries.truncate(limit.min(MAX_TAIL));
        entries
    }
}
fn read_lines(path: &Path) -> Vec<AuditEntry> {
    let Ok(file) = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
    else {
        return vec![];
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn entry(at_ms: i64, operation: Operation, outcome: AuditOutcome) -> AuditEntry {
        AuditEntry {
            at_ms,
            grant: Some(GrantId(uuid::Uuid::from_u128(7))),
            kind: None,
            operation,
            outcome,
            reason: (outcome == AuditOutcome::Denied).then(|| "forbidden".into()),
            target: None,
            count: 1,
        }
    }
    fn lines(path: &Path) -> usize {
        fs::read_to_string(path).map_or(0, |s| s.lines().count())
    }

    #[test]
    fn denials_are_coalesced_and_cannot_evict_admin_records() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::new(dir.path().join("audit.jsonl"), 2048);
        let t0 = Instant::now();
        log.record_at(entry(1, Operation::GrantIssue, AuditOutcome::Succeeded), t0);
        for i in 0..10_000 {
            log.record_at(
                entry(2 + i, Operation::PaperTrash, AuditOutcome::Denied),
                t0,
            );
        }
        // One record so far for 10k identical denials; the admin record is untouched.
        assert_eq!(lines(&dir.path().join("audit.jsonl")), 1);
        assert_eq!(lines(&dir.path().join("audit-admin.jsonl")), 1);
        // The next one after the window writes the count of the suppressed ones.
        log.record_at(
            entry(20_000, Operation::PaperTrash, AuditOutcome::Denied),
            t0 + COALESCE_WINDOW,
        );
        let tail = log.tail(10);
        assert!(tail.iter().any(|e| e.count == 9_999));
        assert!(tail.iter().any(|e| e.operation == Operation::GrantIssue));
        // Many distinct denials rotate the general log but never the admin one.
        for i in 0..2_000 {
            let mut e = entry(30_000 + i, Operation::PaperTrash, AuditOutcome::Denied);
            e.grant = Some(GrantId(uuid::Uuid::from_u128(1_000 + i as u128)));
            log.record(e);
        }
        let total: u64 = ["audit.jsonl", "audit.jsonl.1"]
            .iter()
            .map(|n| fs::metadata(dir.path().join(n)).map_or(0, |m| m.len()))
            .sum();
        assert!(total <= 2 * 2048, "{total}");
        assert_eq!(lines(&dir.path().join("audit-admin.jsonl")), 1);
        assert_eq!(
            fs::metadata(dir.path().join("audit-admin.jsonl"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn an_unsafe_log_is_moved_aside_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        fs::write(&path, "").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let log = AuditLog::new(path.clone(), 4096);
        log.record(entry(1, Operation::PaperCreate, AuditOutcome::Succeeded));
        assert!(log.warning().unwrap().contains("moved aside"));
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(lines(&path), 1);
        // A symlink in place of the log is moved aside too, never written through.
        let target = dir.path().join("target");
        fs::write(&target, "").unwrap();
        let admin = dir.path().join("audit-admin.jsonl");
        std::os::unix::fs::symlink(&target, &admin).unwrap();
        log.record(entry(2, Operation::GrantRevoke, AuditOutcome::Succeeded));
        assert_eq!(fs::read_to_string(&target).unwrap(), "");
        assert!(
            !fs::symlink_metadata(&admin)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(lines(&admin), 1);
    }
}
