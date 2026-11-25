//! Standard REST errors: `{"error":{"code","message","details"?,"request_id"}}`.
//!
//! Handlers return `ApiError`; its response carries the error as an extension and the edge
//! middleware serializes it together with the request id. Responses that did not come from
//! an `ApiError` (framework rejections, unknown routes, wrong methods) get a generic body
//! derived from the status there, so every error has the same shape.
use crate::ipc::gateway::CallError;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use cm_core::{
    ServiceError,
    policy::{Denial, Operation},
};
use cm_protocol::ipc::WireError;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: None,
        }
    }
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
    pub fn unauthenticated() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "a valid bearer token is required (missing, unknown, revoked or expired token)",
        )
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_input", message)
    }
    pub fn invalid_json(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_json", message)
    }
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "resource not found")
    }
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "unexpected daemon reply",
        )
    }
    pub fn timeout() -> Self {
        Self::new(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "the request did not finish within its time limit",
        )
    }
    pub fn busy() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "too many concurrent requests; retry shortly",
        )
    }
    pub fn forbidden(operation: Operation) -> Self {
        match operation {
            Operation::BulkTrash | Operation::BulkMove => Self::new(
                StatusCode::FORBIDDEN,
                "elevation_required",
                "bulk limit reached: this needs a time-limited elevation approved by the owner",
            ),
            Operation::Export => Self::new(
                StatusCode::FORBIDDEN,
                "export_folder_not_allowed",
                "exports may only be written into an allowed export folder",
            ),
            _ => Self::scope_forbidden(),
        }
    }
    pub fn scope_forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "the token's scopes do not allow this request",
        )
    }
    pub fn from_denial(denial: Denial) -> Self {
        match denial {
            Denial::Unauthenticated | Denial::Revoked | Denial::Expired => Self::unauthenticated(),
            Denial::Forbidden(op) => Self::forbidden(op),
        }
    }
    pub fn from_wire(error: WireError) -> Self {
        match error {
            WireError::Denied(d) => Self::from_denial(d),
            WireError::Service(ServiceError::InvalidInput(reason)) => Self::invalid(reason),
            WireError::Service(ServiceError::NotFound) => Self::not_found(),
            WireError::Service(ServiceError::Conflict) => Self::new(
                StatusCode::CONFLICT,
                "conflict",
                "the request conflicts with existing data or state",
            ),
            WireError::Service(ServiceError::Unavailable) | WireError::Unavailable => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "the service is unavailable",
            ),
            WireError::Busy => Self::busy(),
            WireError::TooLarge => Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "the request or its result exceeds a size limit",
            ),
            WireError::Cancelled => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "cancelled",
                "the request was cancelled",
            ),
        }
    }
    pub fn from_call(error: CallError) -> Self {
        match error {
            CallError::Wire(e) => Self::from_wire(e),
            CallError::TimedOut => Self::timeout(),
        }
    }
    /// Generic error for a status produced outside the handlers.
    pub fn for_status(status: StatusCode) -> Self {
        let (code, message) = match status {
            StatusCode::NOT_FOUND => ("not_found", "no such endpoint"),
            StatusCode::METHOD_NOT_ALLOWED => {
                ("method_not_allowed", "method not allowed for this endpoint")
            }
            StatusCode::PAYLOAD_TOO_LARGE => ("too_large", "request body too large"),
            StatusCode::UNSUPPORTED_MEDIA_TYPE => (
                "unsupported_media_type",
                "request body must be application/json",
            ),
            StatusCode::URI_TOO_LONG => ("uri_too_long", "request URI too long"),
            s if s.is_server_error() => ("internal", "internal error"),
            _ => ("bad_request", "malformed request"),
        };
        Self::new(status, code, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = self.status.into_response();
        response.extensions_mut().insert(self);
        response
    }
}

impl From<ServiceError> for ApiError {
    fn from(e: ServiceError) -> Self {
        Self::from_wire(WireError::Service(e))
    }
}
impl From<WireError> for ApiError {
    fn from(e: WireError) -> Self {
        Self::from_wire(e)
    }
}
impl From<CallError> for ApiError {
    fn from(e: CallError) -> Self {
        Self::from_call(e)
    }
}
