//! #26 bounded audit log: who (grant id + issued kind), what (operation), when, result.
//!
//! JSON lines in `<library>/daemon/audit.jsonl` (0600, no symlinks), rotated to
//! `audit.jsonl.1` when it exceeds `max_bytes`, so at most two files of that size exist.
//! Records are built from enums and ids only: never request content, query text, paths,
//! labels, titles or secrets. A file (not the library database) because every committed
//! database write invalidates open import previews.
use cm_protocol::api::AuditEntry;
use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Default size of each of the two files.
pub const DEFAULT_MAX_BYTES: u64 = 512 * 1024;
/// Most records a single `tail` returns.
pub const MAX_TAIL: usize = 500;

pub struct AuditLog {
    path: PathBuf,
    max_bytes: u64,
    lock: Mutex<()>,
}
impl AuditLog {
    pub fn new(path: PathBuf, max_bytes: u64) -> Self {
        Self {
            path,
            max_bytes: max_bytes.max(1024),
            lock: Mutex::new(()),
        }
    }
    fn rotated(&self) -> PathBuf {
        self.path.with_extension("jsonl.1")
    }
    /// Best effort: auditing never blocks or fails the audited request.
    pub fn record(&self, entry: &AuditEntry) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self.append(entry);
    }
    fn append(&self, entry: &AuditEntry) -> io::Result<()> {
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.len() + line.len() as u64 > self.max_bytes)
        {
            fs::rename(&self.path, self.rotated())?;
        }
        let mut file = OpenOptions::new()
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
        file.write_all(&line)
    }
    /// Newest first, at most `limit` (capped at `MAX_TAIL`). Unparsable lines are skipped.
    pub fn tail(&self, limit: usize) -> Vec<AuditEntry> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut entries = Vec::new();
        for path in [self.rotated(), self.path.clone()] {
            entries.extend(read_lines(&path));
        }
        entries.reverse();
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
    use cm_core::policy::Operation;
    use cm_protocol::api::AuditOutcome;

    #[test]
    fn rotates_and_stays_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::new(dir.path().join("audit.jsonl"), 2048);
        for i in 0..200 {
            log.record(&AuditEntry {
                at_ms: i,
                grant: None,
                kind: None,
                operation: Operation::PaperTrash,
                outcome: AuditOutcome::Denied,
                reason: Some("forbidden".into()),
                target: None,
            });
        }
        let total: u64 = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(total <= 2 * 2048, "{total}");
        let tail = log.tail(5);
        assert_eq!(tail.len(), 5);
        assert_eq!(tail[0].at_ms, 199);
        assert_eq!(
            fs::metadata(dir.path().join("audit.jsonl")).unwrap().mode() & 0o777,
            0o600
        );
    }
}
