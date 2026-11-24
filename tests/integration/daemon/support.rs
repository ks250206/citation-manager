//! Shared helpers for #25 daemon integration tests. Every test uses its own temp library
//! and private runtime directory; no default user library is touched.
#![allow(dead_code)]
use cm_daemon::{
    ipc::DaemonConfig,
    jobs::JobPolicy,
    lifecycle::{DaemonPaths, LocalConnector, euid, local_connector},
};
use cm_protocol::{
    client::{
        Session,
        local::{CredentialSource, Launch},
    },
    ipc::{CapabilitySet, Command, Credential, GrantCommand, GrantReply, Reply, WireError},
};
use std::{
    path::PathBuf,
    process::{Child, Command as Process},
    time::{Duration, Instant},
};

pub struct Env {
    pub temp: tempfile::TempDir,
    pub library: PathBuf,
    pub runtime: PathBuf,
}
impl Env {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        Self {
            library: root.join("library"),
            runtime: root.join("run"),
            temp,
        }
    }
    pub fn paths(&self) -> DaemonPaths {
        DaemonPaths::new(&self.library, Some(&self.runtime), euid()).unwrap()
    }
    pub fn config(&self) -> DaemonConfig {
        let mut config = DaemonConfig::new(self.library.clone());
        config.runtime_dir = Some(self.runtime.clone());
        config.idle_timeout = Duration::from_secs(60);
        config.jobs = fast_policy();
        config
    }
    pub fn connector(&self, kind: &str) -> LocalConnector {
        local_connector(self.paths(), kind)
    }
    pub fn connector_with(&self, kind: &str, credential: Credential) -> LocalConnector {
        let mut c = self.connector(kind);
        c.credential = CredentialSource::Explicit(credential);
        c
    }
    pub fn desktop(&self) -> Session {
        Session::open(&self.connector("desktop")).unwrap()
    }
    pub fn session(&self, kind: &str, credential: Credential) -> Session {
        Session::open(&self.connector_with(kind, credential)).unwrap()
    }
    pub fn launching_connector(&self, idle_ms: u64) -> LocalConnector {
        let mut c = self.connector("cli");
        c.launch = Some(Launch {
            papersd: PathBuf::from(env!("CARGO_BIN_EXE_papersd")),
            extra_args: vec!["--idle-timeout-ms".into(), idle_ms.to_string().into()],
            timeout: Duration::from_secs(20),
        });
        c
    }
    pub fn spawn_papersd(&self, idle_ms: u64) -> Child {
        Process::new(env!("CARGO_BIN_EXE_papersd"))
            .arg("--library")
            .arg(&self.library)
            .arg("--runtime-dir")
            .arg(&self.runtime)
            .arg("--idle-timeout-ms")
            .arg(idle_ms.to_string())
            .spawn()
            .unwrap()
    }
}

pub fn fast_policy() -> JobPolicy {
    JobPolicy {
        lease: Duration::from_millis(600),
        base_backoff: Duration::from_millis(100),
        max_backoff: Duration::from_millis(400),
        default_max_attempts: 3,
        poll: Duration::from_millis(50),
    }
}

pub fn ok(result: Result<Result<Reply, WireError>, cm_protocol::client::ClientError>) -> Reply {
    result.unwrap().unwrap()
}

pub fn issue(admin: &mut Session, label: &str, capabilities: CapabilitySet) -> Credential {
    match ok(admin.call(Command::Grants(GrantCommand::Issue {
        label: label.into(),
        capabilities,
        expires_at_ms: None,
    }))) {
        Reply::Grants(GrantReply::Issued { credential, .. }) => credential,
        other => panic!("unexpected {other:?}"),
    }
}

pub fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    condition()
}
