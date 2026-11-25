//! Tool failures as MCP `isError` results with a structured `error` object.
use crate::daemon::CallError;
use cm_protocol::{
    ipc::{Capability, CapabilitySet, Denial, HandshakeRejection, Operation, WireError},
    library::ServiceError,
};
use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{Map, Value, json};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ToolError {
    pub kind: &'static str,
    pub message: String,
    pub details: Map<String, Value>,
}

impl ToolError {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: Map::new(),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_input", message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }
    pub fn with(mut self, key: &str, value: Value) -> Self {
        self.details.insert(key.into(), value);
        self
    }

    pub fn into_result(self) -> CallToolResult {
        let mut error = Map::new();
        error.insert("kind".into(), Value::from(self.kind));
        error.insert("message".into(), Value::from(self.message.clone()));
        error.extend(self.details);
        let value = json!({ "error": Value::Object(error) });
        let mut result = CallToolResult::error(vec![ContentBlock::text(format!(
            "{}: {}\n{value}",
            self.kind, self.message
        ))]);
        result.structured_content = Some(value);
        result
    }

    /// Maps a daemon/transport failure to a caller-visible explanation. `is_write`: the
    /// command may change the library, so an unanswered one may still have been applied.
    pub fn from_call(error: CallError, library: &Path, is_write: bool) -> Self {
        let maybe_applied = "the change may have been applied; check (e.g. with get_paper or \
                             get_paper_annotations) before retrying";
        match error {
            CallError::Wire(WireError::Denied(denial)) => denied(denial, library),
            CallError::Wire(WireError::Service(e)) => service(e),
            CallError::Wire(WireError::Busy) => Self::new(
                "busy",
                "the library service is busy (too many requests or a locked index); retry shortly",
            ),
            CallError::Wire(WireError::TooLarge) => Self::new(
                "too_large",
                "the request or its result exceeds the library service's frame limit; request less",
            ),
            CallError::Wire(WireError::Cancelled) | CallError::Cancelled if is_write => Self::new(
                "cancelled",
                format!(
                    "the request was cancelled while a change was in progress; {maybe_applied}"
                ),
            )
            .with("may_have_been_applied", Value::Bool(true)),
            CallError::Wire(WireError::Cancelled) | CallError::Cancelled => {
                Self::new("cancelled", "the request was cancelled")
            }
            CallError::Wire(WireError::Unavailable) => Self::new(
                "unavailable",
                "the library service is shutting down or a worker is unavailable; retry",
            ),
            CallError::Lost if is_write => Self::new(
                "daemon_restarted",
                format!(
                    "the library service stopped or restarted before confirming the change; \
                     {maybe_applied}. The next call reconnects (starting it on demand)."
                ),
            )
            .with("may_have_been_applied", Value::Bool(true)),
            CallError::Lost => Self::new(
                "daemon_restarted",
                "the library service stopped or restarted before replying; the next call \
                 reconnects (starting it on demand). Retry; for a change, first check whether \
                 it was applied.",
            ),
            CallError::Connect(reason) => Self::new(
                "daemon_unavailable",
                format!("cannot reach or start the library service: {reason}"),
            ),
            CallError::Rejected(HandshakeRejection::Unauthenticated) => Self::new(
                "unauthenticated",
                format!(
                    "the library service rejected this MCP server's credential (revoked, expired \
                     or unknown). The library owner can issue a new one: {}",
                    grant_command(library, "")
                ),
            ),
            CallError::Rejected(reason) => Self::new(
                "daemon_rejected",
                format!("the library service refused the connection: {reason:?}"),
            ),
            CallError::Credential(reason) => Self::new("no_credential", reason),
            CallError::Timeout(limit) if is_write => Self::new(
                "timeout",
                format!(
                    "no reply from the library service within {} s while making a change; \
                     {maybe_applied}",
                    limit.as_secs()
                ),
            )
            .with("may_have_been_applied", Value::Bool(true)),
            CallError::Timeout(limit) => Self::new(
                "timeout",
                format!(
                    "no reply from the library service within {} s; it may be busy with a long \
                     import — retry later",
                    limit.as_secs()
                ),
            ),
        }
    }
}

