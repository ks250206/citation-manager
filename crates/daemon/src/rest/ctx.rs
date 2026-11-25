//! Per-request context: authentication and the endpoint's scope check happen in the `Ctx`
//! extractor, which every handler takes as its first argument, so no path, query or body
//! is parsed for an unauthenticated or under-scoped request. Library work then runs on a
//! blocking thread through an `InternalSession` (the IPC dispatch path).
use super::{BASE_PATH, LogSink, RestConfig, error::ApiError};
use crate::ipc::gateway::{Canceller, Gateway, InternalSession};
use axum::{
    body::Body,
    extract::{FromRequestParts, MatchedPath},
    http::{HeaderMap, Method, StatusCode, Uri, header, request::Parts},
};
use cm_core::policy::{
    ClientKind, GrantId, Operation,
    scopes::{Access, Surface, endpoint},
};
use cm_protocol::ipc::{Command, Credential, Denial, Reply, WireError};
use serde::de::DeserializeOwned;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct App {
    pub gateway: Gateway,
    pub config: Arc<RestConfig>,
    /// The bound address (the Host check accepts only it and `localhost` with its port).
    pub addr: std::net::SocketAddr,
    pub in_flight: Arc<tokio::sync::Semaphore>,
    pub log: LogSink,
}
impl App {
    pub fn new(gateway: Gateway, config: &RestConfig, addr: std::net::SocketAddr) -> Self {
        Self {
            gateway,
            in_flight: Arc::new(tokio::sync::Semaphore::new(config.limits.max_in_flight)),
            log: config.sink(),
            config: Arc::new(config.clone()),
            addr,
        }
    }
}

/// Server-generated request id (never taken from the client), echoed as `X-Request-Id`.
#[derive(Debug, Clone)]
pub struct RequestId(pub String);
impl RequestId {
    pub fn new() -> Self {
        Self(format!("req_{}", uuid::Uuid::now_v7().simple()))
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Deadline(pub Instant);

/// An authenticated request whose token holds every scope of its `ENDPOINTS` row.
pub struct Ctx {
    pub app: App,
    grant: Option<GrantId>,
    deadline: Instant,
}

/// Token kinds accepted over HTTP: scoped API/MCP/CLI tokens. The owner's desktop
/// credential (all capabilities, grant administration, arbitrary local paths) and native
/// host grants are refused here even though IPC accepts them.
fn rest_kind(kind: Option<ClientKind>) -> bool {
    matches!(
        kind,
        Some(ClientKind::Api | ClientKind::Mcp | ClientKind::Cli)
    )
}

pub fn bearer(headers: &HeaderMap) -> Result<Credential, ApiError> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return Err(ApiError::unauthenticated());
    };
    let value = value.to_str().map_err(|_| ApiError::unauthenticated())?;
    let (scheme, token) = value
        .split_once(' ')
        .ok_or_else(ApiError::unauthenticated)?;
    let token = token.trim_start_matches(' ');
    if !scheme.eq_ignore_ascii_case("bearer")
        || token.is_empty()
        || token.len() > 512
        || !token.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(ApiError::unauthenticated());
    }
    Ok(Credential(token.to_owned()))
}

impl FromRequestParts<App> for Ctx {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, app: &App) -> Result<Self, ApiError> {
        let path = parts
            .extensions
            .get::<MatchedPath>()
            .and_then(|m| m.as_str().strip_prefix(BASE_PATH))
            .ok_or_else(ApiError::not_found)?;
        let method = if parts.method == Method::HEAD {
            Method::GET
        } else {
            parts.method.clone()
        };
        let endpoint =
            endpoint(Surface::Rest, &format!("{method} {path}")).ok_or_else(ApiError::internal)?;
        let deadline = parts
            .extensions
            .get::<Deadline>()
            .map_or_else(|| Instant::now() + Duration::from_secs(30), |d| d.0);
        let grant = match endpoint.access {
            Access::Public => None,
            Access::OwnerOnly => return Err(ApiError::forbidden(Operation::Status)),
            Access::Scoped(required) => {
                let credential = bearer(&parts.headers)?;
                let grant = app
                    .gateway
                    .authenticate(&credential)
                    .map_err(|_| ApiError::unauthenticated())?;
                if !rest_kind(app.gateway.kind_of(grant.id)) {
                    return Err(ApiError::new(
                        StatusCode::FORBIDDEN,
                        "token_not_allowed",
                        "this credential cannot be used with the REST API; issue an API token",
                    ));
                }
                app.gateway
                    .preauthorize(grant.id, endpoint.operations)
                    .map_err(|e| {
                        // Before anything runs, a denial is a scope problem. For bulk rows a
                        // token holding the base scope lacks only the time-limited elevation.
                        let error = match e {
                            WireError::Denied(Denial::Forbidden(op)) => {
                                let base = match op {
                                    Operation::BulkTrash => Some(Operation::PaperTrash),
                                    Operation::BulkMove => Some(Operation::OrganizationWrite),
                                    _ => None,
                                };
                                match base {
                                    Some(base) if app.gateway.may(grant.id, base) => {
                                        ApiError::forbidden(op)
                                    }
                                    _ => ApiError::scope_forbidden(),
                                }
                            }
                            other => ApiError::from_wire(other),
                        };
                        if error.status == StatusCode::FORBIDDEN {
                            let scopes: Vec<&str> = required.iter().map(|s| s.name()).collect();
                            error.with_details(serde_json::json!({ "required_scopes": scopes }))
                        } else {
                            error
                        }
                    })?;
                Some(grant.id)
            }
        };
        Ok(Self {
            app: app.clone(),
            grant,
            deadline,
        })
    }
}

