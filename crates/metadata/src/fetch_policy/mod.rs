//! #22 safe public PDF fetching: explicit fetch policy with SSRF protection.
//!
//! - HTTPS only (plain HTTP only when `allow_http`); no userinfo; default ports only.
//! - Host names that are local by convention are rejected before DNS. Every resolved
//!   address of every hop (including redirects) must be public unicast; the connection is
//!   pinned to exactly the checked addresses (`resolve_to_addrs` + a resolver that refuses
//!   every other name), so DNS rebinding between check and connect is not possible.
//! - Redirects are followed manually (at most `max_redirects`), each hop re-validated.
//! - No cookies (reqwest built without the cookie store), no credentials, no Referer, no
//!   ambient proxy, `Accept-Encoding: identity`.
//! - Status 200 only; size limit checked on Content-Length and while streaming; whole-fetch
//!   deadline; Content-Type must be PDF-compatible and the body must carry `%PDF-` within
//!   its first 1024 bytes. The body is hashed (SHA-256) while it is written to the sink.
//!
//! Construct and call on a synchronous worker outside any async runtime (like `doi`).
use cm_core::metadata_resolve::Cancellation;
use reqwest::{Client, Url, header::HeaderMap};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

#[cfg(any(test, feature = "loopback-test-server"))]
pub mod test_server;
#[cfg(test)]
mod tests;

pub const MAX_PDF_BYTES: u64 = 256 * 1024 * 1024;
const MAGIC_WINDOW: usize = 1024;

#[derive(Debug, Clone)]
pub struct FetchPolicy {
    /// Plain `http://` URLs (and https->http redirects). Default false.
    pub allow_http: bool,
    /// Default 64 MiB, at most 256 MiB.
    pub max_bytes: u64,
    /// Whole fetch including redirects. Default 120 s, at most 600 s.
    pub total_timeout: Duration,
    /// Connect timeout and maximum silence between body chunks. Default 15 s.
    pub io_timeout: Duration,
    /// Default 5, at most 10.
    pub max_redirects: u8,
    pub user_agent: String,
    /// Test-only: permits loopback addresses and any port (never other private ranges).
    loopback: bool,
}
impl Default for FetchPolicy {
    fn default() -> Self {
        Self {
            allow_http: false,
            max_bytes: 64 * 1024 * 1024,
            total_timeout: Duration::from_secs(120),
            io_timeout: Duration::from_secs(15),
            max_redirects: 5,
            user_agent: "citation-manager/0.1 (+https://github.com/ks250206/citation-manager)"
                .into(),
            loopback: false,
        }
    }
}

/// Why a fetch did not produce a PDF. `retryable()` decides automatic job retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// URL/host/address/port/scheme rejected by the policy (SSRF guard).
    Blocked(String),
    TooManyRedirects,
    /// 401/403/407: the PDF needs the user's own access. Never bypassed.
    AuthRequired(u16),
    NotFound(u16),
    /// Wrong Content-Type or missing `%PDF-` magic (often a login or landing page).
    NotPdf(String),
    TooLarge,
    Http(u16),
    Timeout,
    Network,
    Cancelled,
    InvalidConfig(String),
    Sink(String),
}
impl FetchError {
    /// Transient failures worth another automatic attempt.
    pub fn retryable(&self) -> bool {
        match self {
            Self::Timeout | Self::Network | Self::Sink(_) => true,
            Self::Http(code) => *code == 408 || *code == 429 || (500..600).contains(code),
            _ => false,
        }
    }
    /// The page or server requires the user's own access: guide to an explicit import.
    pub fn needs_user_download(&self) -> bool {
        matches!(self, Self::AuthRequired(_) | Self::NotPdf(_))
    }
}
impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocked(r) => write!(f, "blocked by fetch policy: {r}"),
            Self::TooManyRedirects => f.write_str("too many redirects"),
            Self::AuthRequired(c) => {
                write!(f, "HTTP {c}: the PDF requires sign-in or access rights")
            }
            Self::NotFound(c) => write!(f, "HTTP {c}: PDF not found"),
            Self::NotPdf(r) => write!(f, "response is not a PDF: {r}"),
            Self::TooLarge => f.write_str("PDF exceeds the size limit"),
            Self::Http(c) => write!(f, "HTTP {c}"),
            Self::Timeout => f.write_str("timed out"),
            Self::Network => f.write_str("network error"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::InvalidConfig(r) => write!(f, "invalid fetch configuration: {r}"),
            Self::Sink(r) => write!(f, "cannot store download: {r}"),
        }
    }
}
impl std::error::Error for FetchError {}