fn service(error: ServiceError) -> ToolError {
    match error {
        ServiceError::InvalidInput(reason) => ToolError::invalid(reason),
        ServiceError::NotFound => ToolError::not_found("record not found (or it is in the trash)"),
        ServiceError::Conflict => ToolError::new(
            "conflict",
            "the record changed or conflicts with existing data; re-read it and retry",
        ),
        ServiceError::Unavailable => ToolError::new(
            "unavailable",
            "the library service is temporarily unavailable",
        ),
    }
}

pub fn grant_command(library: &Path, flags: &str) -> String {
    format!(
        "`papers mcp grant --library {}{flags}` (or `papers-mcp grant ...`)",
        library.display()
    )
}

/// The capabilities an operation needs, as (all-of, any-of) serde names. Computed through
/// the public `satisfied_by` predicate so this crate does not depend on core's table type.
pub fn required_capabilities(operation: Operation) -> (Vec<Capability>, Vec<Capability>) {
    let requirement = operation.requirement();
    let all: CapabilitySet = Capability::ALL.into_iter().collect();
    let needed: Vec<Capability> = Capability::ALL
        .into_iter()
        .filter(|c| {
            let mut without = all.clone();
            without.remove(c);
            !requirement.satisfied_by(&without)
        })
        .collect();
    let any: Vec<Capability> = if needed.is_empty() {
        Capability::ALL
            .into_iter()
            .filter(|c| requirement.satisfied_by(&CapabilitySet::from([*c])))
            .collect()
    } else {
        Vec::new()
    };
    (needed, any)
}

