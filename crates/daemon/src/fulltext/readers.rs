//! #18 search read connections: a small pool of dedicated, interruptible SQLite readers.
//! Concurrent searches each get their own connection (WAL readers do not block each other
//! or the writer), so one slow search cannot hold up the others; each request's deadline is
//! enforced inside SQLite by its reader (`cm_storage::fts::SearchReader`).
use cm_protocol::ipc::WireError;
use cm_storage::{LibraryDirectory, StorageError, fts::SearchReader};
use std::{
    ops::Deref,
    sync::{Condvar, Mutex},
    time::Instant,
};

/// Concurrent search/full-text read requests. More wait for a free reader until their
/// own deadline and then get `WireError::Busy`.
pub const MAX_SEARCH_READERS: usize = 4;

struct State {
    idle: Vec<SearchReader>,
    open: usize,
}

pub struct ReaderPool {
    directory: LibraryDirectory,
    max: usize,
    state: Mutex<State>,
    freed: Condvar,
}
impl ReaderPool {
    /// Opens the first reader now, so a library that cannot be read fails daemon start.
    pub fn new(directory: LibraryDirectory, max: usize) -> Result<Self, StorageError> {
        let first = SearchReader::open(&directory)?;
        Ok(Self {
            directory,
            max: max.max(1),
            state: Mutex::new(State {
                idle: vec![first],
                open: 1,
            }),
            freed: Condvar::new(),
        })
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// A free reader, opening another one up to the pool size; waits until `deadline`.
    pub fn acquire(&self, deadline: Instant) -> Result<Lease<'_>, WireError> {
        let mut state = self.lock();
        loop {
            if let Some(reader) = state.idle.pop() {
                return Ok(Lease {
                    pool: self,
                    reader: Some(reader),
                });
            }
            if state.open < self.max {
                state.open += 1;
                drop(state);
                return match SearchReader::open(&self.directory) {
                    Ok(reader) => Ok(Lease {
                        pool: self,
                        reader: Some(reader),
                    }),
                    Err(_) => {
                        self.lock().open -= 1;
                        self.freed.notify_one();
                        Err(WireError::Unavailable)
                    }
                };
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(WireError::Busy);
            }
            state = self
                .freed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// A reader checked out of the pool; returned on drop.
pub struct Lease<'a> {
    pool: &'a ReaderPool,
    reader: Option<SearchReader>,
}
impl Deref for Lease<'_> {
    type Target = SearchReader;
    fn deref(&self) -> &SearchReader {
        self.reader.as_ref().expect("reader is present until drop")
    }
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(reader) = self.reader.take() {
            self.pool.lock().idle.push(reader);
            self.pool.freed.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_storage::LibraryDb;
    use std::time::Duration;

    /// P2-2 regression: searches do not share one connection. While one request holds a
    /// reader (e.g. a slow search), another request gets its own at once; only when the
    /// pool is exhausted does a request wait, bounded by its own deadline (`Busy`).
    #[test]
    fn concurrent_requests_get_separate_readers_and_waits_are_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let directory =
            LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("library")).unwrap();
        drop(LibraryDb::open(&directory).unwrap());
        let pool = ReaderPool::new(directory, 2).unwrap();
        let soon = || Instant::now() + Duration::from_millis(200);
        let slow = pool.acquire(soon()).unwrap();
        let started = Instant::now();
        let other = pool.acquire(soon()).unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(!std::ptr::eq(&*slow, &*other));
        let started = Instant::now();
        assert_eq!(pool.acquire(soon()).err(), Some(WireError::Busy));
        assert!(started.elapsed() >= Duration::from_millis(150));
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                pool.acquire(Instant::now() + Duration::from_secs(5))
                    .map(|_| ())
            });
            std::thread::sleep(Duration::from_millis(50));
            drop(slow);
            assert_eq!(waiter.join().unwrap(), Ok(()));
        });
        drop(other);
        assert_eq!(
            pool.lock().idle.len(),
            2,
            "readers are reused, not reopened"
        );
    }
}
