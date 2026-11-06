//! Bounded, read-only singleton HTTP lookup. Construct and call on a dedicated worker,
//! outside any async runtime/UI event thread. No configurable production endpoints.
use cm_core::{ServiceError, metadata_resolve::*, papers::normalize_doi};
use reqwest::{Client, Url, header::HeaderMap};
use std::{
    collections::HashMap,
    future::Future,
    time::{Duration, Instant, SystemTime},
};

#[derive(Debug, Clone)]
pub struct HttpConfig {
    pub user_agent: String,
    pub timeout: Duration,
    pub attempts: u8,
    pub cache_entries: usize,
    pub positive_ttl: Duration,
    pub negative_ttl: Duration,
    pub body_bytes: usize,
}
impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            user_agent: "citation-manager/0.1 (+https://github.com/ks250206/citation-manager)"
                .into(),
            timeout: Duration::from_secs(3),
            attempts: 2,
            cache_entries: 64,
            positive_ttl: Duration::from_secs(600),
            negative_ttl: Duration::from_secs(30),
            body_bytes: 256 * 1024,
        }
    }
}
/// Injectable monotonic clock/wait for deterministic retry/cache tests. Production waits
/// poll local cancellation every 10ms. HTTP deadlines always use Tokio's real timer.
pub trait WorkerClock {
    fn now(&self) -> Duration;
    fn unix_time(&self) -> SystemTime;
    fn wait(&self, duration: Duration, cancel: &Cancellation) -> bool;
}
pub struct SystemClock {
    start: Instant,
}
impl Default for SystemClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}
impl WorkerClock for SystemClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
    fn unix_time(&self) -> SystemTime {
        SystemTime::now()
    }
    fn wait(&self, duration: Duration, cancel: &Cancellation) -> bool {
        let start = Instant::now();
        while start.elapsed() < duration {
            if cancel.is_cancelled() {
                return false;
            }
            std::thread::sleep(
                duration
                    .saturating_sub(start.elapsed())
                    .min(Duration::from_millis(10)),
            );
        }
        !cancel.is_cancelled()
    }
}
struct Cached {
    reply: ProviderReply,
    expires: Duration,
    sequence: u64,
}
/// Owns a small current-thread runtime, one client, per-provider pacing and bounded cache.
/// Product API accepts only DOI + Provider; test-only endpoints cannot ship in a build.
/// Construct, call and drop on a synchronous worker outside any async runtime.
pub struct HttpProviders<C = SystemClock> {
    config: HttpConfig,
    clock: C,
    runtime: tokio::runtime::Runtime,
    client: Client,
    cache: HashMap<(Provider, String), Cached>,
    next: HashMap<Provider, Duration>,
    sequence: u64,
    #[cfg(test)]
    endpoints: Option<[String; 2]>,
}
impl HttpProviders<SystemClock> {
    pub fn new(config: HttpConfig) -> Result<Self, ServiceError> {
        Self::with_clock(config, SystemClock::default())
    }
}
impl<C: WorkerClock> HttpProviders<C> {
    pub fn with_clock(config: HttpConfig, clock: C) -> Result<Self, ServiceError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(ServiceError::InvalidInput(
                "construct HTTP providers on a synchronous worker outside an async runtime".into(),
            ));
        }
        if config.user_agent.len() > 512
            || config.user_agent.len() < 10
            || !config.user_agent.contains('/')
            || !config.user_agent.contains("https://")
            || config.user_agent.chars().any(char::is_control)
            || config.timeout.is_zero()
            || config.timeout > Duration::from_secs(10)
            || !(1..=3).contains(&config.attempts)
            || !(1..=64).contains(&config.cache_entries)
            || config.positive_ttl > Duration::from_secs(600)
            || config.negative_ttl > Duration::from_secs(30)
            || !(1..=256 * 1024).contains(&config.body_bytes)
        {
            return Err(ServiceError::InvalidInput(
                "invalid bounded HTTP configuration or identifying public application User-Agent"
                    .into(),
            ));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| ServiceError::Unavailable)?;
        let client = Client::builder()
            .user_agent(&config.user_agent)
            .timeout(config.timeout)
            .connect_timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
            .map_err(|_| ServiceError::InvalidInput("invalid HTTP configuration".into()))?;
        Ok(Self {
            config,
            clock,
            runtime,
            client,
            cache: HashMap::new(),
            next: HashMap::new(),
            sequence: 0,
            #[cfg(test)]
            endpoints: None,
        })
    }
    fn endpoint(&self, provider: Provider, doi: &str) -> Url {
        let base = match provider {
            Provider::Crossref => "https://api.crossref.org/works",
            Provider::DataCite => "https://api.datacite.org/dois",
        };
        #[cfg(test)]
        let base = self
            .endpoints
            .as_ref()
            .map(|e| e[usize::from(provider == Provider::DataCite)].as_str())
            .unwrap_or(base);
        let mut url = Url::parse(base).expect("fixed endpoint");
        url.path_segments_mut()
            .expect("fixed hierarchical endpoint")
            .push(doi);
        url
    }
    fn reply(
        provider: Provider,
        status: ProviderStatus,
        attempts: u8,
        retry_after_seconds: Option<u64>,
    ) -> ProviderReply {
        ProviderReply {
            outcome: ProviderOutcome {
                provider,
                status,
                attempts,
                cached: false,
                retry_after_seconds,
            },
            record: None,
        }
    }
    fn remember(&mut self, key: (Provider, String), reply: &ProviderReply) {
        let ttl = match reply.outcome.status {
            ProviderStatus::Found => self.config.positive_ttl,
            ProviderStatus::NotFound => self.config.negative_ttl,
            _ => return,
        };
        if ttl.is_zero() {
            return;
        }
        self.sequence = self.sequence.wrapping_add(1);
        if self.cache.len() >= self.config.cache_entries
            && let Some(key) = self
                .cache
                .iter()
                .min_by_key(|(_, v)| v.sequence)
                .map(|(k, _)| k.clone())
        {
            self.cache.remove(&key);
        }
        self.cache.insert(
            key,
            Cached {
                reply: reply.clone(),
                expires: self.clock.now().saturating_add(ttl),
                sequence: self.sequence,
            },
        );
    }
}
impl<C: WorkerClock> MetadataProvider for HttpProviders<C> {
    fn lookup(&mut self, provider: Provider, doi: &str, cancel: &Cancellation) -> ProviderReply {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Self::reply(provider, ProviderStatus::Unavailable, 0, None);
        }
        if cancel.is_cancelled() {
            return Self::reply(provider, ProviderStatus::Cancelled, 0, None);
        }
        if doi.len() > 4096 || normalize_doi(doi).ok().as_deref() != Some(doi) {
            return Self::reply(provider, ProviderStatus::IdentityMismatch, 0, None);
        }
        self.cache.retain(|_, v| v.expires > self.clock.now());
        let key = (provider, doi.to_owned());
        if let Some(cached) = self.cache.get(&key) {
            let mut reply = cached.reply.clone();
            reply.outcome.cached = true;
            reply.outcome.attempts = 0;
            return reply;
        }
        let url = self.endpoint(provider, doi);
        let mut last = Self::reply(provider, ProviderStatus::Network, 0, None);
        for attempt in 1..=self.config.attempts {
            if cancel.is_cancelled() {
                return Self::reply(provider, ProviderStatus::Cancelled, attempt - 1, None);
            }
            let wait = self
                .next
                .get(&provider)
                .copied()
                .unwrap_or_default()
                .saturating_sub(self.clock.now());
            // Respect long server delays without blocking a worker for unbounded time.
            if wait > Duration::from_secs(2) {
                return Self::reply(
                    provider,
                    ProviderStatus::RateLimited,
                    attempt - 1,
                    Some(ceil_seconds(wait)),
                );
            }
            if !self.clock.wait(wait, cancel) {
                return Self::reply(provider, ProviderStatus::Cancelled, attempt - 1, None);
            }
            let response = self
                .runtime
                .block_on(cancellable(self.request(url.clone()), cancel));
            let (status, headers, body) = match response {
                Ok(v) => v,
                Err(status) => {
                    last = Self::reply(provider, status.clone(), attempt, None);
                    if !matches!(status, ProviderStatus::Network | ProviderStatus::Timeout) {
                        return last;
                    }
                    self.next
                        .insert(provider, self.clock.now().saturating_add(backoff(attempt)));
                    if attempt < self.config.attempts {
                        continue;
                    }
                    return last;
                }
            };
            let pace = pacing(&headers).max(Duration::from_millis(250));
            self.next
                .insert(provider, self.clock.now().saturating_add(pace));
            if status == 429 || status == 503 {
                let retry = retry_after(&headers, self.clock.unix_time())
                    .unwrap_or_else(|| backoff(attempt));
                let delay = retry.max(pace).max(backoff(attempt));
                self.next
                    .insert(provider, self.clock.now().saturating_add(delay));
                last = Self::reply(
                    provider,
                    if status == 429 {
                        ProviderStatus::RateLimited
                    } else {
                        ProviderStatus::Http(status)
                    },
                    attempt,
                    Some(ceil_seconds(delay)),
                );
                if attempt < self.config.attempts && delay <= Duration::from_secs(2) {
                    continue;
                }
                return last;
            }
            if (500..600).contains(&status) {
                last = Self::reply(provider, ProviderStatus::Http(status), attempt, None);
                self.next.insert(
                    provider,
                    self.clock.now().saturating_add(pace.max(backoff(attempt))),
                );
                if attempt < self.config.attempts {
                    continue;
                }
                return last;
            }
            let mut reply = if status == 404 {
                Self::reply(provider, ProviderStatus::NotFound, attempt, None)
            } else if (300..400).contains(&status) {
                Self::reply(provider, ProviderStatus::RedirectRejected, attempt, None)
            } else if status != 200 {
                Self::reply(provider, ProviderStatus::Http(status), attempt, None)
            } else {
                match crate::providers::parse(provider, doi, url.as_str(), &body) {
                    Ok(record) => ProviderReply {
                        outcome: ProviderOutcome {
                            provider,
                            status: ProviderStatus::Found,
                            attempts: attempt,
                            cached: false,
                            retry_after_seconds: None,
                        },
                        record: Some(record),
                    },
                    Err(status) => Self::reply(provider, status, attempt, None),
                }
            };
            if cancel.is_cancelled() {
                reply = Self::reply(provider, ProviderStatus::Cancelled, attempt, None);
            }
            self.remember(key, &reply);
            return reply;
        }
        last
    }
}
impl<C: WorkerClock> HttpProviders<C> {
    async fn request(&self, url: Url) -> Result<(u16, HeaderMap, Vec<u8>), ProviderStatus> {
        let mut response = self
            .client
            .get(url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(network)?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        // Error/redirect bodies are irrelevant and are never consumed.
        if status != 200 {
            return Ok((status, headers, vec![]));
        }
        let media = headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if !(media == "application/json"
            || (media.starts_with("application/") && media.ends_with("+json")))
        {
            return Err(ProviderStatus::InvalidResponse);
        }
        if response
            .content_length()
            .is_some_and(|n| n > self.config.body_bytes as u64)
        {
            return Err(ProviderStatus::TooLarge);
        }
        let mut body = vec![];
        while let Some(chunk) = response.chunk().await.map_err(network)? {
            if body.len().saturating_add(chunk.len()) > self.config.body_bytes {
                return Err(ProviderStatus::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok((status, headers, body))
    }
}
fn network(error: reqwest::Error) -> ProviderStatus {
    if error.is_timeout() {
        ProviderStatus::Timeout
    } else {
        ProviderStatus::Network
    }
}
async fn cancellable<T>(
    future: impl Future<Output = Result<T, ProviderStatus>>,
    cancel: &Cancellation,
) -> Result<T, ProviderStatus> {
    tokio::pin!(future);
    loop {
        tokio::select! { result=&mut future=>return result, _=tokio::time::sleep(Duration::from_millis(10))=>if cancel.is_cancelled() {return Err(ProviderStatus::Cancelled);} }
    }
}
fn backoff(attempt: u8) -> Duration {
    Duration::from_millis(250 * (1u64 << (attempt - 1)))
}
fn ceil_seconds(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() > 0))
}
fn retry_after(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    let value = headers.get("retry-after")?.to_str().ok()?;
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()
                .map(|date| date.duration_since(now).unwrap_or_default())
        })
}
fn pacing(headers: &HeaderMap) -> Duration {
    let limit = headers
        .get("x-rate-limit-limit")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0);
    let interval = headers
        .get("x-rate-limit-interval")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_suffix('s'))
        .and_then(|v| v.parse::<u32>().ok());
    match (limit, interval) {
        (Some(n), Some(s)) => Duration::from_secs(u64::from(s)).div_f64(f64::from(n)),
        _ => Duration::ZERO,
    }
}

#[cfg(test)]
mod tests;