/// Guidance shown when a PDF needs the user's own browser access.
pub const USER_DOWNLOAD_GUIDANCE: &str = "The PDF is not publicly downloadable. The paper was saved without it; \
download the PDF in your browser with your own access and attach the file explicitly \
(PDF > Attach to the saved paper).";

impl FetchPolicy {
    /// Test-only policy for a local HTTP fixture server: loopback addresses, any port and
    /// plain HTTP are allowed. Private/link-local ranges and local names stay blocked.
    #[cfg(any(test, feature = "loopback-test-server"))]
    pub fn loopback_test_server() -> Self {
        Self {
            allow_http: true,
            loopback: true,
            total_timeout: Duration::from_secs(10),
            io_timeout: Duration::from_secs(3),
            ..Self::default()
        }
    }
    fn validate(&self) -> Result<(), FetchError> {
        if self.max_bytes == 0
            || self.max_bytes > MAX_PDF_BYTES
            || self.total_timeout.is_zero()
            || self.total_timeout > Duration::from_secs(600)
            || self.io_timeout.is_zero()
            || self.io_timeout > self.total_timeout
            || self.max_redirects > 10
            || !(10..=512).contains(&self.user_agent.len())
            || self.user_agent.chars().any(char::is_control)
        {
            return Err(FetchError::InvalidConfig(
                "bounded size/time/redirect limits and an identifying User-Agent are required"
                    .into(),
            ));
        }
        Ok(())
    }
    /// Syntactic check of one URL (every hop). DNS results are checked separately.
    pub fn check_url(&self, raw: &str) -> Result<Url, FetchError> {
        let blocked = |r: &str| Err(FetchError::Blocked(r.into()));
        if raw.len() > 4096 || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return blocked("URL too long or contains whitespace/control characters");
        }
        let url = Url::parse(raw).map_err(|_| FetchError::Blocked("not an absolute URL".into()))?;
        match url.scheme() {
            "https" => {}
            "http" if self.allow_http => {}
            "http" => return blocked("plain HTTP is not allowed; HTTPS only"),
            _ => return blocked("only HTTPS URLs are fetched"),
        }
        if !url.username().is_empty() || url.password().is_some() {
            return blocked("URLs with credentials are not fetched");
        }
        if !self.loopback && url.port().is_some() {
            return blocked("only the default port is allowed");
        }
        match host(&url) {
            Some(Host::Ip(ip)) => self.check_ip(ip)?,
            Some(Host::Name(name)) => check_name(name)?,
            None => return blocked("URL has no host"),
        }
        Ok(url)
    }
    /// Address check applied to IP-literal hosts and every DNS answer.
    pub fn check_ip(&self, ip: IpAddr) -> Result<(), FetchError> {
        if is_public(ip) || (self.loopback && ip.is_loopback()) {
            Ok(())
        } else {
            Err(FetchError::Blocked(format!(
                "address {ip} is not a public unicast address"
            )))
        }
    }
}

enum Host<'a> {
    Ip(IpAddr),
    Name(&'a str),
}
/// The URL parser already canonicalizes IPv4 shorthand (decimal/octal/hex) for http(s).
fn host(url: &Url) -> Option<Host<'_>> {
    let raw = url.host_str()?;
    let bare = raw.trim_start_matches('[').trim_end_matches(']');
    Some(match bare.parse::<IpAddr>() {
        Ok(ip) => Host::Ip(ip),
        Err(_) => Host::Name(raw),
    })
}

fn check_name(name: &str) -> Result<(), FetchError> {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let local = !name.contains('.')
        || [
            "localhost",
            "local",
            "localdomain",
            "internal",
            "intranet",
            "lan",
            "home",
            "corp",
            "home.arpa",
            "invalid",
            "test",
            "onion",
            "arpa",
        ]
        .iter()
        .any(|suffix| name == *suffix || name.ends_with(&format!(".{suffix}")));
    if local {
        return Err(FetchError::Blocked(format!(
            "host {name} is local or reserved"
        )));
    }
    Ok(())
}

/// Public unicast only: rejects unspecified, loopback, private, CGNAT, link-local,
/// documentation/benchmark, multicast/broadcast/reserved, ULA/site-local and v4-embedding
/// IPv6 forms (mapped addresses are judged by their IPv4 address).
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => public_v4(v),
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return public_v4(v4);
            }
            let s = v.segments();
            // Global unicast 2000::/3 only, minus documentation, Teredo/ORCHID/benchmark
            // (2001::/23 IETF protocol assignments) and 6to4 (2002::/16, embeds IPv4).
            s[0] & 0xe000 == 0x2000
                && !(s[0] == 0x2001 && s[1] == 0x0db8)
                && !(s[0] == 0x2001 && s[1] < 0x0200)
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}
fn public_v4(v: Ipv4Addr) -> bool {
    let [a, b, c, _] = v.octets();
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113))
}

