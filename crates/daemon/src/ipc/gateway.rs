//! In-process entry point for transports that live inside papersd (#27 REST).
//!
//! There is no second authorization path: an adapter authenticates the presented token with
//! the same `Authenticator` as the IPC handshake, and every command it runs goes through
//! `Session::dispatch` exactly like an IPC request (live-grant authorization, the #26 bulk
//! and export-folder guards, preview grant binding, redaction and the audit log).
use super::{Session, Shared, record_denial};
use crate::{api::ApiService, ipc::auth::now_ms};
use cm_core::{
    metadata_resolve::Cancellation,
    policy::{CapabilitySet, ClientKind, Denial, Grant, GrantId, Operation},
};
use cm_protocol::ipc::{Command, Credential, Reply, ServerFrame, WireError};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// Cloneable handle to the running daemon's services for in-process adapters.
#[derive(Clone)]
pub struct Gateway(pub(super) Arc<Shared>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    Wire(WireError),
    /// No reply within the adapter's deadline. The request was cancelled where possible;
    /// a write that already reached the library worker may still complete.
    TimedOut,
}
impl From<WireError> for CallError {
    fn from(e: WireError) -> Self {
        Self::Wire(e)
    }
}

impl Gateway {
    /// Token verification (same as the IPC handshake): unknown, revoked and expired
    /// credentials are all `Unauthenticated`.
    pub fn authenticate(&self, credential: &Credential) -> Result<Grant, Denial> {
        self.0.auth.authenticate(credential, now_ms())
    }
    pub fn kind_of(&self, grant: GrantId) -> Option<ClientKind> {
        self.0.auth.kind_of(grant)
    }
    /// Effective capabilities of the grant right now.
    pub fn held(&self, grant: GrantId) -> Result<CapabilitySet, Denial> {
        self.0.auth.held(grant, None, now_ms())
    }
    /// Authorizes every operation of a multi-command endpoint *before* any of them runs
    /// (so a denied second step never leaves a half-done request). Each command is still
    /// authorized again by `Session::dispatch` when it runs. Denials are audited.
    pub fn preauthorize(&self, grant: GrantId, operations: &[Operation]) -> Result<(), WireError> {
        let now = now_ms();
        for operation in operations {
            if let Err(denial) = self.0.auth.authorize(grant, None, *operation, now) {
                let error = WireError::Denied(denial);
                record_denial(&self.0, grant, *operation, &error);
                return Err(error);
            }
        }
        Ok(())
    }
    /// The same decision `Session::dispatch` makes, without running anything (used by
    /// adapters to decide what a response may contain, e.g. whether to echo a record).
    pub fn may(&self, grant: GrantId, operation: Operation) -> bool {
        self.0
            .auth
            .authorize(grant, None, operation, now_ms())
            .is_ok()
    }
    pub fn api(&self) -> &ApiService {
        &self.0.api
    }
    pub fn activity(&self) -> &Arc<crate::lifecycle::Activity> {
        &self.0.activity
    }
    /// A short-lived session for one adapter request (its own retained parse/PDF state).
    pub fn session(&self, grant: GrantId) -> InternalSession {
        let id = self.0.sessions.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = mpsc::channel();
        let cancels: Arc<Mutex<HashMap<u64, Cancellation>>> = Arc::default();
        InternalSession {
            session: Session {
                id,
                grant,
                requested: None,
                engine: self.0.engine.for_session(id),
                shared: self.0.clone(),
                tx,
                in_flight: Arc::new(AtomicUsize::new(0)),
                cancels: cancels.clone(),
            },
            rx,
            next: 0,
            canceller: Canceller(cancels),
        }
    }
}

/// Cancels every outstanding request of an `InternalSession` (e.g. the HTTP client went away
/// or the request deadline passed while the blocking call was still waiting).
#[derive(Clone)]
pub struct Canceller(Arc<Mutex<HashMap<u64, Cancellation>>>);
impl Canceller {
    pub fn cancel_all(&self) {
        for c in self.0.lock().unwrap_or_else(|e| e.into_inner()).values() {
            c.cancel();
        }
    }
}

pub struct InternalSession {
    session: Session,
    rx: mpsc::Receiver<ServerFrame>,
    next: u64,
    canceller: Canceller,
}
impl InternalSession {
    pub fn grant(&self) -> GrantId {
        self.session.grant
    }
    pub fn canceller(&self) -> Canceller {
        self.canceller.clone()
    }
    /// Runs one command through `Session::dispatch` and waits for its reply until
    /// `deadline`. Progress frames are ignored.
    pub fn call(&mut self, command: Command, deadline: Instant) -> Result<Reply, CallError> {
        if Instant::now() >= deadline {
            return Err(CallError::TimedOut);
        }
        self.next += 1;
        let id = self.next;
        self.session.dispatch(id, command);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left.max(Duration::from_millis(1))) {
                Ok(ServerFrame::Response { id: got, result }) if got == id => {
                    return result.map_err(CallError::Wire);
                }
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(_) => {
                    if let Some(c) = self
                        .session
                        .cancels
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&id)
                    {
                        c.cancel();
                    }
                    return Err(CallError::TimedOut);
                }
            }
        }
    }
}
impl Drop for InternalSession {
    fn drop(&mut self) {
        self.canceller.cancel_all();
        self.session.engine.end_session(self.session.id);
    }
}
