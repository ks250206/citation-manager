//! #27 local REST API (DESIGN §13, docs/ADR/rest-api.md, contract: docs/openapi.json).
//!
//! The listener runs inside papersd so it shares the single library writer and services.
//! It is off unless the persisted #26 `ApiSettings` enable it, binds only the configured
//! loopback address, and follows settings changes at runtime (`Manager`). Requests are
//! handled on a small tokio runtime; every library command is dispatched through
//! `ipc::gateway` into the same `Session::dispatch` as IPC, so authorization, the bulk and
//! export guards, preview binding and the audit log are identical for REST, CLI and MCP.
//!
//! Defenses (edge middleware, before any handler): exact `Host` allowlist (127.0.0.1,
//! localhost, [::1] with the bound port; DNS rebinding), browser `Origin`s refused unless
//! configured, no CORS by default, URI/header/body limits, per-request timeouts, connection
//! and concurrency limits, standard JSON errors with a request id. Logs carry only the
//! request id, method, route template, status and duration: never bodies, queries or tokens.
mod ctx;
mod edge;
pub mod error;
mod handlers;
pub mod openapi;

use crate::{api::ApiService, ipc::gateway::Gateway, lifecycle::PinGuard};
use cm_protocol::api::ListenerState;
use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

/// Path prefix of every endpoint (DESIGN §13.1). Breaking changes go to `/api/v2`.
pub const BASE_PATH: &str = "/api/v1";

/// Request/connection limits. Defaults are documented in docs/ADR/rest-api.md.
#[derive(Debug, Clone)]
pub struct Limits {
    /// JSON request bodies (everything except `POST /imports`).
    pub json_body: usize,
    /// `POST /imports` request body (the BibTeX/RIS text is at most `import_text`).
    pub import_body: usize,
    pub import_text: usize,
    /// Path + query.
    pub max_uri: usize,
    pub max_header_bytes: usize,
    pub request_timeout: Duration,
    /// Imports, exports, DOI lookups and bulk operations.
    pub long_timeout: Duration,
    pub header_read_timeout: Duration,
    /// A keep-alive connection is closed after this long regardless of activity.
    pub connection_lifetime: Duration,
    pub max_connections: usize,
    pub max_in_flight: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            json_body: 1024 * 1024,
            import_body: 5 * 1024 * 1024,
            import_text: 2 * 1024 * 1024,
            max_uri: 16 * 1024,
            max_header_bytes: 32 * 1024,
            request_timeout: Duration::from_secs(30),
            long_timeout: Duration::from_secs(120),
            header_read_timeout: Duration::from_secs(10),
            connection_lifetime: Duration::from_secs(600),
            max_connections: 64,
            max_in_flight: 32,
        }
    }
}

pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// A browser origin as sent in the `Origin` header: `scheme://host[:port]`, ASCII, no path,
/// query, fragment or credentials. Used to validate the configured allowlist.
pub fn valid_origin(origin: &str) -> bool {
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    matches!(
        scheme,
        "http" | "https" | "chrome-extension" | "moz-extension" | "safari-web-extension"
    ) && !rest.is_empty()
        && rest.len() <= 253
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
}

#[derive(Clone, Default)]
pub struct RestConfig {
    /// Exact browser origins (`scheme://host[:port]`, see `valid_origin`) allowed to call the
    /// API; they also get CORS headers. Empty (the default) refuses every request carrying an
    /// `Origin` header. papersd: `--api-allow-origin` (repeatable).
    pub allowed_origins: Vec<String>,
    pub limits: Limits,
    /// One line per request (id, method, route template, status, duration) and listener
    /// state changes. `None` writes to stderr.
    pub log: Option<LogSink>,
}
impl RestConfig {
    fn sink(&self) -> LogSink {
        self.log
            .clone()
            .unwrap_or_else(|| Arc::new(|line: &str| eprintln!("papersd rest: {line}")))
    }
}

/// Starts, moves and stops the listener as the persisted settings change.
pub struct Manager {
    stop: Arc<AtomicBool>,
    gateway: Gateway,
    thread: Option<thread::JoinHandle<()>>,
}
impl Manager {
    pub fn start(gateway: Gateway, config: RestConfig) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            let gateway = gateway.clone();
            thread::Builder::new()
                .name("papersd-rest-manager".into())
                .spawn(move || manage(&gateway, &config, &stop))?
        };
        Ok(Self {
            stop,
            gateway,
            thread: Some(thread),
        })
    }
    pub fn stop(mut self) {
        self.shutdown();
    }
    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.gateway.api().notify_listener();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Drop for Manager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn desired(api: &ApiService) -> Option<SocketAddr> {
    let settings = api.settings();
    settings
        .enabled
        .then(|| SocketAddr::new(settings.bind.ip(), settings.port))
}

