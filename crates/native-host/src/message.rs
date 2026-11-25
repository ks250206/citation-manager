//! Browser <-> host message contract, version 1 (see docs/ADR/native-messaging.md).
//!
//! Request:  `{"v": 1, "id": "<client id>", "type": "<type>", ...fields}`
//! Response: `{"v": 1, "id": <echo or null>, "ok": true,  "type": "<type>", "result": {...}}`
//!       or  `{"v": 1, "id": <echo or null>, "ok": false, "error": {"code", "message", "retryable", "detail"?}}`
//!
//! The set of types is closed and each maps to exactly one capture/destination command; the
//! host is not a generic IPC proxy. Unknown fields are ignored (additive within a version).
use cm_protocol::{
    capture::{CaptureCommit, CaptureRequest},
    import::{ImportCommit, ImportDecision, PreviewId},
    jobs::JobId,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Version of this browser-facing contract (independent of the daemon IPC version).
pub const HOST_PROTOCOL_VERSION: u32 = 1;
/// Request ids are echoed verbatim, so they are kept short and printable.
pub const MAX_ID_LEN: usize = 64;
/// A capture preview has one candidate; a few decisions bound the commit message.
pub const MAX_DECISIONS: usize = 16;
/// Daemon-supplied error text forwarded to the browser is cut at this many characters.
const MAX_ERROR_MESSAGE: usize = 512;

pub const TYPES: [&str; 7] = [
    "hello",
    "destinations",
    "capture.preview",
    "capture.commit",
    "capture.cancel",
    "capture.pdf_job",
    "capture.retry_pdf",
];

#[derive(Debug, Clone)]
pub enum Request {
    /// Installation check; never contacts (or starts) the daemon.
    Hello,
    /// Collection/tag names and IDs for the destination picker (`DestinationRead`).
    Destinations,
    Preview(Box<CaptureRequest>),
    Commit(CaptureCommit),
    Cancel(PreviewId),
    PdfJob(JobId),
    RetryPdf(JobId),
}
impl Request {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::Destinations => "destinations",
            Self::Preview(_) => "capture.preview",
            Self::Commit(_) => "capture.commit",
            Self::Cancel(_) => "capture.cancel",
            Self::PdfJob(_) => "capture.pdf_job",
            Self::RetryPdf(_) => "capture.retry_pdf",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Incoming {
    pub id: Value,
    pub request: Request,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Not valid UTF-8 JSON, not an object, bad id, or fields of the wrong shape.
    BadMessage,
    UnsupportedVersion,
    UnknownType,
    /// The request is over the host or daemon size limit (the host exits after a too-large frame).
    TooLarge,
    /// The result would exceed the 1 MiB browser limit.
    ResponseTooLarge,
    /// No daemon-issued capture grant is stored: run `papers-native-host pair`.
    NotPaired,
    /// The stored grant is unknown, revoked or expired: pair again.
    Unauthorized,
    /// The grant does not allow this operation (e.g. Merge without LibraryWrite).
    Denied,
    InvalidInput,
    /// The library changed or the preview expired: preview again.
    Conflict,
    NotFound,
    Busy,
    Cancelled,
    /// The daemon could not be started/reached or went away. Retryable.
    DaemonUnavailable,
    /// The daemon refused the connection (other library, peer check, version).
    DaemonRejected,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}
impl HostError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let retryable = matches!(
            code,
            ErrorCode::Busy | ErrorCode::DaemonUnavailable | ErrorCode::Conflict
        );
        let mut message: String = message.into();
        if message.chars().count() > MAX_ERROR_MESSAGE {
            message = message.chars().take(MAX_ERROR_MESSAGE).collect::<String>() + "…";
        }
        Self {
            code,
            message,
            retryable,
            detail: None,
        }
    }
    pub fn with_detail(mut self, detail: Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

fn bad(message: &str) -> HostError {
    HostError::new(ErrorCode::BadMessage, message)
}

fn valid_id(id: &Value) -> bool {
    match id {
        Value::String(s) => {
            !s.is_empty()
                && s.len() <= MAX_ID_LEN
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
        }
        Value::Number(n) => n.is_u64(),
        Value::Null => true,
        _ => false,
    }
}

#[derive(Deserialize)]
struct CommitFields {
    preview_id: PreviewId,
    decisions: Vec<ImportDecision>,
    #[serde(default)]
    pdf: Option<usize>,
}
#[derive(Deserialize)]
struct PreviewFields {
    capture: CaptureRequest,
}
#[derive(Deserialize)]
struct PreviewIdFields {
    preview_id: PreviewId,
}
#[derive(Deserialize)]
struct JobFields {
    job: JobId,
}

/// Parses one message body. On error, returns the request id when it could be recovered so
/// the browser can correlate the error.
pub fn parse(body: &[u8]) -> Result<Incoming, (Value, HostError)> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| (Value::Null, bad("message is not valid UTF-8 JSON")))?;
    let Value::Object(mut map) = value else {
        return Err((Value::Null, bad("message must be a JSON object")));
    };
    let id = map.remove("id").unwrap_or(Value::Null);
    if !valid_id(&id) {
        return Err((
            Value::Null,
            bad("id must be null, a non-negative integer or 1..=64 of [A-Za-z0-9._:-]"),
        ));
    }
    let fail = |e: HostError| (id.clone(), e);
    match map.remove("v") {
        Some(Value::Number(n)) if n.as_u64() == Some(u64::from(HOST_PROTOCOL_VERSION)) => {}
        Some(Value::Number(_)) => {
            return Err(fail(
                HostError::new(
                    ErrorCode::UnsupportedVersion,
                    "unsupported protocol version",
                )
                .with_detail(json!({ "supported": [HOST_PROTOCOL_VERSION] })),
            ));
        }
        _ => return Err(fail(bad("v (protocol version) is required"))),
    }
    let kind = match map.remove("type") {
        Some(Value::String(kind)) => kind,
        _ => return Err(fail(bad("type is required"))),
    };
    let fields = |map: Map<String, Value>| Value::Object(map);
    let shape = |e: serde_json::Error| fail(bad(&format!("invalid {kind} fields: {e}")));
    let request = match kind.as_str() {
        "hello" => Request::Hello,
        "destinations" => Request::Destinations,
        "capture.preview" => {
            let f: PreviewFields = serde_json::from_value(fields(map)).map_err(shape)?;
            Request::Preview(Box::new(f.capture))
        }
        "capture.commit" => {
            let f: CommitFields = serde_json::from_value(fields(map)).map_err(shape)?;
            if f.decisions.is_empty() || f.decisions.len() > MAX_DECISIONS {
                return Err(fail(bad("decisions must have 1..=16 entries")));
            }
            Request::Commit(CaptureCommit {
                commit: ImportCommit {
                    preview_id: f.preview_id,
                    decisions: f.decisions,
                },
                pdf: f.pdf,
            })
        }
        "capture.cancel" => {
            let f: PreviewIdFields = serde_json::from_value(fields(map)).map_err(shape)?;
            Request::Cancel(f.preview_id)
        }
        "capture.pdf_job" | "capture.retry_pdf" => {
            let f: JobFields = serde_json::from_value(fields(map)).map_err(shape)?;
            if kind == "capture.pdf_job" {
                Request::PdfJob(f.job)
            } else {
                Request::RetryPdf(f.job)
            }
        }
        _ => {
            return Err(fail(
                HostError::new(ErrorCode::UnknownType, "unknown message type")
                    .with_detail(json!({ "supported": TYPES })),
            ));
        }
    };
    Ok(Incoming { id, request })
}