fn name<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn denied(denial: Denial, library: &Path) -> ToolError {
    match denial {
        Denial::Forbidden(operation) => {
            let (all, any) = required_capabilities(operation);
            // Capabilities an MCP grant can carry (see credential::Profile).
            let mcp_grantable = |c: &Capability| {
                matches!(
                    c,
                    Capability::LibraryRead
                        | Capability::JobsRead
                        | Capability::LibraryWrite
                        | Capability::NetworkLookup
                        | Capability::LocalFileWrite
                )
            };
            // All-of: every capability must be grantable. Any-of: offer the grantable ones.
            let caps: Vec<Capability> = if all.is_empty() {
                any.iter().copied().filter(mcp_grantable).collect()
            } else if all.iter().all(mcp_grantable) {
                all.clone()
            } else {
                Vec::new()
            };
            let write = caps
                .iter()
                .any(|c| matches!(c, Capability::LibraryWrite | Capability::NetworkLookup));
            let export = caps.contains(&Capability::LocalFileWrite);
            let shown: Vec<Capability> = if caps.is_empty() {
                if all.is_empty() {
                    any.clone()
                } else {
                    all.clone()
                }
            } else {
                caps.clone()
            };
            let hint = if write || export {
                let flags = match (write, export) {
                    (true, true) => " --write --export",
                    (true, false) => " --write",
                    _ => " --export",
                };
                format!(
                    "This MCP server is read-only unless the library owner approves a separate \
                     grant: {}. The running server uses the new credential on its next call.",
                    grant_command(library, flags)
                )
            } else {
                "This operation is not available to MCP connections.".to_owned()
            };
            ToolError::new(
                "permission_denied",
                format!(
                    "this MCP connection's grant does not allow {} (needs {}). {hint}",
                    name(&operation).as_str().unwrap_or("this operation"),
                    shown
                        .iter()
                        .filter_map(|c| name(c).as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                        .join(if all.is_empty() { " or " } else { " and " }),
                ),
            )
            .with("operation", name(&operation))
            .with("denial", Value::from("forbidden"))
            .with(
                "required_capabilities",
                json!({"all_of": all.iter().map(name).collect::<Vec<_>>(),
                       "any_of": any.iter().map(name).collect::<Vec<_>>()}),
            )
            .with("hint", Value::from(hint))
        }
        Denial::Revoked => ToolError::new(
            "grant_revoked",
            format!(
                "this MCP connection's grant was revoked by the library owner; nothing more is \
                 allowed until a new grant is approved: {}",
                grant_command(library, "")
            ),
        )
        .with("denial", Value::from("revoked")),
        Denial::Expired => ToolError::new(
            "grant_expired",
            format!(
                "this MCP connection's grant has expired; ask the library owner for a new one: {}",
                grant_command(library, "")
            ),
        )
        .with("denial", Value::from("expired")),
        Denial::Unauthenticated => ToolError::new(
            "unauthenticated",
            "the library service does not recognize this MCP server's credential",
        )
        .with("denial", Value::from("unauthenticated")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirements_are_reported_by_capability_name() {
        assert_eq!(
            required_capabilities(Operation::PaperCreate),
            (vec![Capability::LibraryWrite], vec![])
        );
        assert_eq!(
            required_capabilities(Operation::Export),
            (
                vec![Capability::LibraryRead, Capability::LocalFileWrite],
                vec![]
            )
        );
        let (all, any) = required_capabilities(Operation::ImportPreview);
        assert!(all.is_empty());
        assert_eq!(
            any,
            vec![Capability::LibraryWrite, Capability::CapturePreview]
        );
    }

    #[test]
    fn unanswered_writes_never_say_retry_later() {
        let library = Path::new("/l");
        for error in [
            CallError::Timeout(std::time::Duration::from_secs(5)),
            CallError::Cancelled,
            CallError::Wire(WireError::Cancelled),
            CallError::Lost,
        ] {
            let write = ToolError::from_call(error.clone(), library, true);
            assert!(
                write.message.contains("may have been applied"),
                "{error:?}: {}",
                write.message
            );
            assert!(!write.message.contains("retry later"), "{}", write.message);
            assert_eq!(write.details["may_have_been_applied"], true);
            let read = ToolError::from_call(error, library, false);
            assert!(!read.message.contains("may have been applied"));
        }
    }

    #[test]
    fn denials_explain_how_to_grant_write_access() {
        let library = Path::new("/home/u/lib");
        let e = ToolError::from_call(
            CallError::Wire(WireError::Denied(Denial::Forbidden(Operation::PaperUpdate))),
            library,
            false,
        );
        assert_eq!(e.kind, "permission_denied");
        assert_eq!(e.details["operation"], "paper_update");
        assert!(e.message.contains("--write"), "{}", e.message);
        assert!(e.message.contains("/home/u/lib"));
        let export = ToolError::from_call(
            CallError::Wire(WireError::Denied(Denial::Forbidden(Operation::Export))),
            library,
            false,
        );
        assert!(export.message.contains("--export"), "{}", export.message);
        // Any-of requirements offer only what an MCP grant can carry.
        let preview = ToolError::from_call(
            CallError::Wire(WireError::Denied(Denial::Forbidden(
                Operation::ImportPreview,
            ))),
            library,
            false,
        );
        assert!(preview.message.contains("--write"), "{}", preview.message);
        assert!(
            !preview.message.contains("capture_preview"),
            "{}",
            preview.message
        );
        let delete = ToolError::from_call(
            CallError::Wire(WireError::Denied(Denial::Forbidden(Operation::PaperTrash))),
            library,
            false,
        );
        assert!(delete.message.contains("not available to MCP"));
        let revoked = ToolError::from_call(
            CallError::Wire(WireError::Denied(Denial::Revoked)),
            library,
            false,
        );
        assert_eq!(revoked.kind, "grant_revoked");
        let result = revoked.into_result();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.unwrap()["error"]["kind"],
            "grant_revoked"
        );
    }
}
