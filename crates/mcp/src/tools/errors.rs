//! Tool failures as MCP `isError` results with a structured `error` object.
use crate::{credential, daemon::CallError};
use cm_protocol::{
    ipc::{Capability, Denial, HandshakeRejection, Operation, Requirement, WireError},
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

/// The capabilities an operation needs, as alternatives: any one alternative is enough and
/// each needs all of its capabilities. `All` is one alternative, `Any` one per capability
/// and #26's `AnyOf` (the #25 bundle or the fine-grained scope capabilities) is kept as is.
pub fn required_capabilities(operation: Operation) -> Vec<Vec<Capability>> {
    match operation.requirement() {
        Requirement::All(needed) => vec![needed.to_vec()],
        Requirement::Any(options) => options.iter().map(|c| vec![*c]).collect(),
        Requirement::AnyOf(alternatives) => alternatives.iter().map(|a| a.to_vec()).collect(),
    }
}

fn name<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn names(capabilities: &[Capability]) -> Vec<String> {
    capabilities
        .iter()
        .filter_map(|c| name(c).as_str().map(str::to_owned))
        .collect()
}

/// `a and b`, or `a or (b and c)` for several alternatives.
fn describe(alternatives: &[Vec<Capability>]) -> String {
    let several = alternatives.len() > 1;
    alternatives
        .iter()
        .map(|alternative| {
            let all = names(alternative).join(" and ");
            if several && alternative.len() > 1 {
                format!("({all})")
            } else {
                all
            }
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

/// The `grant` flags an alternative needs beyond the read-only profile, or `None` when an
/// MCP grant can never carry it (see `credential::Profile`).
fn grant_flags(alternative: &[Capability]) -> Option<(bool, bool)> {
    let (write, export) = (credential::write_extra(), credential::export_extra());
    let read = credential::read_only();
    let mut flags = (false, false);
    for capability in alternative {
        if write.contains(capability) {
            flags.0 = true;
        } else if export.contains(capability) {
            flags.1 = true;
        } else if !read.contains(capability) {
            return None;
        }
    }
    Some(flags)
}

fn denied(denial: Denial, library: &Path) -> ToolError {
    match denial {
        Denial::Forbidden(operation) => {
            let alternatives = required_capabilities(operation);
            // Offer only the alternatives an MCP grant can carry, cheapest grant first.
            let mut grantable: Vec<(&Vec<Capability>, (bool, bool))> = alternatives
                .iter()
                .filter_map(|a| grant_flags(a).map(|flags| (a, flags)))
                .collect();
            grantable.sort_by_key(|(_, (write, export))| u8::from(*write) + u8::from(*export));
            let shown: Vec<Vec<Capability>> = if grantable.is_empty() {
                alternatives.clone()
            } else {
                grantable.iter().map(|(a, _)| (*a).clone()).collect()
            };
            let hint = match grantable.first() {
                None => "This operation is not available to MCP connections.".to_owned(),
                Some((_, flags)) => {
                    let flags = match flags {
                        (true, true) => " --write --export",
                        (true, false) => " --write",
                        (false, true) => " --export",
                        (false, false) => "",
                    };
                    format!(
                        "This MCP server is read-only unless the library owner approves a \
                         separate grant: {}. The running server uses the new credential on its \
                         next call.",
                        grant_command(library, flags)
                    )
                }
            };
            // all_of: needed by every alternative; any_of: enough on its own (the #29 shape);
            // alternatives: the complete requirement.
            let all_of: Vec<Capability> = alternatives
                .first()
                .map(|first| {
                    first
                        .iter()
                        .copied()
                        .filter(|c| alternatives.iter().all(|a| a.contains(c)))
                        .collect()
                })
                .unwrap_or_default();
            let any_of: Vec<Capability> = alternatives
                .iter()
                .filter(|a| a.len() == 1)
                .map(|a| a[0])
                .collect();
            ToolError::new(
                "permission_denied",
                format!(
                    "this MCP connection's grant does not allow {} (needs {}). {hint}",
                    name(&operation).as_str().unwrap_or("this operation"),
                    describe(&shown),
                ),
            )
            .with("operation", name(&operation))
            .with("denial", Value::from("forbidden"))
            .with(
                "required_capabilities",
                json!({"all_of": all_of.iter().map(name).collect::<Vec<_>>(),
                       "any_of": any_of.iter().map(name).collect::<Vec<_>>(),
                       "alternatives": alternatives.iter()
                           .map(|a| a.iter().map(name).collect::<Vec<_>>())
                           .collect::<Vec<_>>()}),
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
            vec![vec![Capability::LibraryWrite]]
        );
        // #26: the #25 bundle (LibraryRead) or the fine-grained `papers:read` capability,
        // each with an arbitrary path (LocalFileWrite) or an allowed export folder.
        assert_eq!(
            required_capabilities(Operation::Export),
            vec![
                vec![Capability::LibraryRead, Capability::LocalFileWrite],
                vec![Capability::MetadataRead, Capability::LocalFileWrite],
                vec![Capability::MetadataRead, Capability::ExportAllowedDir],
            ]
        );
        assert_eq!(
            required_capabilities(Operation::ImportPreview),
            vec![
                vec![Capability::LibraryWrite],
                vec![Capability::CapturePreview]
            ]
        );
        // #26 split annotation writes from LibraryWrite.
        assert_eq!(
            required_capabilities(Operation::AnnotationWrite),
            vec![vec![Capability::AnnotationsWrite]]
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
        assert!(!export.message.contains("--write"), "{}", export.message);
        // Only the any-of alternative an MCP grant can carry is offered.
        assert!(
            export
                .message
                .contains("needs library_read and local_file_write)"),
            "{}",
            export.message
        );
        assert!(!export.message.contains("export_allowed_dir"));
        assert_eq!(
            export.details["required_capabilities"]["alternatives"],
            json!([
                ["library_read", "local_file_write"],
                ["metadata_read", "local_file_write"],
                ["metadata_read", "export_allowed_dir"]
            ])
        );
        // add_annotation is part of the write grant (#26: AnnotationsWrite).
        let annotate = ToolError::from_call(
            CallError::Wire(WireError::Denied(Denial::Forbidden(
                Operation::AnnotationWrite,
            ))),
            library,
            false,
        );
        assert!(annotate.message.contains("--write"), "{}", annotate.message);
        assert!(
            annotate.message.contains("needs annotations_write)"),
            "{}",
            annotate.message
        );
        assert_eq!(
            annotate.details["required_capabilities"]["all_of"],
            json!(["annotations_write"])
        );
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