/// Name resolution used for every hop. Injectable so tests can map names to fixtures.
pub trait HostResolver: Send + Sync {
    fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<IpAddr>>;
}
/// System resolver (getaddrinfo) on a helper thread, bounded by the policy's I/O timeout.
pub struct SystemResolver;
impl HostResolver for SystemResolver {
    fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<IpAddr>> {
        (host, port)
            .to_socket_addrs()
            .map(|addrs| addrs.map(|a| a.ip()).collect())
    }
}

/// A successfully downloaded and verified PDF (bytes went to the caller's sink).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedPdf {
    pub sha256: [u8; 32],
    pub bytes: u64,
    /// After redirects.
    pub final_url: String,
    pub content_type: Option<String>,
    pub redirects: u8,
}

/// Refuses every name: only the per-hop `resolve_to_addrs` override can be connected.
struct DenyResolver;
impl reqwest::dns::Resolve for DenyResolver {
    fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async {
            Err::<reqwest::dns::Addrs, _>(
                std::io::Error::other("name not pre-validated by fetch policy").into(),
            )
        })
    }
}

pub struct PdfFetcher {
    policy: FetchPolicy,
    resolver: Arc<dyn HostResolver>,
}
impl PdfFetcher {
    pub fn new(policy: FetchPolicy) -> Self {
        Self::with_resolver(policy, Arc::new(SystemResolver))
    }
    pub fn with_resolver(policy: FetchPolicy, resolver: Arc<dyn HostResolver>) -> Self {
        Self { policy, resolver }
    }
    pub fn policy(&self) -> &FetchPolicy {
        &self.policy
    }

