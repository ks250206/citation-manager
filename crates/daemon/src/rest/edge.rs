//! Edge middleware applied to every route and the fallback: request id, Host/Origin checks
//! (DNS rebinding, browser origins), optional CORS for configured origins, URI limit,
//! concurrency limit, per-request timeout, uniform JSON errors and the content-free log line.
use super::{
    ctx::{App, Deadline, RequestId},
    error::ApiError,
};
use axum::{
    body::Body,
    extract::{MatchedPath, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::time::{Duration, Instant};

/// Routes that may run longer than the default request timeout.
fn long_route(route: &str) -> bool {
    route.ends_with("/imports")
        || route.ends_with("/exports")
        || route.ends_with("/resolve-doi")
        || route.ends_with("/bulk-delete")
        || route.ends_with("/bulk-move")
}

fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PATCH => "PATCH",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

/// `Host` must name the bound port on a loopback literal or `localhost` (case-insensitive).
/// Anything else, including a rebinding attacker's own host name, is refused.
pub fn host_allowed(host: &str, port: u16) -> bool {
    let host = host.to_ascii_lowercase();
    [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
    .contains(&host)
}

enum Admission {
    Pass { cors: Option<HeaderValue> },
    Preflight(HeaderValue),
}

fn single(headers: &HeaderMap, name: header::HeaderName) -> Result<Option<&str>, ()> {
    let mut values = headers.get_all(name).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(());
    }
    first.map(|v| v.to_str().map_err(|_| ())).transpose()
}

fn admit(app: &App, request: &Request) -> Result<Admission, ApiError> {
    let limits = &app.config.limits;
    if request
        .uri()
        .path_and_query()
        .is_some_and(|pq| pq.as_str().len() > limits.max_uri)
    {
        return Err(ApiError::for_status(StatusCode::URI_TOO_LONG));
    }
    let headers = request.headers();
    let bad_host = || {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "host_not_allowed",
            "the Host header must be 127.0.0.1, localhost or [::1] with the API port",
        )
    };
    let host = single(headers, header::HOST).map_err(|()| bad_host())?;
    if !host.is_some_and(|h| host_allowed(h, app.port)) {
        return Err(bad_host());
    }
    // An absolute-form request target must name the same authority.
    if let Some(authority) = request.uri().authority()
        && !host_allowed(authority.as_str(), app.port)
    {
        return Err(bad_host());
    }
    let refused = || {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "origin_not_allowed",
            "browser origins are not allowed to call this API",
        )
    };
    let origin = single(headers, header::ORIGIN).map_err(|()| refused())?;
    let cors = match origin {
        None => {
            // A browser that sends no Origin still marks cross-site fetches.
            if single(headers, header::HeaderName::from_static("sec-fetch-site"))
                .ok()
                .flatten()
                .is_some_and(|site| site == "cross-site" || site == "same-site")
            {
                return Err(refused());
            }
            None
        }
        Some(origin) if app.config.allowed_origins.iter().any(|o| o == origin) => {
            Some(HeaderValue::from_str(origin).map_err(|_| refused())?)
        }
        Some(_) => return Err(refused()),
    };
    if let Some(origin) = cors.clone()
        && request.method() == Method::OPTIONS
        && headers.contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
    {
        return Ok(Admission::Preflight(origin));
    }
    Ok(Admission::Pass { cors })
}

fn add_cors(response: &mut Response, origin: HeaderValue) {
    let h = response.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    h.insert(header::VARY, HeaderValue::from_static("Origin"));
    h.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("X-Request-Id"),
    );
}

pub async fn edge(State(app): State<App>, mut request: Request, next: Next) -> Response {
    let started = Instant::now();
    let request_id = RequestId::new();
    let method = method_label(request.method());
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned());
    let mut cors_origin = None;
    let mut response = match admit(&app, &request) {
        Err(error) => error.into_response(),
        Ok(Admission::Preflight(origin)) => {
            let mut r = StatusCode::NO_CONTENT.into_response();
            let h = r.headers_mut();
            h.insert(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("GET, POST, PATCH, DELETE"),
            );
            h.insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("authorization, content-type"),
            );
            h.insert(
                header::ACCESS_CONTROL_MAX_AGE,
                HeaderValue::from_static("600"),
            );
            cors_origin = Some(origin);
            r
        }
        Ok(Admission::Pass { cors }) => {
            cors_origin = cors;
            match app.in_flight.clone().try_acquire_owned() {
                Err(_) => ApiError::busy().into_response(),
                Ok(permit) => {
                    let limits = &app.config.limits;
                    let timeout = if route.as_deref().is_some_and(long_route) {
                        limits.long_timeout
                    } else {
                        limits.request_timeout
                    };
                    // Blocking calls give up slightly earlier so they can answer themselves.
                    let deadline = started + timeout.saturating_sub(Duration::from_millis(250));
                    request.extensions_mut().insert(request_id.clone());
                    request.extensions_mut().insert(Deadline(deadline));
                    let response = tokio::time::timeout(timeout, next.run(request))
                        .await
                        .unwrap_or_else(|_| ApiError::timeout().into_response());
                    drop(permit);
                    response
                }
            }
        }
    };
    finalize(&mut response, &request_id);
    if let Some(origin) = cors_origin {
        add_cors(&mut response, origin);
    }
    let status = response.status().as_u16();
    let route = route.as_deref().unwrap_or("<unmatched>");
    (app.log)(&format!(
        "request_id={} method={method} route={route} status={status} duration_ms={}",
        request_id.0,
        started.elapsed().as_millis()
    ));
    response
}

/// Every error becomes the standard body; every response gets the request id and
/// no-store/nosniff headers.
fn finalize(response: &mut Response, request_id: &RequestId) {
    if response.status().is_client_error() || response.status().is_server_error() {
        let error = response
            .extensions_mut()
            .remove::<ApiError>()
            .unwrap_or_else(|| ApiError::for_status(response.status()));
        let mut body = serde_json::json!({
            "code": error.code,
            "message": error.message,
            "request_id": request_id.0,
        });
        if let Some(details) = error.details {
            body["details"] = details;
        }
        let bytes = serde_json::to_vec(&serde_json::json!({ "error": body })).unwrap_or_default();
        *response.body_mut() = Body::from(bytes);
        let h = response.headers_mut();
        h.remove(header::CONTENT_LENGTH);
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        if error.status == StatusCode::UNAUTHORIZED {
            h.insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"citation-manager\""),
            );
        }
        if error.status == StatusCode::SERVICE_UNAVAILABLE {
            h.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
    }
    let h = response.headers_mut();
    if let Ok(id) = HeaderValue::from_str(&request_id.0) {
        h.insert(header::HeaderName::from_static("x-request-id"), id);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
}

#[cfg(test)]
mod tests {
    use super::host_allowed;

    #[test]
    fn host_allowlist_is_exact() {
        for ok in [
            "127.0.0.1:4000",
            "localhost:4000",
            "LOCALHOST:4000",
            "[::1]:4000",
        ] {
            assert!(host_allowed(ok, 4000), "{ok}");
        }
        for bad in [
            "127.0.0.1",
            "localhost",
            "127.0.0.1:4001",
            "evil.example:4000",
            "localhost.evil.example:4000",
            "127.0.0.2:4000",
            "0.0.0.0:4000",
            "[::ffff:127.0.0.1]:4000",
            " localhost:4000",
            "",
        ] {
            assert!(!host_allowed(bad, 4000), "{bad}");
        }
    }
}