fn manage(gateway: &Gateway, config: &RestConfig, stop: &AtomicBool) {
    let api = gateway.api();
    let log = config.sink();
    let mut current: Option<Listener> = None;
    let mut failed: Option<SocketAddr> = None;
    let mut seen = api.settings_generation();
    while !stop.load(Ordering::SeqCst) {
        let want = desired(api);
        if current.as_ref().map(|l| l.addr) != want {
            if let Some(listener) = current.take() {
                listener.shutdown();
                api.set_listener_state(ListenerState::Stopped);
                log("listener stopped");
            }
            if let Some(addr) = want {
                match Listener::start(addr, gateway, config) {
                    Ok(listener) => {
                        log(&format!("listening on http://{addr}{BASE_PATH}"));
                        current = Some(listener);
                        failed = None;
                        api.set_listener_state(ListenerState::Listening);
                    }
                    Err(e) => {
                        // Retried every second; logged once per address.
                        if failed != Some(addr) {
                            log(&format!("cannot listen on {addr}: {}", e.kind()));
                            failed = Some(addr);
                        }
                        api.set_listener_state(ListenerState::Stopped);
                    }
                }
            }
        }
        seen = api.wait_settings_change(seen, Duration::from_secs(1));
    }
    if let Some(listener) = current.take() {
        listener.shutdown();
    }
    api.set_listener_state(ListenerState::Stopped);
}

/// A bound listener with its own runtime. Holding it keeps papersd from idle exit: REST
/// clients cannot start the daemon, so an enabled API keeps it running.
struct Listener {
    addr: SocketAddr,
    runtime: tokio::runtime::Runtime,
    shutdown: tokio::sync::watch::Sender<bool>,
    _pin: PinGuard,
}
impl Listener {
    fn start(addr: SocketAddr, gateway: &Gateway, config: &RestConfig) -> io::Result<Self> {
        if !addr.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "only loopback addresses",
            ));
        }
        let std_listener = std::net::TcpListener::bind(addr)?;
        std_listener.set_nonblocking(true)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(config.limits.max_in_flight + 4)
            .thread_name("papersd-rest")
            .enable_io()
            .enable_time()
            .build()?;
        let (shutdown, stopped) = tokio::sync::watch::channel(false);
        let app = handlers::App::new(gateway.clone(), config, addr.port());
        let limits = config.limits.clone();
        let listener = {
            let _guard = runtime.enter();
            tokio::net::TcpListener::from_std(std_listener)?
        };
        runtime.spawn(serve(listener, app, limits, stopped));
        Ok(Self {
            addr,
            runtime,
            shutdown,
            _pin: gateway.activity().pin(),
        })
    }
    fn shutdown(self) {
        let _ = self.shutdown.send(true);
        // Open connections are dropped; blocking work already handed to the library
        // worker finishes there (its reply is discarded).
        self.runtime.shutdown_timeout(Duration::from_secs(3));
    }
}

async fn serve(
    listener: tokio::net::TcpListener,
    app: handlers::App,
    limits: Limits,
    mut stopped: tokio::sync::watch::Receiver<bool>,
) {
    use hyper_util::{
        rt::{TokioIo, TokioTimer},
        service::TowerToHyperService,
    };
    let connections = Arc::new(tokio::sync::Semaphore::new(limits.max_connections));
    let router = handlers::router(app);
    loop {
        let accepted = tokio::select! {
            _ = stopped.changed() => return,
            accepted = listener.accept() => accepted,
        };
        let Ok((stream, peer)) = accepted else {
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        };
        // Bound to loopback, so this holds already; checked again as defense in depth.
        if !peer.ip().is_loopback() {
            continue;
        }
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            continue; // over the connection limit: closed immediately
        };
        let _ = stream.set_nodelay(true);
        let service = TowerToHyperService::new(router.clone());
        let limits = limits.clone();
        tokio::spawn(async move {
            let connection = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(limits.header_read_timeout)
                .max_buf_size(limits.max_header_bytes.max(8192))
                .max_headers(64)
                .keep_alive(true)
                .serve_connection(TokioIo::new(stream), service);
            let _ = tokio::time::timeout(limits.connection_lifetime, connection).await;
            drop(permit);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::valid_origin;

    #[test]
    fn origins_are_bare_scheme_host_port() {
        for ok in [
            "https://app.example",
            "http://localhost:3000",
            "chrome-extension://abcdefghijklmnop",
            "http://[::1]:8080",
        ] {
            assert!(valid_origin(ok), "{ok}");
        }
        for bad in [
            "",
            "null",
            "*",
            "https://",
            "https://app.example/",
            "https://app.example/path",
            "https://user@app.example",
            "ftp://app.example",
            "https://app.example?x",
            "https://app example",
        ] {
            assert!(!valid_origin(bad), "{bad}");
        }
    }
}