    /// Fetches `url` into `sink`. `progress(bytes)` returning false cancels. Nothing is
    /// written to the library here; the caller imports the verified bytes by digest.
    pub fn fetch(
        &self,
        url: &str,
        sink: &mut dyn Write,
        cancel: &Cancellation,
        progress: &mut dyn FnMut(u64) -> bool,
    ) -> Result<FetchedPdf, FetchError> {
        self.policy.validate()?;
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(FetchError::InvalidConfig(
                "fetch on a synchronous worker outside an async runtime".into(),
            ));
        }
        let mut current = self.policy.check_url(url)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| FetchError::Network)?;
        let started = Instant::now();
        let mut redirects = 0u8;
        loop {
            if cancel.is_cancelled() {
                return Err(FetchError::Cancelled);
            }
            let remaining = self
                .policy
                .total_timeout
                .checked_sub(started.elapsed())
                .filter(|d| !d.is_zero())
                .ok_or(FetchError::Timeout)?;
            let client = self.client_for(&current, remaining)?;
            let hop = runtime.block_on(cancellable(
                self.hop(&client, &current, sink, cancel, progress),
                cancel,
                remaining,
            ))?;
            match hop {
                Hop::Done(mut fetched) => {
                    fetched.redirects = redirects;
                    return Ok(fetched);
                }
                Hop::Redirect(location) => {
                    redirects += 1;
                    if redirects > self.policy.max_redirects {
                        return Err(FetchError::TooManyRedirects);
                    }
                    let next = current
                        .join(&location)
                        .map_err(|_| FetchError::Blocked("invalid redirect location".into()))?;
                    current = self.policy.check_url(next.as_str())?;
                }
            }
        }
    }

    /// One client per hop, pinned to the validated addresses of that hop's host.
    fn client_for(&self, url: &Url, remaining: Duration) -> Result<Client, FetchError> {
        let mut builder = Client::builder()
            .user_agent(&self.policy.user_agent)
            .timeout(remaining)
            .connect_timeout(self.policy.io_timeout)
            .read_timeout(self.policy.io_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .retry(reqwest::retry::never())
            .no_proxy()
            .dns_resolver(DenyResolver);
        if let Some(Host::Name(name)) = host(url) {
            let port = url
                .port_or_known_default()
                .ok_or_else(|| FetchError::Blocked("unknown port".into()))?;
            let addrs = self.resolve_checked(name, port)?;
            builder = builder.resolve_to_addrs(name, &addrs);
        }
        builder
            .build()
            .map_err(|_| FetchError::InvalidConfig("HTTP client".into()))
    }

    fn resolve_checked(&self, name: &str, port: u16) -> Result<Vec<SocketAddr>, FetchError> {
        let (tx, rx) = mpsc::channel();
        let resolver = self.resolver.clone();
        let host = name.to_owned();
        std::thread::Builder::new()
            .name("pdf-dns".into())
            .spawn(move || {
                let _ = tx.send(resolver.resolve(&host, port));
            })
            .map_err(|_| FetchError::Network)?;
        let ips = match rx.recv_timeout(self.policy.io_timeout) {
            Ok(Ok(ips)) => ips,
            Ok(Err(_)) => return Err(FetchError::Network),
            Err(_) => return Err(FetchError::Timeout),
        };
        if ips.is_empty() {
            return Err(FetchError::Network);
        }
        // Every answer must be public: one private record could be chosen at connect time.
        for ip in &ips {
            self.policy.check_ip(*ip).map_err(|e| match e {
                FetchError::Blocked(r) => FetchError::Blocked(format!("{name} resolves to {r}")),
                other => other,
            })?;
        }
        Ok(ips
            .into_iter()
            .take(16)
            .map(|ip| SocketAddr::new(ip, port))
            .collect())
    }

    async fn hop(
        &self,
        client: &Client,
        url: &Url,
        sink: &mut dyn Write,
        cancel: &Cancellation,
        progress: &mut dyn FnMut(u64) -> bool,
    ) -> Result<Hop, FetchError> {
        let mut response = client
            .get(url.clone())
            .header("Accept", "application/pdf")
            .header("Accept-Encoding", "identity")
            .send()
            .await
            .map_err(network)?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        match status {
            200 => {}
            301 | 302 | 303 | 307 | 308 => {
                let location = headers
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| FetchError::Blocked("redirect without Location".into()))?;
                return Ok(Hop::Redirect(location.to_owned()));
            }
            401 | 403 | 407 => return Err(FetchError::AuthRequired(status)),
            404 | 410 => return Err(FetchError::NotFound(status)),
            _ => return Err(FetchError::Http(status)),
        }
        let content_type = media_type(&headers);
        if let Some(media) = &content_type
            && !matches!(
                media.as_str(),
                "application/pdf"
                    | "application/x-pdf"
                    | "application/octet-stream"
                    | "binary/octet-stream"
                    | "application/download"
                    | "application/force-download"
            )
        {
            return Err(FetchError::NotPdf(format!("Content-Type {media}")));
        }
        if headers
            .get("content-encoding")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| !v.trim().eq_ignore_ascii_case("identity"))
        {
            return Err(FetchError::NotPdf("encoded response body".into()));
        }
        if response
            .content_length()
            .is_some_and(|n| n > self.policy.max_bytes)
        {
            return Err(FetchError::TooLarge);
        }
        let mut hash = Sha256::new();
        let mut total = 0u64;
        let mut head: Vec<u8> = Vec::with_capacity(MAGIC_WINDOW);
        let mut magic = false;
        while let Some(chunk) = response.chunk().await.map_err(network)? {
            if cancel.is_cancelled() {
                return Err(FetchError::Cancelled);
            }
            total = total.saturating_add(chunk.len() as u64);
            if total > self.policy.max_bytes {
                return Err(FetchError::TooLarge);
            }
            if !magic {
                let take = (MAGIC_WINDOW - head.len()).min(chunk.len());
                head.extend_from_slice(&chunk[..take]);
                magic = head.windows(5).any(|w| w == b"%PDF-");
                if !magic && head.len() >= MAGIC_WINDOW {
                    return Err(FetchError::NotPdf("no %PDF- header".into()));
                }
            }
            hash.update(&chunk);
            sink.write_all(&chunk)
                .map_err(|e| FetchError::Sink(e.to_string()))?;
            if !progress(total) {
                return Err(FetchError::Cancelled);
            }
        }
        if !magic {
            return Err(FetchError::NotPdf("no %PDF- header".into()));
        }
        sink.flush().map_err(|e| FetchError::Sink(e.to_string()))?;
        Ok(Hop::Done(FetchedPdf {
            sha256: hash.finalize().into(),
            bytes: total,
            final_url: url.to_string(),
            content_type,
            redirects: 0,
        }))
    }
}

enum Hop {
    Done(FetchedPdf),
    Redirect(String),
}

fn media_type(headers: &HeaderMap) -> Option<String> {
    headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|v| !v.is_empty())
}
fn network(error: reqwest::Error) -> FetchError {
    if error.is_timeout() {
        FetchError::Timeout
    } else {
        FetchError::Network
    }
}
async fn cancellable<T>(
    future: impl std::future::Future<Output = Result<T, FetchError>>,
    cancel: &Cancellation,
    limit: Duration,
) -> Result<T, FetchError> {
    let deadline = tokio::time::Instant::now() + limit;
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                if cancel.is_cancelled() {
                    return Err(FetchError::Cancelled);
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(FetchError::Timeout);
                }
            }
        }
    }
}