/// Cancels the request's outstanding daemon work if the handler future is dropped
/// (client disconnect or timeout) before the blocking call returned.
struct CancelOnDrop(Option<(Canceller, Arc<AtomicBool>)>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some((c, flag)) = self.0.take() {
            flag.store(true, Ordering::SeqCst);
            c.cancel_all();
        }
    }
}

pub struct Calls {
    session: InternalSession,
    deadline: Instant,
    /// Set when the client went away or the request timed out: no further command runs
    /// (stops bulk loops between items).
    cancelled: Arc<AtomicBool>,
}
impl Calls {
    pub fn call(&mut self, command: Command) -> Result<Reply, ApiError> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "cancelled",
                "the request was cancelled",
            ));
        }
        self.session
            .call(command, self.deadline)
            .map_err(ApiError::from_call)
    }
}

impl Ctx {
    pub fn grant(&self) -> GrantId {
        self.grant.expect("scoped endpoint")
    }
    /// Whether the token may also perform `operation` (decides what a write echoes back).
    pub fn may(&self, operation: Operation) -> bool {
        self.app.gateway.may(self.grant(), operation)
    }
    pub fn limits(&self) -> &super::Limits {
        &self.app.config.limits
    }
    /// Runs daemon commands on a blocking thread, in one internal session.
    pub async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Calls) -> Result<T, ApiError> + Send + 'static,
    ) -> Result<T, ApiError> {
        let session = self.app.gateway.session(self.grant());
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut guard = CancelOnDrop(Some((session.canceller(), cancelled.clone())));
        let mut calls = Calls {
            session,
            deadline: self.deadline,
            cancelled,
        };
        let result = tokio::task::spawn_blocking(move || work(&mut calls)).await;
        guard.0 = None;
        result.map_err(|_| ApiError::internal())?
    }
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|essence| essence.trim().eq_ignore_ascii_case("application/json"))
}

/// Reads and parses a JSON body after authentication: `application/json` only, at most
/// `limit` bytes, unknown fields rejected by the target types.
pub async fn json_body<T: DeserializeOwned>(
    headers: &HeaderMap,
    body: Body,
    limit: usize,
) -> Result<T, ApiError> {
    if !is_json(headers) {
        return Err(ApiError::for_status(StatusCode::UNSUPPORTED_MEDIA_TYPE));
    }
    let too_large = || ApiError::for_status(StatusCode::PAYLOAD_TOO_LARGE);
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > limit as u64)
    {
        return Err(too_large());
    }
    let bytes = axum::body::to_bytes(body, limit)
        .await
        .map_err(|_| too_large())?;
    serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::invalid_json(format!("invalid JSON body: {e}")))
}

pub fn query<T: DeserializeOwned>(uri: &Uri) -> Result<T, ApiError> {
    axum::extract::Query::<T>::try_from_uri(uri)
        .map(|q| q.0)
        .map_err(|e| ApiError::invalid(format!("invalid query: {}", e.body_text())))
}

pub fn uuid(text: &str, what: &str) -> Result<uuid::Uuid, ApiError> {
    uuid::Uuid::parse_str(text)
        .ok()
        .filter(|u| !u.is_nil())
        .ok_or_else(|| ApiError::invalid(format!("{what} must be a UUID")))
}

/// Comma-separated UUID list (at most `max`).
pub fn uuid_list(text: &str, what: &str, max: usize) -> Result<Vec<uuid::Uuid>, ApiError> {
    let ids = text
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| uuid(s.trim(), what))
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() > max {
        return Err(ApiError::invalid(format!("at most {max} {what} values")));
    }
    Ok(ids)
}

/// Opaque cursors: URL-safe base64 of the daemon's own cursor text.
pub fn encode_cursor(raw: &str) -> String {
    cm_protocol::ipc::b64::encode(raw.as_bytes())
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}
pub fn decode_cursor(text: &str) -> Result<String, ApiError> {
    let invalid = || ApiError::invalid("invalid cursor");
    if text.len() > 12 * 1024 {
        return Err(invalid());
    }
    let mut standard = text.replace('-', "+").replace('_', "/");
    while !standard.len().is_multiple_of(4) {
        standard.push('=');
    }
    let bytes = cm_protocol::ipc::b64::decode(&standard).ok_or_else(invalid)?;
    String::from_utf8(bytes).map_err(|_| invalid())
}

/// `null` and absent differ: `Some(None)` clears, `None` leaves unchanged.
pub fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    <Option<T> as serde::Deserialize>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_url_safely() {
        for raw in ["", "a", "{\"version\":1,\"id\":\"x\"}", "??>>~~"] {
            let encoded = encode_cursor(raw);
            assert!(!encoded.contains(['+', '/', '=']), "{encoded}");
            assert_eq!(decode_cursor(&encoded).unwrap(), raw);
        }
        assert!(decode_cursor("!!!").is_err());
    }

    #[test]
    fn bearer_parsing_is_strict() {
        let mut headers = HeaderMap::new();
        assert!(bearer(&headers).is_err());
        headers.insert(header::AUTHORIZATION, "Bearer abc".parse().unwrap());
        assert_eq!(bearer(&headers).unwrap().0, "abc");
        headers.insert(header::AUTHORIZATION, "bearer abc".parse().unwrap());
        assert!(bearer(&headers).is_ok());
        for bad in ["Basic abc", "Bearer", "Bearer ", "Bearer a b", "Token abc"] {
            headers.insert(header::AUTHORIZATION, bad.parse().unwrap());
            assert!(bearer(&headers).is_err(), "{bad}");
        }
        headers.insert(header::AUTHORIZATION, "Bearer abc".parse().unwrap());
        headers.append(header::AUTHORIZATION, "Bearer def".parse().unwrap());
        assert!(bearer(&headers).is_err(), "two Authorization headers");
    }
}
