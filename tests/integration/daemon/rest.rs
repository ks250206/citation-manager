//! #27 REST API against a real in-process papersd with the API enabled on an ephemeral
//! loopback port. Tokens are issued through #26 (`ApiCommand::IssueToken`). For every
//! `ENDPOINTS` REST row: normal case, unauthenticated, insufficient scope, revoked token,
//! spoofed Host/Origin; for every body endpoint: invalid JSON, wrong media type, unknown
//! fields; plus limits, export path restrictions, leak checks for lower scopes, REST/IPC
//! parity for the same grant, the request log and the settings-driven listener lifecycle.
//! Every response is checked against docs/openapi.json (status documented, body valid).
mod support;
use cm_core::{
    metadata_resolve::Cancellation,
    search::extraction::{ExtractError, ExtractedPage, PageBatch, PageTextExtractor},
};
use cm_daemon::{ipc::start, rest::RestConfig};
use cm_protocol::{
    annotations::{
        Anchor, AnnotationId, AnnotationKind, AnnotationRequest, NewAnnotation,
        Source as AnnotationSource,
    },
    exchange::{ImportReply, ImportRequestUi},
    import::{
        ImportAction, ImportCommit, ImportDecision, ImportDestination, ImportRequest, PaperDraft,
    },
};
use cm_protocol::{
    api::{
        Access, ApiCommand, ApiReply, ApiSettings, BindAddress, BulkKind, ClientKind, ENDPOINTS,
        IssueToken, ListenerState, Scope, ScopeSet, Surface,
    },
    client::{Session, secrets::MemorySecretStore},
    domain::PaperId,
    ipc::{Command, Credential, Denial, Reply, WireError},
    jobs::{JobCommand, JobReply, JobSubmit, kinds},
    library::{LibraryReply, LibraryRequest},
    library_query::LibraryQuery,
    search::{SearchReply, SearchRequest},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use support::*;

const MARKER_TITLE: &str = "Zephyrine lattice marker 4b1d";
const PAGE_WORD: &str = "quokkatext";
const NOTE_TEXT: &str = "private note glimmerquote";
const API: &str = "/api/v1";

// ------------------------------------------------------------------ HTTP client

#[derive(Debug)]
struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}
impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn code(&self) -> String {
        self.json()["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

#[derive(Clone, Default)]
struct Req {
    method: &'static str,
    path: String,
    host: Option<String>,
    token: Option<String>,
    headers: Vec<(String, String)>,
    body: Option<(String, Vec<u8>)>,
}
impl Req {
    fn new(method: &'static str, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            ..Self::default()
        }
    }
    fn token(mut self, token: &Credential) -> Self {
        self.token = Some(token.0.clone());
        self
    }
    fn json(mut self, body: &Value) -> Self {
        self.body = Some(("application/json".into(), serde_json::to_vec(body).unwrap()));
        self
    }
    fn raw(mut self, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some((content_type.into(), body.into()));
        self
    }
    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
    fn host(mut self, host: &str) -> Self {
        self.host = Some(host.into());
        self
    }
}

struct Conn {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    port: u16,
}
impl Conn {
    fn open(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            port,
        }
    }
    fn send(&mut self, req: &Req) -> Resp {
        let mut head = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\n",
            req.method,
            req.path,
            req.host
                .clone()
                .unwrap_or_else(|| format!("127.0.0.1:{}", self.port))
        );
        if let Some(token) = &req.token {
            head.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        for (k, v) in &req.headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        let body = match &req.body {
            Some((content_type, body)) => {
                head.push_str(&format!(
                    "Content-Type: {content_type}\r\nContent-Length: {}\r\n",
                    body.len()
                ));
                body.clone()
            }
            None => {
                if req.method == "POST" || req.method == "PATCH" {
                    head.push_str("Content-Length: 0\r\n");
                }
                Vec::new()
            }
        };
        head.push_str("\r\n");
        self.writer.write_all(head.as_bytes()).unwrap();
        // An early error response may arrive (and the server may close) before the body.
        let _ = self.writer.write_all(&body);
        self.read_response()
    }
    fn read_response(&mut self) -> Resp {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        let status: u16 = line
            .split_whitespace()
            .nth(1)
            .unwrap_or_else(|| panic!("bad status line {line:?}"))
            .parse()
            .unwrap();
        let mut headers = vec![];
        loop {
            let mut h = String::new();
            self.reader.read_line(&mut h).unwrap();
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            let (k, v) = h.split_once(':').unwrap();
            headers.push((k.trim().to_owned(), v.trim().to_owned()));
        }
        let get = |name: &str| {
            headers
                .iter()
                .find(|(k, _): &&(String, String)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        };
        let mut body = vec![];
        if let Some(len) = get("content-length") {
            body.resize(len.parse().unwrap(), 0);
            self.reader.read_exact(&mut body).unwrap();
        } else if get("transfer-encoding").is_some_and(|t| t.contains("chunked")) {
            loop {
                let mut size = String::new();
                self.reader.read_line(&mut size).unwrap();
                let n = usize::from_str_radix(size.trim(), 16).unwrap();
                let mut chunk = vec![0; n + 2];
                self.reader.read_exact(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
        } else if status != 204 && status != 304 {
            let _ = self.reader.read_to_end(&mut body);
        }
        Resp {
            status,
            headers,
            body,
        }
    }
}

// ------------------------------------------------------------------ OpenAPI contract

fn openapi() -> Value {
    serde_json::from_str(include_str!("../../../docs/openapi.json")).unwrap()
}

fn resolve<'a>(doc: &'a Value, schema: &'a Value) -> &'a Value {
    match schema.get("$ref").and_then(Value::as_str) {
        Some(target) => {
            let name = target.strip_prefix("#/components/schemas/").unwrap();
            resolve(doc, &doc["components"]["schemas"][name])
        }
        None => schema,
    }
}

/// The subset of JSON Schema the document uses: $ref, oneOf, const, enum, type (with
/// nullable unions), required, properties, additionalProperties: false, items.
fn validate(doc: &Value, schema: &Value, value: &Value, at: &str) -> Result<(), String> {
    let schema = resolve(doc, schema);
    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        let ok = options
            .iter()
            .filter(|o| validate(doc, o, value, at).is_ok())
            .count();
        return if ok == 1 {
            Ok(())
        } else {
            Err(format!("{at}: {ok} oneOf alternatives match"))
        };
    }
    if let Some(c) = schema.get("const")
        && c != value
    {
        return Err(format!("{at}: expected {c}"));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array)
        && !values.contains(value)
    {
        return Err(format!("{at}: {value} not in enum"));
    }
    if let Some(t) = schema.get("type") {
        let types: Vec<&str> = match t {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        let matches = types.iter().any(|t| match *t {
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "array" => value.is_array(),
            "object" => value.is_object(),
            "null" => value.is_null(),
            _ => false,
        });
        if !matches {
            return Err(format!("{at}: {value} is not {types:?}"));
        }
    }
    if let Some(object) = value.as_object() {
        for key in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let key = key.as_str().unwrap();
            if !object.contains_key(key) {
                return Err(format!("{at}: missing {key}"));
            }
        }
        let props = schema.get("properties").and_then(Value::as_object);
        for (key, v) in object {
            match props.and_then(|p| p.get(key)) {
                Some(s) => validate(doc, s, v, &format!("{at}.{key}"))?,
                None if schema.get("additionalProperties") == Some(&json!(false)) => {
                    return Err(format!("{at}: unexpected {key}"));
                }
                None => {}
            }
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (i, v) in array.iter().enumerate() {
            validate(doc, items, v, &format!("{at}[{i}]"))?;
        }
    }
    Ok(())
}

/// The response status is documented for the endpoint and the body matches its schema.
/// Every error must also carry the request id that is in `X-Request-Id`.
fn conforms(doc: &Value, endpoint: &str, resp: &Resp) {
    let (method, path) = endpoint.split_once(' ').unwrap();
    let op = &doc["paths"][path][method.to_ascii_lowercase()];
    assert!(op.is_object(), "{endpoint} not documented");
    let documented = &op["responses"][resp.status.to_string()];
    assert!(
        documented.is_object(),
        "{endpoint}: status {} not documented: {}",
        resp.status,
        resp.text()
    );
    let request_id = resp.header("x-request-id").expect("X-Request-Id");
    assert!(request_id.starts_with("req_"));
    match documented["content"]["application/json"]["schema"].as_object() {
        Some(_) => {
            let schema = &documented["content"]["application/json"]["schema"];
            let body = resp.json();
            if let Err(e) = validate(doc, schema, &body, "$") {
                panic!("{endpoint} {}: {e}\n{body:#}", resp.status);
            }
            if resp.status >= 400 {
                assert_eq!(body["error"]["request_id"], json!(request_id));
            }
        }
        None => assert!(resp.body.is_empty(), "{endpoint}: unexpected body"),
    }
}

// ------------------------------------------------------------------ fixture

/// Every page of every PDF says `page N discusses <last word of the file>`.
struct Fake;
impl PageTextExtractor for Fake {
    fn extract(
        &self,
        path: &Path,
        first_page: u32,
        max_pages: u32,
        _max_chars: u32,
        _cancel: &Cancellation,
    ) -> Result<PageBatch, ExtractError> {
        let bytes = std::fs::read(path).map_err(|e| ExtractError::Failed(e.to_string()))?;
        let marker = String::from_utf8_lossy(&bytes)
            .split_whitespace()
            .last()
            .unwrap_or("")
            .to_owned();
        let pages = 3;
        Ok(PageBatch {
            page_count: pages,
            sha256: Sha256::digest(&bytes).into(),
            pages: (first_page..(first_page + max_pages).min(pages))
                .map(|i| ExtractedPage {
                    page_index: i,
                    page_label: Some(format!("{}", i + 1)),
                    text: format!("page {i} discusses {marker}"),
                    truncated: false,
                })
                .collect(),
        })
    }
}

struct Api {
    env: Env,
    daemon: Option<cm_daemon::ipc::RunningDaemon>,
    admin: Session,
    port: u16,
    exports: PathBuf,
    log: Arc<Mutex<Vec<String>>>,
    doc: Value,
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn api(session: &mut Session, command: ApiCommand) -> Result<ApiReply, WireError> {
    match session.call(Command::Api(command)).unwrap() {
        Ok(Reply::Api(reply)) => Ok(reply),
        Ok(other) => panic!("{other:?}"),
        Err(e) => Err(e),
    }
}

impl Api {
    fn start() -> Self {
        Self::start_with(|_| {})
    }
    fn start_with(configure: impl FnOnce(&mut RestConfig)) -> Self {
        let env = Env::new();
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let mut config = env.config();
        config.secret_store = Some(Arc::new(MemorySecretStore::default()));
        config.extractor = Some(Arc::new(Fake));
        let sink = log.clone();
        config.rest = RestConfig {
            log: Some(Arc::new(move |line: &str| {
                sink.lock().unwrap().push(line.to_owned())
            })),
            ..RestConfig::default()
        };
        configure(&mut config.rest);
        let daemon = start(config).unwrap();
        let admin = env.desktop();
        let exports = env.temp.path().canonicalize().unwrap().join("exports");
        std::fs::create_dir(&exports).unwrap();
        let port = free_port();
        let mut this = Self {
            env,
            daemon: Some(daemon),
            admin,
            port,
            exports: exports.clone(),
            log,
            doc: openapi(),
        };
        this.set(true, port, vec![exports]);
        this
    }
    fn set(&mut self, enabled: bool, port: u16, export_dirs: Vec<PathBuf>) {
        api(
            &mut self.admin,
            ApiCommand::SaveSettings(ApiSettings {
                enabled,
                bind: BindAddress::LoopbackV4,
                port,
                export_dirs,
            }),
        )
        .unwrap();
        self.port = port;
        let want = if enabled {
            ListenerState::Listening
        } else {
            ListenerState::Stopped
        };
        assert!(wait_until(Duration::from_secs(10), || self.listener() == want));
    }
    fn listener(&mut self) -> ListenerState {
        match api(&mut self.admin, ApiCommand::Overview).unwrap() {
            ApiReply::Overview(o) => o.listener,
            other => panic!("{other:?}"),
        }
    }
    fn token(&mut self, label: &str, scopes: &[Scope]) -> (cm_protocol::ipc::GrantId, Credential) {
        self.token_kind(label, ClientKind::Api, scopes)
    }
    fn token_kind(
        &mut self,
        label: &str,
        kind: ClientKind,
        scopes: &[Scope],
    ) -> (cm_protocol::ipc::GrantId, Credential) {
        match api(
            &mut self.admin,
            ApiCommand::IssueToken(IssueToken {
                label: label.into(),
                kind,
                scopes: scopes.iter().copied().collect(),
                expires_at_ms: None,
            }),
        )
        .unwrap()
        {
            ApiReply::Issued {
                token, credential, ..
            } => (token.id, credential),
            other => panic!("{other:?}"),
        }
    }
    fn send(&self, req: &Req) -> Resp {
        Conn::open(self.port).send(req)
    }
    /// Sends and checks the response against the OpenAPI document.
    fn call(&self, endpoint: &str, req: Req) -> Resp {
        let resp = self.send(&req);
        conforms(&self.doc, endpoint, &resp);
        resp
    }
    fn log_lines(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}
impl Drop for Api {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.shutdown();
        }
    }
}
fn elevate(a: &mut Api, grant: cm_protocol::ipc::GrantId, kind: BulkKind) {
    api(
        &mut a.admin,
        ApiCommand::Elevate {
            grant,
            kind,
            duration_ms: 60_000,
        },
    )
    .unwrap();
}

fn issuable() -> Vec<Scope> {
    Scope::ALL.into_iter().filter(|s| s.issuable()).collect()
}

/// A request for each REST row with plausible ids (authorization happens first, so the
/// ids need not exist for the 401/403/Host/Origin probes).
fn probes() -> Vec<(&'static str, Req)> {
    let id = "019a0000-0000-7000-8000-000000000001";
    let paper = json!({"title": "probe"});
    let ids = json!({"paper_ids": [id]});
    ENDPOINTS
        .iter()
        .filter(|e| e.surface == Surface::Rest)
        .map(|e| {
            let (method, path) = e.name.split_once(' ').unwrap();
            let method: &'static str = match method {
                "GET" => "GET",
                "POST" => "POST",
                "PATCH" => "PATCH",
                "DELETE" => "DELETE",
                other => panic!("{other}"),
            };
            let concrete = path.replace("{id}", id).replace("{page}", "0");
            let concrete = if e.name == "GET /search" {
                format!("{concrete}?q=probe")
            } else {
                concrete
            };
            let mut req = Req::new(method, format!("{API}{concrete}"));
            let body = match e.name {
                "POST /papers" | "PATCH /papers/{id}" => Some(paper.clone()),
                "POST /papers/bulk-delete" | "POST /collections/{id}/papers/bulk-move" => {
                    Some(ids.clone())
                }
                "POST /papers/resolve-doi" => Some(json!({"doi": "10.1234/probe"})),
                "POST /attachments/{id}/annotations" => Some(json!({
                    "kind": "note", "anchor": {"state": "resolved", "page_index": 0},
                    "quote": "", "comment": "probe"
                })),
                "PATCH /annotations/{id}" => Some(json!({"comment": "probe"})),
                "POST /collections" | "POST /tags" => Some(json!({"name": "probe"})),
                "PATCH /collections/{id}" => {
                    Some(json!({"name": "probe", "parent": null, "sort_order": 0}))
                }
                "POST /collections/{id}/papers" => Some(json!({"paper_id": id})),
                "POST /imports" => {
                    Some(json!({"format": "bibtex", "content": "@article{a,title={A}}"}))
                }
                "POST /exports" => Some(json!({"format": "bibtex", "file_name": "probe.bib"})),
                "POST /jobs" => Some(
                    json!({"kind": "diagnostic.wait", "idempotency_key": "probe",
                    "payload": {"millis": 0}}),
                ),
                _ => None,
            };
            if let Some(body) = body {
                req = req.json(&body);
            }
            (e.name, req)
        })
        .collect()
}
fn has_body(name: &str) -> bool {
    matches!(name.split_once(' ').unwrap().0, "POST" | "PATCH")
        && !matches!(
            name,
            "POST /papers/{id}/restore" | "POST /jobs/{id}/cancel" | "POST /jobs/{id}/retry"
        )
}

// ------------------------------------------------------------------ tests

#[test]
fn every_endpoint_requires_a_valid_rest_token() {
    let mut a = Api::start();
    let owner = std::fs::read_to_string(a.env.paths().desktop_credential).unwrap();
    let owner = Credential(owner.trim().to_owned());
    for (name, req) in probes() {
        if name == "GET /health" {
            let resp = a.call(name, req);
            assert_eq!(resp.status, 200);
            assert_eq!(resp.json(), json!({"status": "ok"}));
            continue;
        }
        // No token, a malformed header, an unknown token.
        let resp = a.call(name, req.clone());
        assert_eq!(
            (resp.status, resp.code().as_str()),
            (401, "unauthenticated"),
            "{name}"
        );
        assert!(
            resp.header("www-authenticate")
                .unwrap()
                .starts_with("Bearer")
        );
        let resp = a.call(name, req.clone().header("Authorization", "Basic abc"));
        assert_eq!(resp.status, 401, "{name}");
        let resp = a.call(name, req.clone().token(&Credential("0".repeat(64))));
        assert_eq!(resp.status, 401, "{name}");
        // The owner's desktop credential is never accepted over HTTP.
        let resp = a.call(name, req.clone().token(&owner));
        assert_eq!(
            (resp.status, resp.code().as_str()),
            (403, "token_not_allowed"),
            "{name}"
        );
    }
    // MCP and CLI tokens are the same principal model and work over REST too.
    let (_, mcp) = a.token_kind("mcp", ClientKind::Mcp, &[Scope::PapersRead]);
    let resp = a.call(
        "GET /papers",
        Req::new("GET", format!("{API}/papers")).token(&mcp),
    );
    assert_eq!(resp.status, 200);
}

#[test]
fn every_endpoint_enforces_exactly_its_scopes() {
    let mut a = Api::start();
    let probes = probes();
    for row in ENDPOINTS.iter().filter(|e| e.surface == Surface::Rest) {
        let Access::Scoped(required) = row.access else {
            continue;
        };
        let req = &probes.iter().find(|(n, _)| *n == row.name).unwrap().1;
        for missing in required.iter().filter(|s| s.issuable()) {
            let scopes: Vec<Scope> = issuable().into_iter().filter(|s| s != missing).collect();
            let (_, token) = a.token(&format!("without {missing}"), &scopes);
            let resp = a.call(row.name, req.clone().token(&token));
            assert_eq!(
                resp.status,
                403,
                "{} without {missing}: {}",
                row.name,
                resp.text()
            );
            assert_eq!(resp.code(), "forbidden", "{}", row.name);
            let listed: Vec<&str> = required.iter().map(|s| s.name()).collect();
            assert_eq!(
                resp.json()["error"]["details"]["required_scopes"],
                json!(listed)
            );
        }
        if required.contains(&Scope::BulkElevated) {
            // Every issuable scope, but no time-limited elevation.
            let (_, token) = a.token("no elevation", &issuable());
            let resp = a.call(row.name, req.clone().token(&token));
            assert_eq!(
                (resp.status, resp.code().as_str()),
                (403, "elevation_required"),
                "{}",
                row.name
            );
        }
        // Exactly the required scopes pass authorization (the request may still fail
        // later, e.g. 404 for the probe id, but never with 401/403).
        let exact: Vec<Scope> = required.iter().copied().filter(|s| s.issuable()).collect();
        let (grant, token) = a.token("exact", &exact);
        if required.contains(&Scope::BulkElevated) {
            let kind = if row.name.contains("bulk-delete") {
                BulkKind::Delete
            } else {
                BulkKind::Move
            };
            elevate(&mut a, grant, kind);
        }
        let resp = a.call(row.name, req.clone().token(&token));
        assert!(
            resp.status != 401 && resp.status != 403,
            "{} with exactly {exact:?}: {} {}",
            row.name,
            resp.status,
            resp.text()
        );
    }
}

#[test]
fn revoked_tokens_are_refused_everywhere_including_live_keep_alive_connections() {
    let mut a = Api::start();
    let (grant, token) = a.token("soon revoked", &issuable());
    // Keep-alive: the same TCP connection serves a request, then the token is revoked.
    let mut conn = Conn::open(a.port);
    let first = conn.send(&Req::new("GET", format!("{API}/papers")).token(&token));
    assert_eq!(first.status, 200);
    api(&mut a.admin, ApiCommand::Revoke(grant)).unwrap();
    let second = conn.send(&Req::new("GET", format!("{API}/papers")).token(&token));
    conforms(&a.doc, "GET /papers", &second);
    assert_eq!(
        (second.status, second.code().as_str()),
        (401, "unauthenticated")
    );
    for (name, req) in probes() {
        if name == "GET /health" {
            continue;
        }
        let resp = a.call(name, req.token(&token));
        assert_eq!(resp.status, 401, "{name}");
    }
    // Narrowing a live token's scopes applies to its next request as well.
    let (grant, token) = a.token("narrowed", &[Scope::PapersRead, Scope::PapersWrite]);
    let mut conn = Conn::open(a.port);
    let req = Req::new("POST", format!("{API}/tags"))
        .token(&token)
        .json(&json!({"name": "before narrowing"}));
    assert_eq!(conn.send(&req).status, 201);
    api(
        &mut a.admin,
        ApiCommand::SetScopes {
            grant,
            scopes: [Scope::PapersRead].into_iter().collect::<ScopeSet>(),
        },
    )
    .unwrap();
    let resp = conn.send(
        &Req::new("POST", format!("{API}/tags"))
            .token(&token)
            .json(&json!({"name": "after narrowing"})),
    );
    assert_eq!(resp.status, 403);
}

#[test]
fn invalid_bodies_are_rejected_on_every_body_endpoint() {
    let mut a = Api::start();
    let (grant, token) = a.token("all", &issuable());
    // Body checks happen after authorization, and elevation is per kind: first everything
    // but bulk-delete under a Move elevation, then bulk-delete under a Delete elevation.
    for kind in [BulkKind::Move, BulkKind::Delete] {
        elevate(&mut a, grant, kind);
        for (name, req) in probes() {
            if !has_body(name) || (name == "POST /papers/bulk-delete") != (kind == BulkKind::Delete)
            {
                continue;
            }
            let bad = req
                .clone()
                .token(&token)
                .raw("application/json", "{\"title\": ");
            let resp = a.call(name, bad);
            assert_eq!(
                (resp.status, resp.code().as_str()),
                (400, "invalid_json"),
                "{name}"
            );
            let wrong_type = req.clone().token(&token).raw("text/plain", "{}");
            let resp = a.call(name, wrong_type);
            assert_eq!(resp.status, 415, "{name}");
            let unknown = req
                .clone()
                .token(&token)
                .json(&json!({"unexpected_field": 1}));
            let resp = a.call(name, unknown);
            assert_eq!(resp.status, 400, "{name}: {}", resp.text());
            // Unauthenticated invalid JSON is a 401, not a parse error: bodies are only read
            // after authentication.
            let resp = a.call(name, req.clone().raw("application/json", "{"));
            assert_eq!(resp.status, 401, "{name}");
        }
    }
}

#[test]
fn spoofed_host_origin_and_cross_site_requests_are_refused_everywhere() {
    let mut a = Api::start();
    let (_, token) = a.token("all", &issuable());
    let port = a.port;
    for (name, req) in probes() {
        let req = req.token(&token);
        for host in [
            format!("evil.example:{port}"),
            format!("127.0.0.1:{}", port.wrapping_add(1).max(1024)),
            format!("localhost.evil.example:{port}"),
            "127.0.0.1".to_owned(),
        ] {
            let resp = a.call(name, req.clone().host(&host));
            assert_eq!(
                (resp.status, resp.code().as_str()),
                (400, "host_not_allowed"),
                "{name} Host {host}"
            );
        }
        for origin in [
            "https://evil.example",
            "null",
            &format!("http://127.0.0.1:{port}"),
        ] {
            let resp = a.call(name, req.clone().header("Origin", origin));
            assert_eq!(
                (resp.status, resp.code().as_str()),
                (403, "origin_not_allowed"),
                "{name} Origin {origin}"
            );
            assert!(resp.header("access-control-allow-origin").is_none());
        }
        let resp = a.call(name, req.clone().header("Sec-Fetch-Site", "cross-site"));
        assert_eq!(resp.status, 403, "{name}");
    }
    // Accepted spellings of the loopback host.
    for host in [format!("localhost:{port}"), format!("LOCALHOST:{port}")] {
        let resp = a.call(
            "GET /health",
            Req::new("GET", format!("{API}/health")).host(&host),
        );
        assert_eq!(resp.status, 200);
    }
    // No CORS by default: a preflight is refused like any other browser request.
    let resp = a.send(
        &Req::new("OPTIONS", format!("{API}/papers"))
            .header("Origin", "https://evil.example")
            .header("Access-Control-Request-Method", "GET"),
    );
    assert_eq!(resp.status, 403);
    assert!(resp.header("access-control-allow-origin").is_none());
    // Unknown paths and methods get the standard error as well.
    let resp = a.send(&Req::new("GET", format!("{API}/nope")));
    assert_eq!((resp.status, resp.code().as_str()), (404, "not_found"));
    let resp = a.send(&Req::new("PUT", format!("{API}/papers")).token(&token));
    assert_eq!(resp.status, 405);
    assert_eq!(resp.code(), "method_not_allowed");
}

#[test]
fn a_configured_origin_gets_cors_and_others_do_not() {
    let a = Api::start_with(|rest| rest.allowed_origins = vec!["https://app.example".into()]);
    let resp = a.send(
        &Req::new("OPTIONS", format!("{API}/papers"))
            .header("Origin", "https://app.example")
            .header("Access-Control-Request-Method", "GET"),
    );
    assert_eq!(resp.status, 204);
    assert_eq!(
        resp.header("access-control-allow-origin"),
        Some("https://app.example")
    );
    assert_eq!(
        resp.header("access-control-allow-private-network"),
        Some("true")
    );
    let resp =
        a.send(&Req::new("GET", format!("{API}/papers")).header("Origin", "https://app.example"));
    assert_eq!(resp.status, 401, "still needs a token");
    assert_eq!(
        resp.header("access-control-allow-origin"),
        Some("https://app.example")
    );
    let resp =
        a.send(&Req::new("GET", format!("{API}/health")).header("Origin", "https://other.example"));
    assert_eq!(resp.status, 403);
}

fn create_paper(a: &Api, token: &Credential, title: &str, doi: &str) -> String {
    let resp = a.call(
        "POST /papers",
        Req::new("POST", format!("{API}/papers"))
            .token(token)
            .json(&json!({"title": title, "doi": doi, "year": 2024,
                "authors": [{"family_name": "Lovelace", "given_name": "Ada", "role": "author"}]})),
    );
    assert_eq!(resp.status, 201, "{}", resp.text());
    resp.json()["paper"]["id"].as_str().unwrap().to_owned()
}

/// Attaches a "PDF" through the owner's IPC session and waits for the fake extraction.
fn attach(a: &mut Api, paper: &str, word: &str) -> String {
    // The file name is neutral: only the extracted page text carries `word`.
    let path = a
        .env
        .temp
        .path()
        .canonicalize()
        .unwrap()
        .join(format!("{paper}.pdf"));
    std::fs::write(&path, format!("%PDF-1.7 fake {word}")).unwrap();
    let job = match a
        .admin
        .call(Command::Jobs(JobCommand::Submit(JobSubmit {
            kind: kinds::ATTACHMENT_IMPORT.into(),
            idempotency_key: word.into(),
            payload: json!({"paper": paper, "path": path, "original_name": "paper.pdf", "media_type": "application/pdf"}),
            max_attempts: None,
            deadline_at_ms: None,
        })))
        .unwrap()
        .unwrap()
    {
        Reply::Jobs(JobReply::Job(job)) => job.id,
        other => panic!("{other:?}"),
    };
    assert!(wait_until(Duration::from_secs(30), || {
        matches!(
            a.admin.call(Command::Jobs(JobCommand::Get(job))).unwrap(),
            Ok(Reply::Jobs(JobReply::Job(j))) if !j.state.is_active()
        )
    }));
    let (_, reader) = a.token("status reader", &[Scope::PapersRead]);
    let mut attachment = None;
    assert!(wait_until(Duration::from_secs(30), || {
        let resp = a
            .send(&Req::new("GET", format!("{API}/papers/{paper}/fulltext/status")).token(&reader));
        let body = resp.json();
        let first = &body["attachments"][0];
        attachment = first["attachment_id"].as_str().map(str::to_owned);
        first["state"] == json!("complete")
    }));
    attachment.unwrap()
}

#[test]
fn every_endpoint_normal_case_matches_the_contract() {
    let mut a = Api::start();
    let (grant, full) = a.token("full", &issuable());
    let get = |path: &str| Req::new("GET", format!("{API}{path}")).token(&full);
    let post = |path: &str, body: Value| {
        Req::new("POST", format!("{API}{path}"))
            .token(&full)
            .json(&body)
    };
    let patch = |path: &str, body: Value| {
        Req::new("PATCH", format!("{API}{path}"))
            .token(&full)
            .json(&body)
    };
    let delete = |path: &str| Req::new("DELETE", format!("{API}{path}")).token(&full);
    let covered = std::cell::RefCell::new(std::collections::BTreeSet::new());
    let call = |a: &Api, name: &'static str, req: Req, status: u16| -> Value {
        let resp = a.call(name, req);
        assert_eq!(resp.status, status, "{name}: {}", resp.text());
        covered.borrow_mut().insert(name);
        resp.json()
    };

    assert_eq!(
        call(
            &a,
            "GET /health",
            Req::new("GET", format!("{API}/health")),
            200
        )["status"],
        "ok"
    );
    let tag = call(
        &a,
        "POST /tags",
        post("/tags", json!({"name": "Rest Tag", "color": "#336699"})),
        201,
    );
    let tag_id = tag["tag"]["id"].as_str().unwrap().to_owned();
    let coll = call(
        &a,
        "POST /collections",
        post("/collections", json!({"name": "Rest Coll"})),
        201,
    );
    let coll_id = coll["collection"]["id"].as_str().unwrap().to_owned();
    let other = call(
        &a,
        "POST /collections",
        post("/collections", json!({"name": "Other"})),
        201,
    );
    let other_id = other["collection"]["id"].as_str().unwrap().to_owned();
    let renamed = call(
        &a,
        "PATCH /collections/{id}",
        patch(
            &format!("/collections/{coll_id}"),
            json!({"name": "Rest Collection", "parent": null, "sort_order": 1}),
        ),
        200,
    );
    assert_eq!(renamed["collection"]["name"], "Rest Collection");

    let paper = create_paper(&a, &full, MARKER_TITLE, "10.1234/rest.1");
    covered.borrow_mut().insert("POST /papers");
    let patched = call(
        &a,
        "PATCH /papers/{id}",
        patch(
            &format!("/papers/{paper}"),
            json!({"journal": "Journal of REST", "year": null}),
        ),
        200,
    );
    assert_eq!(patched["paper"]["journal"], "Journal of REST");
    assert!(patched["paper"].get("year").is_none());
    let second = create_paper(&a, &full, "Second paper", "10.1234/rest.2");

    // Cursor paging over two papers.
    let page1 = call(&a, "GET /papers", get("/papers?limit=1"), 200);
    assert_eq!(page1["papers"].as_array().unwrap().len(), 1);
    let cursor = page1["next_cursor"].as_str().unwrap().to_owned();
    let page2 = call(
        &a,
        "GET /papers",
        get(&format!("/papers?limit=1&cursor={cursor}")),
        200,
    );
    let mut seen = vec![
        page1["papers"][0]["id"].as_str().unwrap().to_owned(),
        page2["papers"][0]["id"].as_str().unwrap().to_owned(),
    ];
    seen.sort();
    let mut both = vec![paper.clone(), second.clone()];
    both.sort();
    assert_eq!(seen, both);
    let filtered = call(
        &a,
        "GET /papers",
        get("/papers?keyword=Zephyrine&sort=title&direction=desc"),
        200,
    );
    assert_eq!(filtered["papers"].as_array().unwrap().len(), 1);

    call(
        &a,
        "POST /collections/{id}/papers",
        post(
            &format!("/collections/{coll_id}/papers"),
            json!({"paper_id": paper}),
        ),
        200,
    );
    let attachment = attach(&mut a, &paper, PAGE_WORD);
    let detail = call(
        &a,
        "GET /papers/{id}",
        get(&format!("/papers/{paper}")),
        200,
    );
    assert_eq!(detail["collections"][0]["id"], json!(coll_id));
    assert_eq!(detail["attachments"][0]["id"], json!(attachment));
    assert!(detail["attachments"][0].get("storage_key").is_none());
    let status = call(
        &a,
        "GET /papers/{id}/fulltext/status",
        get(&format!("/papers/{paper}/fulltext/status")),
        200,
    );
    assert_eq!(status["attachments"][0]["state"], "complete");
    let text = call(
        &a,
        "GET /papers/{id}/fulltext",
        get(&format!(
            "/papers/{paper}/fulltext?from_page=1&max_pages=1&max_chars=1000"
        )),
        200,
    );
    assert_eq!(text["pages"][0]["page_index"], 1);
    assert!(
        text["pages"][0]["text"]
            .as_str()
            .unwrap()
            .contains(PAGE_WORD)
    );
    assert_eq!(text["next_page"], 2);
    let found = call(
        &a,
        "GET /search",
        get(&format!("/search?q={PAGE_WORD}&limit=2")),
        200,
    );
    assert!(
        found["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["source"] == "page"
                && h["paper_id"] == json!(paper)
                && h["snippet"].as_str().unwrap().contains(PAGE_WORD))
    );
    // No PDFium in this test build: the renderer reports itself unavailable (documented).
    let render = a.call(
        "GET /attachments/{id}/pages/{page}",
        get(&format!("/attachments/{attachment}/pages/0?zoom=1.5")),
    );
    assert!(
        render.status == 200
            || (render.status == 503 && render.code() == "pdf_runtime_unavailable"),
        "{}",
        render.text()
    );
    covered
        .borrow_mut()
        .insert("GET /attachments/{id}/pages/{page}");

    let note = call(
        &a,
        "POST /attachments/{id}/annotations",
        post(
            &format!("/attachments/{attachment}/annotations"),
            json!({
                "kind": "note", "anchor": {"state": "resolved", "page_index": 0, "page_label": "1"},
                "quote": "", "comment": NOTE_TEXT, "color": "yellow"
            }),
        ),
        201,
    );
    assert_eq!(note["annotation"]["source"], "ai");
    let note_id = note["annotation"]["id"].as_str().unwrap().to_owned();
    let list = call(
        &a,
        "GET /papers/{id}/annotations",
        get(&format!("/papers/{paper}/annotations")),
        200,
    );
    assert_eq!(list["annotations"][0]["comment"], NOTE_TEXT);
    let edited = call(
        &a,
        "PATCH /annotations/{id}",
        patch(
            &format!("/annotations/{note_id}"),
            json!({"comment": "edited note", "color": null}),
        ),
        200,
    );
    assert_eq!(edited["annotation"]["comment"], "edited note");
    assert_eq!(edited["annotation"]["color"], Value::Null);
    call(
        &a,
        "DELETE /annotations/{id}",
        delete(&format!("/annotations/{note_id}")),
        204,
    );

    // DOI preview: an unparseable DOI is refused by the resolver itself (no network here).
    let resolve = call(
        &a,
        "POST /papers/resolve-doi",
        post("/papers/resolve-doi", json!({"doi": "not a doi"})),
        400,
    );
    assert_eq!(resolve["error"]["code"], "invalid_input");

    let bib = "@article{one, title={Imported One}, doi={10.5555/one}, year={2020}}\n\
               @article{two, title={Imported Two}, doi={10.5555/two}, year={2021}}\n\
               @article{dup, title={Duplicate of the first}, doi={10.1234/rest.1}}\n";
    let preview = call(
        &a,
        "POST /imports",
        post(
            "/imports",
            json!({"format": "bibtex", "content": bib, "dry_run": true}),
        ),
        200,
    );
    assert_eq!(preview["total_records"], 3);
    assert_eq!(preview["candidates"][2]["existing_match"], json!(paper));
    let imported = call(
        &a,
        "POST /imports",
        post(
            "/imports",
            json!({"format": "bibtex", "content": bib, "destination": {"tags": [tag_id]}}),
        ),
        201,
    );
    assert_eq!(imported["created"].as_array().unwrap().len(), 2);
    assert_eq!(imported["skipped"], 1);
    let imported_ids: Vec<String> = imported["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();

    let exported = call(
        &a,
        "POST /exports",
        post(
            "/exports",
            json!({"format": "bibtex", "file_name": "refs.bib"}),
        ),
        201,
    );
    assert_eq!(exported["records"], 4);
    let written = std::fs::read_to_string(a.exports.join("refs.bib")).unwrap();
    assert!(written.contains("Imported Two") && written.contains(MARKER_TITLE));
    // Existing files are never overwritten.
    let again = call(
        &a,
        "POST /exports",
        post(
            "/exports",
            json!({"format": "bibtex", "file_name": "refs.bib",
        "selection": {"kind": "ids", "ids": [paper]}}),
        ),
        400,
    );
    assert_eq!(again["error"]["code"], "invalid_input");

    // A job owned by this token (submitted over IPC with the same credential).
    let mut ipc = a.env.session("cli", full.clone());
    let job = match ipc
        .call(Command::Jobs(JobCommand::Submit(JobSubmit {
            kind: kinds::DIAGNOSTIC_WAIT.into(),
            idempotency_key: "rest-job".into(),
            payload: json!({"millis": 60_000}),
            max_attempts: None,
            deadline_at_ms: None,
        })))
        .unwrap()
        .unwrap()
    {
        Reply::Jobs(JobReply::Job(job)) => job.id,
        other => panic!("{other:?}"),
    };
    let got = call(&a, "GET /jobs/{id}", get(&format!("/jobs/{}", job.0)), 200);
    assert_eq!(got["job"]["kind"], kinds::DIAGNOSTIC_WAIT);
    let cancelled = call(
        &a,
        "POST /jobs/{id}/cancel",
        Req::new("POST", format!("{API}/jobs/{}/cancel", job.0)).token(&full),
        200,
    );
    assert!(
        cancelled["job"]["cancel_requested"] == json!(true)
            || cancelled["job"]["state"] == "cancelled"
    );
    // Once cancelled, the token may retry its own job; then cancel it again.
    assert!(wait_until(Duration::from_secs(10), || {
        a.send(&get(&format!("/jobs/{}", job.0))).json()["job"]["state"] == "cancelled"
    }));
    let retried = call(
        &a,
        "POST /jobs/{id}/retry",
        Req::new("POST", format!("{API}/jobs/{}/retry", job.0)).token(&full),
        200,
    );
    assert!(retried["job"]["state"] == "queued" || retried["job"]["state"] == "running");
    a.send(&Req::new("POST", format!("{API}/jobs/{}/cancel", job.0)).token(&full));
    // Submitted over REST: owned by the token, visible over IPC with the same credential.
    let submitted = call(
        &a,
        "POST /jobs",
        post(
            "/jobs",
            json!({"kind": kinds::DIAGNOSTIC_WAIT, "idempotency_key": "rest-submitted",
                "payload": {"millis": 0}}),
        ),
        201,
    );
    let submitted_id = submitted["job"]["id"].as_str().unwrap().to_owned();
    match ipc
        .call(Command::Jobs(JobCommand::Get(cm_protocol::jobs::JobId(
            submitted_id.parse().unwrap(),
        ))))
        .unwrap()
    {
        Ok(Reply::Jobs(JobReply::Job(j))) => assert_eq!(j.idempotency_key, "rest-submitted"),
        other => panic!("{other:?}"),
    }
    // Kinds that need a local-file capability stay owner-only.
    let local = a.call(
        "POST /jobs",
        post(
            "/jobs",
            json!({"kind": kinds::ATTACHMENT_IMPORT, "idempotency_key": "nope",
                "payload": {"paper": paper, "path": "/etc/passwd"}}),
        ),
    );
    assert_eq!(local.status, 403, "{}", local.text());
    // Another owner's job is invisible (404, not 403).
    let (_, other_token) = a.token("other jobs", &[Scope::JobsRead]);
    let hidden = a.call(
        "GET /jobs/{id}",
        Req::new("GET", format!("{API}/jobs/{}", job.0)).token(&other_token),
    );
    assert_eq!(hidden.status, 404);

    elevate(&mut a, grant, BulkKind::Move);
    let moved = call(
        &a,
        "POST /collections/{id}/papers/bulk-move",
        post(
            &format!("/collections/{other_id}/papers/bulk-move"),
            json!({"paper_ids": [paper], "from": coll_id}),
        ),
        200,
    );
    assert_eq!(moved["results"][0]["ok"], true);
    assert_eq!(moved["stopped"], Value::Null);
    // Elevation is per kind: a Move elevation does not lift the delete limit.
    let refused = a.call(
        "POST /papers/bulk-delete",
        post("/papers/bulk-delete", json!({"paper_ids": imported_ids})),
    );
    assert_eq!(
        (refused.status, refused.code().as_str()),
        (403, "elevation_required")
    );
    elevate(&mut a, grant, BulkKind::Delete);
    let bulk = call(
        &a,
        "POST /papers/bulk-delete",
        post("/papers/bulk-delete", json!({"paper_ids": imported_ids})),
        200,
    );
    assert!(
        bulk["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["ok"] == true)
    );

    let trashed = call(
        &a,
        "DELETE /papers/{id}",
        delete(&format!("/papers/{second}")),
        200,
    );
    assert_eq!(trashed["deleted"], true);
    let restored = call(
        &a,
        "POST /papers/{id}/restore",
        Req::new("POST", format!("{API}/papers/{second}/restore")).token(&full),
        200,
    );
    assert_eq!(restored["deleted"], false);
    let colls = call(&a, "GET /collections", get("/collections"), 200);
    assert_eq!(colls["collections"].as_array().unwrap().len(), 2);
    call(
        &a,
        "DELETE /collections/{id}",
        delete(&format!("/collections/{coll_id}")),
        204,
    );
    let tags = call(&a, "GET /tags", get("/tags"), 200);
    assert_eq!(tags["tags"][0]["name"], "Rest Tag");
    call(
        &a,
        "DELETE /tags/{id}",
        delete(&format!("/tags/{tag_id}")),
        204,
    );

    let all: std::collections::BTreeSet<&str> = ENDPOINTS
        .iter()
        .filter(|e| e.surface == Surface::Rest)
        .map(|e| e.name)
        .collect();
    let covered = covered.into_inner();
    let missing: Vec<_> = all.difference(&covered).collect();
    assert!(missing.is_empty(), "normal case not exercised: {missing:?}");
    // The audit log records REST writes as API-token operations (no content).
    match api(&mut a.admin, ApiCommand::Audit { limit: 200 }).unwrap() {
        ApiReply::Audit(entries) => {
            assert!(entries.iter().any(|e| e.grant == Some(grant)
                && e.kind == Some(ClientKind::Api)
                && e.operation == cm_protocol::ipc::Operation::PaperCreate));
        }
        other => panic!("{other:?}"),
    }
}

fn expect(a: &Api, name: &str, req: Req, status: u16, code: &str) {
    let resp = a.call(name, req);
    assert_eq!(
        (resp.status, resp.code().as_str()),
        (status, code),
        "{name}: {}",
        resp.text()
    );
}

#[test]
fn limits_and_path_restrictions() {
    let mut a = Api::start();
    let (grant, full) = a.token("full", &issuable());
    let paper = create_paper(&a, &full, MARKER_TITLE, "10.1234/limits");
    let get = |path: &str| Req::new("GET", format!("{API}{path}")).token(&full);
    let post = |path: &str, body: Value| {
        Req::new("POST", format!("{API}{path}"))
            .token(&full)
            .json(&body)
    };
    // Paging and character limits.
    expect(
        &a,
        "GET /papers",
        get("/papers?limit=0"),
        400,
        "invalid_input",
    );
    expect(
        &a,
        "GET /papers",
        get("/papers?limit=501"),
        400,
        "invalid_input",
    );
    expect(
        &a,
        "GET /papers",
        get("/papers?cursor=bm90LWEtY3Vyc29y"),
        400,
        "invalid_input",
    );
    expect(
        &a,
        "GET /papers",
        get("/papers?unknown=1"),
        400,
        "invalid_input",
    );
    expect(
        &a,
        "GET /search",
        get("/search?q=a&limit=101"),
        400,
        "invalid_input",
    );
    expect(
        &a,
        "GET /search",
        get("/search?q=a&snippet_chars=5000"),
        400,
        "invalid_input",
    );
    expect(
        &a,
        "GET /search",
        get("/search?q=a&timeout_ms=0"),
        400,
        "invalid_input",
    );
    expect(
        &a,
        "GET /search",
        get(&format!("/search?q={}", "a".repeat(4097))),
        400,
        "invalid_input",
    );
    expect(
        &a,
        "GET /search",
        get("/search?q=%22unclosed"),
        400,
        "invalid_query",
    );
    for range in [
        "max_pages=51",
        "max_pages=0",
        "max_chars=400001",
        "max_chars=0",
    ] {
        expect(
            &a,
            "GET /papers/{id}/fulltext",
            get(&format!("/papers/{paper}/fulltext?{range}")),
            400,
            "invalid_input",
        );
    }
    // No PDF attached: nothing to read.
    expect(
        &a,
        "GET /papers/{id}/fulltext",
        get(&format!("/papers/{paper}/fulltext")),
        404,
        "not_found",
    );
    expect(
        &a,
        "GET /papers/{id}",
        get("/papers/not-a-uuid"),
        400,
        "invalid_input",
    );
    expect(
        &a,
        "GET /attachments/{id}/pages/{page}",
        get(&format!("/attachments/{paper}/pages/0?zoom=100")),
        400,
        "invalid_input",
    );
    // Bodies.
    let huge = json!({"title": "x".repeat(1024 * 1024 + 1)});
    expect(&a, "POST /papers", post("/papers", huge), 413, "too_large");
    expect(
        &a,
        "PATCH /papers/{id}",
        Req::new("PATCH", format!("{API}/papers/{paper}"))
            .token(&full)
            .json(&json!({})),
        400,
        "invalid_input",
    );
    let too_many: Vec<String> = (0..501)
        .map(|i| format!("019a0000-0000-7000-8000-{i:012}"))
        .collect();
    elevate(&mut a, grant, BulkKind::Delete);
    expect(
        &a,
        "POST /papers/bulk-delete",
        post("/papers/bulk-delete", json!({"paper_ids": too_many})),
        400,
        "invalid_input",
    );
    expect(
        &a,
        "POST /papers/bulk-delete",
        post("/papers/bulk-delete", json!({"paper_ids": [paper, paper]})),
        400,
        "invalid_input",
    );
    let big_import = "@article{a,title={x}}\n".repeat(110_000);
    expect(
        &a,
        "POST /imports",
        post(
            "/imports",
            json!({"format": "bibtex", "content": big_import}),
        ),
        413,
        "too_large",
    );
    // URI limit.
    let resp = a.send(&get(&format!("/papers?keyword={}", "k".repeat(17 * 1024))));
    assert!(resp.status == 414 || resp.status == 431, "{}", resp.status);
    // Export: plain names inside the owner's folders only; never a client path.
    for name in [
        "../escape.bib",
        "/etc/passwd",
        ".hidden.bib",
        "a/b.bib",
        "a\\b.bib",
    ] {
        expect(
            &a,
            "POST /exports",
            post("/exports", json!({"format": "bibtex", "file_name": name})),
            400,
            "invalid_input",
        );
    }
    let outside = a.env.temp.path().canonicalize().unwrap();
    expect(
        &a,
        "POST /exports",
        post(
            "/exports",
            json!({"format": "bibtex", "file_name": "x.bib", "folder": outside}),
        ),
        403,
        "export_folder_not_allowed",
    );
    expect(
        &a,
        "POST /exports",
        post(
            "/exports",
            json!({"format": "bibtex", "file_name": "x.bib", "folder": format!("{}/../", a.exports.display())}),
        ),
        403,
        "export_folder_not_allowed",
    );
    assert!(!outside.join("x.bib").exists());
    // The extension must match the format (daemon export guard, #26).
    for (format, name) in [("bibtex", "x.json"), ("ris", "x.bib"), ("bibtex", "noext")] {
        expect(
            &a,
            "POST /exports",
            post("/exports", json!({"format": format, "file_name": name})),
            403,
            "export_folder_not_allowed",
        );
    }
    // A symlink placed inside the allowed folder is refused by the writer.
    std::os::unix::fs::symlink(&outside, a.exports.join("link.bib")).unwrap();
    expect(
        &a,
        "POST /exports",
        post(
            "/exports",
            json!({"format": "bibtex", "file_name": "link.bib"}),
        ),
        400,
        "invalid_input",
    );
    assert!(
        !outside.join("link").exists()
            && std::fs::read_dir(&outside)
                .unwrap()
                .all(|e| e.unwrap().file_name() != "x.bib")
    );
    let port = a.port;
    a.set(true, port, vec![]);
    expect(
        &a,
        "POST /exports",
        post(
            "/exports",
            json!({"format": "bibtex", "file_name": "y.bib"}),
        ),
        403,
        "export_folder_not_allowed",
    );
}

#[test]
fn lower_scopes_never_see_full_text_or_annotation_content() {
    let mut a = Api::start();
    let (_, full) = a.token("full", &issuable());
    let paper = create_paper(&a, &full, MARKER_TITLE, "10.1234/leak");
    let attachment = attach(&mut a, &paper, PAGE_WORD);
    let resp = a.call(
        "POST /attachments/{id}/annotations",
        Req::new(
            "POST",
            format!("{API}/attachments/{attachment}/annotations"),
        )
        .token(&full)
        .json(
            &json!({"kind": "note", "anchor": {"state": "resolved", "page_index": 0},
                "quote": "", "comment": NOTE_TEXT}),
        ),
    );
    assert_eq!(resp.status, 201);
    let note_id = resp.json()["annotation"]["id"].as_str().unwrap().to_owned();
    let secrets = [PAGE_WORD, "glimmerquote"];
    let clean = |resp: &Resp| {
        for s in secrets {
            assert!(!resp.text().contains(s), "leaked {s}: {}", resp.text());
        }
    };
    // papers:read alone: metadata yes, search/fulltext/annotations no.
    let (_, meta) = a.token("metadata only", &[Scope::PapersRead]);
    for (name, path) in [
        ("GET /search", format!("/search?q={PAGE_WORD}")),
        ("GET /search", "/search?q=glimmerquote".to_owned()),
        (
            "GET /papers/{id}/fulltext",
            format!("/papers/{paper}/fulltext"),
        ),
        (
            "GET /papers/{id}/annotations",
            format!("/papers/{paper}/annotations"),
        ),
    ] {
        let resp = a.call(name, Req::new("GET", format!("{API}{path}")).token(&meta));
        assert_eq!(resp.status, 403, "{name}");
        clean(&resp);
    }
    for (name, path) in [
        ("GET /papers", format!("/papers?keyword={PAGE_WORD}")),
        ("GET /papers", "/papers?keyword=glimmerquote".to_owned()),
        ("GET /papers/{id}", format!("/papers/{paper}")),
        (
            "GET /papers/{id}/fulltext/status",
            format!("/papers/{paper}/fulltext/status"),
        ),
    ] {
        let resp = a.call(name, Req::new("GET", format!("{API}{path}")).token(&meta));
        assert_eq!(resp.status, 200, "{name}");
        clean(&resp);
    }
    // Two of the three read scopes are still not enough for search snippets.
    for pair in [
        [Scope::PapersRead, Scope::FilesRead],
        [Scope::PapersRead, Scope::AnnotationsRead],
    ] {
        let (_, t) = a.token("two of three", &pair);
        let resp = a.call(
            "GET /search",
            Req::new("GET", format!("{API}/search?q={PAGE_WORD}")).token(&t),
        );
        assert_eq!(resp.status, 403);
        clean(&resp);
    }
    // Write-only tokens get ids and timestamps back, not stored records.
    let (_, write_only) = a.token("write only", &[Scope::PapersWrite]);
    let resp = a.call(
        "PATCH /papers/{id}",
        Req::new("PATCH", format!("{API}/papers/{paper}"))
            .token(&write_only)
            .json(&json!({"volume": "7"})),
    );
    assert_eq!(resp.status, 200);
    assert!(resp.json().get("paper").is_none());
    assert!(!resp.text().contains(MARKER_TITLE));
    let (_, annotate_only) = a.token("annotate only", &[Scope::AnnotationsWrite]);
    let resp = a.call(
        "PATCH /annotations/{id}",
        Req::new("PATCH", format!("{API}/annotations/{note_id}"))
            .token(&annotate_only)
            .json(&json!({"color": "green"})),
    );
    assert_eq!(resp.status, 200);
    assert!(resp.json().get("annotation").is_none());
    clean(&resp);
    // Import preview for a write-only token hides which existing paper matched.
    let resp = a.call(
        "POST /imports",
        Req::new("POST", format!("{API}/imports"))
            .token(&write_only)
            .json(&json!({
            "format": "bibtex", "dry_run": true,
            "content": "@article{d, title={Dup}, doi={10.1234/leak}}"})),
    );
    assert_eq!(resp.status, 200);
    assert_eq!(resp.json()["candidates"][0]["existing_match"], Value::Null);
    assert!(!resp.text().contains(&paper));
}

/// Review P2: a token that cannot read papers neither learns from a dry run whether a
/// record already exists, nor creates a duplicate by committing it.
#[test]
fn write_only_imports_never_duplicate_and_never_reveal_existing_papers() {
    let mut a = Api::start();
    let (_, full) = a.token("full", &issuable());
    let existing = create_paper(&a, &full, MARKER_TITLE, "10.1234/existing");
    let (_, write_only) = a.token("write only", &[Scope::PapersWrite]);
    let import = |content: &str, dry_run: bool| {
        a.call(
            "POST /imports",
            Req::new("POST", format!("{API}/imports"))
                .token(&write_only)
                .json(&json!({"format": "bibtex", "dry_run": dry_run, "content": content})),
        )
    };
    // Same citation key as the existing paper and an exact DOI match: nothing in the dry
    // run may differ from a record that matches nothing.
    let dup = "@article{same, title={Existing again}, doi={10.1234/existing}}";
    let fresh = "@article{same, title={Brand new}, doi={10.1234/brand-new}}";
    let shown = |resp: &Resp| {
        let mut body = resp.json();
        // Titles differ by construction; everything else must be identical.
        body["candidates"][0]["title"] = Value::Null;
        body
    };
    let dup_preview = import(dup, true);
    let fresh_preview = import(fresh, true);
    assert_eq!(dup_preview.status, 200);
    assert_eq!(shown(&dup_preview), shown(&fresh_preview));
    assert!(!dup_preview.text().contains(&existing));
    // The warning is one constant text, present whether or not anything matched.
    assert!(fresh_preview.text().contains("hidden for this client"));
    // Committing the duplicate skips it daemon-side (no second paper, no error).
    let committed = import(dup, false);
    assert_eq!(committed.status, 201, "{}", committed.text());
    assert_eq!(committed.json()["created"], json!([]));
    let committed = import(fresh, false);
    assert_eq!(committed.json()["created"].as_array().unwrap().len(), 1);
    let all = a.call(
        "GET /papers",
        Req::new("GET", format!("{API}/papers?limit=500")).token(&full),
    );
    // The existing paper and the fresh import only.
    assert_eq!(
        all.json()["papers"].as_array().unwrap().len(),
        2,
        "{}",
        all.text()
    );
    let existing_count = all.json()["papers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["doi"] == json!("10.1234/existing"))
        .count();
    assert_eq!(existing_count, 1);
    // The same rule over IPC for the same grant: Create on a hidden duplicate is skipped.
    let mut ipc = a.env.session("cli", write_only.clone());
    let preview = match ipc
        .call(Command::Import(ImportRequestUi::Preview(ImportRequest {
            candidates: vec![{
                let mut d = PaperDraft::titled("Existing via IPC");
                d.bibliography.doi = Some("10.1234/existing".into());
                d
            }],
            destination: ImportDestination::default(),
        })))
        .unwrap()
        .unwrap()
    {
        Reply::Import(ImportReply::Preview(p)) => p,
        other => panic!("{other:?}"),
    };
    assert!(preview.candidates[0].matches.is_empty());
    let outcome = match ipc
        .call(Command::Import(ImportRequestUi::Commit(ImportCommit {
            preview_id: preview.id,
            decisions: vec![ImportDecision {
                candidate_index: 0,
                action: ImportAction::Create,
            }],
        })))
        .unwrap()
        .unwrap()
    {
        Reply::Import(ImportReply::Committed(o)) => o,
        other => panic!("{other:?}"),
    };
    assert!(outcome.created.is_empty());
}

/// Review P2: tokens cannot spoof human provenance or review AI annotations.
#[test]
fn tokens_create_only_ai_annotations_and_cannot_confirm_them() {
    let mut a = Api::start();
    let (_, full) = a.token("full", &issuable());
    let paper = create_paper(&a, &full, MARKER_TITLE, "10.1234/provenance");
    let attachment = attach(&mut a, &paper, PAGE_WORD);
    for source in ["human", "import"] {
        let resp = a.call(
            "POST /attachments/{id}/annotations",
            Req::new("POST", format!("{API}/attachments/{attachment}/annotations"))
                .token(&full)
                .json(&json!({"kind": "note", "source": source,
                    "anchor": {"state": "resolved", "page_index": 0}, "quote": "", "comment": "x"})),
        );
        assert_eq!(
            (resp.status, resp.code().as_str()),
            (400, "invalid_input"),
            "{source}"
        );
    }
    let created = a.call(
        "POST /attachments/{id}/annotations",
        Req::new(
            "POST",
            format!("{API}/attachments/{attachment}/annotations"),
        )
        .token(&full)
        .json(
            &json!({"kind": "note", "anchor": {"state": "resolved", "page_index": 0},
                "quote": "", "comment": "ai note"}),
        ),
    );
    assert_eq!(created.status, 201);
    assert_eq!(created.json()["annotation"]["source"], "ai");
    let id = AnnotationId(
        created.json()["annotation"]["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap(),
    );
    // The same token over IPC: human source, confirmation and replayed rows are refused.
    let mut ipc = a.env.session("mcp", full.clone());
    let new = |source| NewAnnotation {
        attachment_id: cm_protocol::domain::AttachmentId(attachment.parse().unwrap()),
        kind: AnnotationKind::Note,
        source,
        anchor: Anchor::Resolved {
            page_index: 0,
            page_label: None,
            quads: vec![],
        },
        quote: String::new(),
        context_before: None,
        context_after: None,
        color: None,
        comment: Some("spoof".into()),
    };
    assert!(matches!(
        ipc.call(Command::Annotations(AnnotationRequest::Create(Box::new(
            new(AnnotationSource::Human)
        ))))
        .unwrap(),
        Err(WireError::Service(
            cm_protocol::library::ServiceError::InvalidInput(_)
        ))
    ));
    assert!(matches!(
        ipc.call(Command::Annotations(AnnotationRequest::ConfirmAi(id)))
            .unwrap(),
        Err(WireError::Denied(Denial::Forbidden(_)))
    ));
    assert!(
        ipc.call(Command::Annotations(AnnotationRequest::Create(Box::new(
            new(AnnotationSource::Ai)
        ))))
        .unwrap()
        .is_ok()
    );
    // The owner (desktop) still confirms AI annotations.
    assert!(
        a.admin
            .call(Command::Annotations(AnnotationRequest::ConfirmAi(id)))
            .unwrap()
            .is_ok()
    );
}

/// Review P3: request-target and framing edge cases.
#[test]
fn edge_cases_absolute_form_duplicate_host_http10_and_chunked_bodies() {
    let mut a = Api::start();
    let (_, token) = a.token("edge", &issuable());
    let port = a.port;
    let raw = |text: String| {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let _ = s.write_all(text.as_bytes());
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        String::from_utf8_lossy(&out).into_owned()
    };
    // Absolute-form target naming another authority, with a valid Host.
    let out = raw(format!(
        "GET http://evil.example:{port}{API}/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    ));
    assert!(
        out.starts_with("HTTP/1.1 400") && out.contains("host_not_allowed"),
        "{out}"
    );
    // Absolute-form naming the API itself is fine.
    let out = raw(format!(
        "GET http://127.0.0.1:{port}{API}/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    ));
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    // Two Host headers.
    let out = raw(format!(
        "GET {API}/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nHost: evil.example:{port}\r\nConnection: close\r\n\r\n"
    ));
    assert!(out.starts_with("HTTP/1.1 400"), "{out}");
    // HTTP/1.0 without Host.
    let out = raw(format!("GET {API}/health HTTP/1.0\r\n\r\n"));
    assert!(
        out.starts_with("HTTP/1.0 400") || out.starts_with("HTTP/1.1 400"),
        "{out}"
    );
    // A chunked body over the JSON limit (no Content-Length to check up front).
    let chunk = "a".repeat(64 * 1024);
    let mut body = String::from("{\"title\":\"");
    while body.len() < 1024 * 1024 + 1 {
        body.push_str(&chunk);
    }
    body.push_str("\"}");
    let mut request = format!(
        "POST {API}/papers HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        token.0
    );
    for piece in body.as_bytes().chunks(64 * 1024) {
        request.push_str(&format!("{:x}\r\n", piece.len()));
        request.push_str(std::str::from_utf8(piece).unwrap());
        request.push_str("\r\n");
    }
    request.push_str("0\r\n\r\n");
    let out = raw(request);
    assert!(
        out.starts_with("HTTP/1.1 413") && out.contains("too_large"),
        "{}",
        &out[..out.len().min(300)]
    );
    // Nothing was created.
    let list = a.call(
        "GET /papers",
        Req::new("GET", format!("{API}/papers")).token(&token),
    );
    assert_eq!(list.json()["papers"], json!([]));
    drop(a.admin.call(Command::Status));
}

#[test]
fn rest_and_ipc_agree_for_the_same_grant() {
    let mut a = Api::start();
    let (_, token) = a.token("parity", &issuable());
    for (title, doi) in [
        (MARKER_TITLE, "10.1/p1"),
        ("Second parity", "10.1/p2"),
        ("Third", "10.1/p3"),
    ] {
        create_paper(&a, &token, title, doi);
    }
    let paper = create_paper(&a, &token, "Paged parity paper", "10.1/p4");
    attach(&mut a, &paper, PAGE_WORD);
    // The same credential over IPC (as the CLI and MCP adapters use it).
    let mut ipc = a.env.session("cli", token.clone());
    let rest = a.call(
        "GET /papers",
        Req::new("GET", format!("{API}/papers?limit=500")).token(&token),
    );
    let rest_papers: Vec<Value> = rest.json()["papers"].as_array().unwrap().clone();
    let ipc_papers = match ipc
        .call(Command::Query(LibraryQuery {
            limit: 500,
            ..LibraryQuery::default()
        }))
        .unwrap()
        .unwrap()
    {
        Reply::Page(page) => page.papers,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        rest_papers,
        serde_json::to_value(&ipc_papers)
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
    );
    // Single paper.
    let rest_one = a.call(
        "GET /papers/{id}",
        Req::new("GET", format!("{API}/papers/{paper}")).token(&token),
    );
    let ipc_one = match ipc
        .call(Command::Library(LibraryRequest::Get(PaperId(
            paper.parse().unwrap(),
        ))))
        .unwrap()
        .unwrap()
    {
        Reply::Library(LibraryReply::Paper(p)) => p,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        rest_one.json()["paper"],
        serde_json::to_value(&*ipc_one).unwrap()
    );
    // Search: same hits, order, snippets and scores.
    for q in [PAGE_WORD, "parity", "Zephyrine"] {
        let rest = a.call(
            "GET /search",
            Req::new("GET", format!("{API}/search?q={q}&limit=50")).token(&token),
        );
        let ipc_hits = match ipc
            .call(Command::Search(Box::new(SearchRequest {
                limit: 50,
                ..SearchRequest::new(q)
            })))
            .unwrap()
            .unwrap()
        {
            Reply::Search(SearchReply::Results(page)) => page.hits,
            other => panic!("{other:?}"),
        };
        let rest_hits = rest.json()["results"].as_array().unwrap().clone();
        assert!(!rest_hits.is_empty(), "{q}");
        assert_eq!(rest_hits.len(), ipc_hits.len(), "{q}");
        for (r, i) in rest_hits.iter().zip(&ipc_hits) {
            assert_eq!(r["paper_id"], json!(i.paper_id.0), "{q}");
            assert_eq!(r["title"], json!(i.paper_title));
            assert_eq!(r["snippet"], json!(i.snippet));
            assert_eq!(r["source"], serde_json::to_value(i.source).unwrap());
            assert_eq!(r["page_index"], json!(i.page_index));
            assert_eq!(r["score"].as_f64().unwrap(), i.score);
        }
    }
    // The same denials for a lower grant on both transports.
    let (_, meta) = a.token("parity meta", &[Scope::PapersRead]);
    let mut ipc_meta = a.env.session("mcp", meta.clone());
    let rest = a.call(
        "GET /search",
        Req::new("GET", format!("{API}/search?q=parity")).token(&meta),
    );
    assert_eq!(rest.status, 403);
    assert_eq!(
        ipc_meta
            .call(Command::Search(Box::new(SearchRequest::new("parity"))))
            .unwrap()
            .unwrap_err(),
        WireError::Denied(Denial::Forbidden(
            cm_protocol::ipc::Operation::FulltextSearch
        ))
    );
    let rest = a.call(
        "GET /papers",
        Req::new("GET", format!("{API}/papers")).token(&meta),
    );
    assert_eq!(rest.status, 200);
    assert!(
        ipc_meta
            .call(Command::Query(LibraryQuery::default()))
            .unwrap()
            .is_ok()
    );
}

#[test]
fn the_request_log_has_no_tokens_queries_or_content() {
    let mut a = Api::start();
    let (_, token) = a.token("logged", &issuable());
    let paper = create_paper(&a, &token, MARKER_TITLE, "10.1234/log");
    a.send(&Req::new("GET", format!("{API}/search?q=Zephyrine")).token(&token));
    a.send(
        &Req::new("PATCH", format!("{API}/papers/{paper}"))
            .token(&token)
            .json(&json!({"abstract": "secret abstract words"})),
    );
    a.send(&Req::new("GET", format!("{API}/papers")).token(&Credential("f".repeat(64))));
    a.send(&Req::new("GET", format!("{API}/health")).host("evil.example:1"));
    let lines = a.log_lines();
    assert!(lines.len() >= 5, "{lines:?}");
    for line in &lines {
        for forbidden in [
            &token.0[..],
            MARKER_TITLE,
            "Zephyrine",
            "secret abstract",
            &"f".repeat(64),
            "evil.example",
            "10.1234",
        ] {
            assert!(!line.contains(forbidden), "{forbidden} in {line}");
        }
    }
    let request_lines: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("request_id="))
        .collect();
    assert!(
        request_lines
            .iter()
            .any(|l| l.contains("route=/api/v1/papers/{id}")
                && l.contains("method=PATCH")
                && l.contains("status=200"))
    );
    assert!(request_lines.iter().any(|l| l.contains("status=401")));
    assert!(request_lines.iter().all(|l| l.contains(" duration_ms=")));
}

#[test]
fn the_listener_is_off_by_default_and_follows_the_settings() {
    let env = Env::new();
    let mut config = env.config();
    config.secret_store = Some(Arc::new(MemorySecretStore::default()));
    let daemon = start(config).unwrap();
    let mut admin = env.desktop();
    let overview = |admin: &mut Session| match api(admin, ApiCommand::Overview).unwrap() {
        ApiReply::Overview(o) => o,
        other => panic!("{other:?}"),
    };
    let o = overview(&mut admin);
    assert!(!o.settings.enabled);
    assert_eq!(o.listener, ListenerState::Stopped);
    let port = o.settings.port;
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err() || port_in_use_elsewhere(port));
    let save = |admin: &mut Session, enabled: bool, port: u16| {
        api(
            admin,
            ApiCommand::SaveSettings(ApiSettings {
                enabled,
                bind: BindAddress::LoopbackV4,
                port,
                export_dirs: vec![],
            }),
        )
        .unwrap();
    };
    let first = free_port();
    save(&mut admin, true, first);
    assert!(wait_until(Duration::from_secs(10), || overview(&mut admin)
        .listener
        == ListenerState::Listening));
    assert_eq!(
        Conn::open(first)
            .send(&Req::new("GET", format!("{API}/health")))
            .status,
        200
    );
    let second = free_port();
    save(&mut admin, true, second);
    assert!(wait_until(Duration::from_secs(10), || {
        TcpStream::connect(("127.0.0.1", first)).is_err()
            && TcpStream::connect(("127.0.0.1", second)).is_ok()
    }));
    // The Host check follows the new port.
    let resp = Conn::open(second)
        .send(&Req::new("GET", format!("{API}/health")).host(&format!("127.0.0.1:{first}")));
    assert_eq!(resp.status, 400);
    save(&mut admin, false, second);
    assert!(wait_until(Duration::from_secs(10), || overview(&mut admin)
        .listener
        == ListenerState::Stopped
        && TcpStream::connect(("127.0.0.1", second)).is_err()));
    drop(admin);
    daemon.shutdown();
}

fn port_in_use_elsewhere(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_err()
}

/// The real binary: an enabled API comes up when papersd starts and keeps it running.
#[test]
fn papersd_binary_serves_the_persisted_api_on_start() {
    let env = Env::new();
    let port = free_port();
    {
        let mut config = env.config();
        config.secret_store = Some(Arc::new(MemorySecretStore::default()));
        let daemon = start(config).unwrap();
        let mut admin = env.desktop();
        api(
            &mut admin,
            ApiCommand::SaveSettings(ApiSettings {
                enabled: true,
                bind: BindAddress::LoopbackV4,
                port,
                export_dirs: vec![],
            }),
        )
        .unwrap();
        drop(admin);
        daemon.shutdown();
    }
    // A short idle timeout: the enabled listener must still keep the daemon alive.
    let mut child = env.spawn_papersd(300);
    assert!(wait_until(Duration::from_secs(20), || TcpStream::connect(
        ("127.0.0.1", port)
    )
    .is_ok()));
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        child.try_wait().unwrap().is_none(),
        "papersd idled out with the API enabled"
    );
    let resp = Conn::open(port).send(&Req::new("GET", format!("{API}/health")));
    assert_eq!(resp.status, 200);
    let resp = Conn::open(port).send(&Req::new("GET", format!("{API}/papers")));
    assert_eq!(resp.status, 401);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn timeouts_connection_and_concurrency_limits() {
    // Slow headers are cut off; connections beyond the limit are closed at once.
    let a = Api::start_with(|rest| {
        rest.limits.header_read_timeout = Duration::from_millis(300);
        rest.limits.max_connections = 2;
    });
    let mut slow = TcpStream::connect(("127.0.0.1", a.port)).unwrap();
    slow.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    slow.write_all(
        format!(
            "GET {API}/health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n",
            a.port
        )
        .as_bytes(),
    )
    .unwrap();
    let started = std::time::Instant::now();
    let mut buf = [0u8; 256];
    let n = slow.read(&mut buf).unwrap_or(0);
    let answer = String::from_utf8_lossy(&buf[..n]).into_owned();
    assert!(n == 0 || answer.starts_with("HTTP/1.1 408"), "{answer}");
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(slow);
    assert!(wait_until(Duration::from_secs(5), || {
        let mut first = Conn::open(a.port);
        let mut second = Conn::open(a.port);
        let health = Req::new("GET", format!("{API}/health"));
        if first.send(&health).status != 200 || second.send(&health).status != 200 {
            return false;
        }
        let mut third = TcpStream::connect(("127.0.0.1", a.port)).unwrap();
        third
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let _ = third.write_all(
            format!(
                "GET {API}/health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                a.port
            )
            .as_bytes(),
        );
        let mut buf = [0u8; 64];
        matches!(third.read(&mut buf), Ok(0) | Err(_))
    }));

    // Every request past its time limit ends as a documented 504 with a request id.
    let mut b = Api::start_with(|rest| rest.limits.request_timeout = Duration::from_millis(1));
    let (_, token) = b.token("slow", &[Scope::PapersRead]);
    let resp = b.call(
        "GET /papers",
        Req::new("GET", format!("{API}/papers")).token(&token),
    );
    assert_eq!((resp.status, resp.code().as_str()), (504, "timeout"));

    // No free request slot: 503 busy with Retry-After.
    let c = Api::start_with(|rest| rest.limits.max_in_flight = 0);
    let resp = c.call("GET /health", Req::new("GET", format!("{API}/health")));
    assert_eq!((resp.status, resp.code().as_str()), (503, "busy"));
    assert_eq!(resp.header("retry-after"), Some("1"));
}

/// Shutdown drains the REST side before the library worker exits and the lock is released:
/// a new daemon can take the library at once and serves the persisted API again.
#[test]
fn shutdown_with_the_api_enabled_releases_the_library_promptly() {
    let mut a = Api::start();
    let (_, token) = a.token("restart", &[Scope::PapersRead, Scope::PapersWrite]);
    create_paper(&a, &token, MARKER_TITLE, "10.1234/restart");
    // An idle keep-alive connection must not hold the daemon open.
    let mut idle = Conn::open(a.port);
    assert_eq!(
        idle.send(&Req::new("GET", format!("{API}/health"))).status,
        200
    );
    let started = std::time::Instant::now();
    a.daemon.take().unwrap().shutdown();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    let mut config = a.env.config();
    config.secret_store = Some(Arc::new(MemorySecretStore::default()));
    let again = start(config).expect("library lock released");
    assert!(wait_until(Duration::from_secs(10), || {
        TcpStream::connect(("127.0.0.1", a.port)).is_ok()
    }));
    let resp = a.send(&Req::new("GET", format!("{API}/papers")).token(&token));
    assert_eq!(resp.status, 200);
    assert!(resp.text().contains(MARKER_TITLE));
    drop(idle);
    again.shutdown();
}

/// Sends a JSON POST in two steps: the head with `Expect: 100-continue`, and the body only
/// after the server asked for it (i.e. after the request was authenticated and authorized).
/// `between` runs in that gap.
fn post_after_continue(
    port: u16,
    token: &Credential,
    path: &str,
    body: &Value,
    between: impl FnOnce(),
) -> Resp {
    let mut conn = Conn::open(port);
    let body = serde_json::to_vec(body).unwrap();
    let head = format!(
        "POST {API}{path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nExpect: 100-continue\r\n\r\n",
        body.len(),
        token = token.0,
    );
    conn.writer.write_all(head.as_bytes()).unwrap();
    let mut line = String::new();
    conn.reader.read_line(&mut line).unwrap();
    assert!(
        line.contains(" 100 "),
        "expected 100 Continue, got {line:?}"
    );
    loop {
        line.clear();
        conn.reader.read_line(&mut line).unwrap();
        if line.trim_end().is_empty() {
            break;
        }
    }
    between();
    conn.writer.write_all(&body).unwrap();
    conn.read_response()
}

/// An elevation checked when a bulk request arrived must not carry the batch once it ended:
/// every item is authorized again, so nothing runs on the ordinary burst allowance.
#[test]
fn bulk_batches_stop_when_the_elevation_ends_after_admission() {
    let mut a = Api::start();
    let (grant, token) = a.token("bulk", &issuable());
    let papers: Vec<String> = (0..3)
        .map(|i| {
            create_paper(
                &a,
                &token,
                &format!("Bulk {i}"),
                &format!("10.1234/bulk{i}"),
            )
        })
        .collect();
    let detail = |a: &Api, id: &str| {
        let resp = a.send(&Req::new("GET", format!("{API}/papers/{id}")).token(&token));
        assert_eq!(resp.status, 200, "{}", resp.text());
        resp.json()
    };
    let port = a.port;
    let stopped = |resp: &Resp| {
        assert_eq!(resp.status, 200, "{}", resp.text());
        let body = resp.json();
        assert_eq!(body["stopped"], "elevation_required", "{body}");
        let results = body["results"].as_array().unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0]["error"], "elevation_required");
        assert!(results[1..].iter().all(|r| r["error"] == "not_attempted"));
    };

    elevate(&mut a, grant, BulkKind::Delete);
    let resp = post_after_continue(
        port,
        &token,
        "/papers/bulk-delete",
        &json!({ "paper_ids": papers }),
        || {
            api(&mut a.admin, ApiCommand::EndElevation(grant)).unwrap();
        },
    );
    conforms(&a.doc, "POST /papers/bulk-delete", &resp);
    stopped(&resp);
    for id in &papers {
        assert_eq!(detail(&a, id)["paper"]["deleted_at"], Value::Null);
    }

    let collection = |a: &Api, name: &str| {
        let resp = a.send(
            &Req::new("POST", format!("{API}/collections"))
                .token(&token)
                .json(&json!({ "name": name })),
        );
        assert_eq!(resp.status, 201, "{}", resp.text());
        resp.json()["collection"]["id"].as_str().unwrap().to_owned()
    };
    let from = collection(&a, "From");
    let to = collection(&a, "To");
    for id in &papers {
        let resp = a.send(
            &Req::new("POST", format!("{API}/collections/{from}/papers"))
                .token(&token)
                .json(&json!({ "paper_id": id })),
        );
        assert_eq!(resp.status, 200, "{}", resp.text());
    }
    elevate(&mut a, grant, BulkKind::Move);
    let resp = post_after_continue(
        port,
        &token,
        &format!("/collections/{to}/papers/bulk-move"),
        &json!({ "paper_ids": papers, "from": from }),
        || {
            api(&mut a.admin, ApiCommand::EndElevation(grant)).unwrap();
        },
    );
    conforms(&a.doc, "POST /collections/{id}/papers/bulk-move", &resp);
    stopped(&resp);
    for id in &papers {
        let collections = detail(&a, id)["collections"].clone();
        let ids: Vec<&str> = collections
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec![from.as_str()], "{id} was not moved");
    }
}