pub fn success(id: &Value, kind: &str, result: Value) -> Value {
    json!({ "v": HOST_PROTOCOL_VERSION, "id": id, "ok": true, "type": kind, "result": result })
}

pub fn failure(id: &Value, error: &HostError) -> Value {
    json!({ "v": HOST_PROTOCOL_VERSION, "id": id, "ok": false, "error": error })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_protocol::import::ImportAction;

    fn parse_value(v: Value) -> Result<Incoming, (Value, HostError)> {
        parse(&serde_json::to_vec(&v).unwrap())
    }
    fn code(v: Value) -> (Value, ErrorCode) {
        let (id, e) = parse_value(v).unwrap_err();
        (id, e.code)
    }

    #[test]
    fn every_type_maps_to_exactly_one_request() {
        let job = "0192f0b2-0000-7000-8000-000000000001";
        let cases = [
            (json!({"v":1,"id":"a","type":"hello"}), "hello"),
            (
                json!({"v":1,"id":"a","type":"destinations"}),
                "destinations",
            ),
            (
                json!({"v":1,"id":"a","type":"capture.preview",
                       "capture":{"page_url":"https://example.org/p","offline":true}}),
                "capture.preview",
            ),
            (
                json!({"v":1,"id":7,"type":"capture.commit","preview_id":"p1",
                       "decisions":[{"candidate_index":0,"action":"Create"}],"pdf":0}),
                "capture.commit",
            ),
            (
                json!({"v":1,"type":"capture.cancel","preview_id":"p1"}),
                "capture.cancel",
            ),
            (
                json!({"v":1,"type":"capture.pdf_job","job":job}),
                "capture.pdf_job",
            ),
            (
                json!({"v":1,"type":"capture.retry_pdf","job":job}),
                "capture.retry_pdf",
            ),
        ];
        for (value, kind) in cases {
            assert_eq!(parse_value(value).unwrap().request.type_name(), kind);
        }
        let Request::Commit(commit) = parse_value(json!({"v":1,"type":"capture.commit",
            "preview_id":"p1","decisions":[{"candidate_index":0,"action":"Skip"}]}))
        .unwrap()
        .request
        else {
            panic!()
        };
        assert_eq!(commit.pdf, None);
        assert_eq!(commit.commit.decisions[0].action, ImportAction::Skip);
    }

    #[test]
    fn version_type_id_and_shape_errors() {
        assert_eq!(
            code(json!({"v":2,"id":"x","type":"hello"})),
            (json!("x"), ErrorCode::UnsupportedVersion)
        );
        assert_eq!(
            code(json!({"id":"x","type":"hello"})).1,
            ErrorCode::BadMessage
        );
        assert_eq!(
            code(json!({"v":"1","type":"hello"})).1,
            ErrorCode::BadMessage
        );
        assert_eq!(code(json!({"v":1})).1, ErrorCode::BadMessage);
        // Generic proxying is not offered: daemon command names are unknown types.
        for kind in [
            "Library",
            "grants.issue",
            "jobs.submit",
            "import.commit",
            "status",
        ] {
            assert_eq!(
                code(json!({"v":1,"id":"x","type":kind})),
                (json!("x"), ErrorCode::UnknownType)
            );
        }
        for id in [
            json!(""),
            json!("a b"),
            json!("x".repeat(65)),
            json!(-1),
            json!([1]),
        ] {
            assert_eq!(
                code(json!({"v":1,"id":id,"type":"hello"})),
                (Value::Null, ErrorCode::BadMessage)
            );
        }
        assert_eq!(
            code(json!({"v":1,"id":"x","type":"capture.preview","capture":{"title":"t"}})),
            (json!("x"), ErrorCode::BadMessage)
        );
        assert_eq!(
            code(json!({"v":1,"type":"capture.commit","preview_id":"p","decisions":[]})).1,
            ErrorCode::BadMessage
        );
        assert_eq!(
            code(json!({"v":1,"type":"capture.pdf_job","job":"not-a-uuid"})).1,
            ErrorCode::BadMessage
        );
        assert_eq!(parse(b"[1,2]").unwrap_err().1.code, ErrorCode::BadMessage);
        assert_eq!(
            parse(b"{\"v\":1,").unwrap_err().1.code,
            ErrorCode::BadMessage
        );
        assert_eq!(
            parse(&[0xff, 0xfe]).unwrap_err().1.code,
            ErrorCode::BadMessage
        );
    }

    #[test]
    fn page_forwarded_authority_fields_are_ignored() {
        let incoming = parse_value(json!({"v":1,"type":"capture.preview","capabilities":["library_write"],
            "client_kind":"desktop","capture":{"page_url":"https://e.org","client_kind":"desktop"}}))
        .unwrap();
        assert!(matches!(incoming.request, Request::Preview(_)));
    }

    #[test]
    fn envelopes_and_error_text_bounds() {
        let ok = success(&json!("a"), "hello", json!({"x":1}));
        assert_eq!(ok["ok"], json!(true));
        assert_eq!(ok["v"], json!(1));
        let e = HostError::new(ErrorCode::DaemonUnavailable, "x".repeat(2000));
        assert!(e.retryable);
        assert_eq!(e.message.chars().count(), MAX_ERROR_MESSAGE + 1);
        let f = failure(&Value::Null, &e);
        assert_eq!(f["error"]["code"], json!("daemon_unavailable"));
        assert!(f["error"].get("detail").is_none());
        assert!(!HostError::new(ErrorCode::Denied, "no").retryable);
    }
}
