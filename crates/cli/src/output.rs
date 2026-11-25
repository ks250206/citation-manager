//! Output contract: one JSON envelope (schema `papers.v1`) or human text on stdout, diagnostics
//! on stderr, semantic exit codes. See docs/ADR/cli.md for the full table.
use serde_json::{Value, json};
use std::io::Write;

/// Version of the stdout envelope. Additive fields keep it; incompatible changes bump it.
pub const SCHEMA: &str = "papers.v1";

/// Process exit codes. Stable: scripts may branch on them.
pub mod exit {
    pub const OK: i32 = 0;
    /// Unexpected/internal failure (also a cancelled request or a protocol error).
    pub const OTHER: i32 = 1;
    /// Bad command line or input rejected as invalid (by the CLI or the daemon).
    pub const USAGE: i32 = 2;
    pub const NOT_FOUND: i32 = 3;
    /// Conflict, duplicate needing a decision, unresolved metadata or missing confirmation.
    pub const CONFLICT: i32 = 4;
    /// Authentication or authorization refused (grant scope, revoked/expired, credential).
    pub const PERMISSION: i32 = 5;
    /// papersd could not be reached or started, or it reported itself unavailable.
    pub const DAEMON: i32 = 6;
    /// The requested feature is not available in this build (MCP stdio #29, REST #27).
    pub const UNAVAILABLE: i32 = 7;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Usage,
    InvalidInput,
    NotFound,
    Conflict,
    /// A duplicate candidate needs `--on-duplicate` or `--merge <id>`.
    Duplicate,
    /// A write needs `--yes` (or an interactive "y") and none was given.
    ConfirmationRequired,
    /// Metadata is missing (e.g. DOI lookup returned no title) and needs explicit input.
    Unresolved,
    PermissionDenied,
    DaemonUnavailable,
    FeatureUnavailable,
    Internal,
}
impl Kind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Duplicate => "duplicate",
            Self::ConfirmationRequired => "confirmation_required",
            Self::Unresolved => "unresolved",
            Self::PermissionDenied => "permission_denied",
            Self::DaemonUnavailable => "daemon_unavailable",
            Self::FeatureUnavailable => "feature_unavailable",
            Self::Internal => "internal",
        }
    }
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Usage | Self::InvalidInput => exit::USAGE,
            Self::NotFound => exit::NOT_FOUND,
            Self::Conflict | Self::Duplicate | Self::ConfirmationRequired | Self::Unresolved => {
                exit::CONFLICT
            }
            Self::PermissionDenied => exit::PERMISSION,
            Self::DaemonUnavailable => exit::DAEMON,
            Self::FeatureUnavailable => exit::UNAVAILABLE,
            Self::Internal => exit::OTHER,
        }
    }
}

/// A successful result: JSON `data`, its human rendering and non-fatal warnings.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub data: Value,
    pub text: Vec<String>,
    pub warnings: Vec<String>,
}
impl Report {
    pub fn new(data: Value) -> Self {
        Self {
            data,
            ..Self::default()
        }
    }
    pub fn line(mut self, line: impl Into<String>) -> Self {
        self.text.push(line.into());
        self
    }
}

/// A failure, optionally carrying the partial result (e.g. the preview that needs a decision).
#[derive(Debug)]
pub struct CliError {
    pub kind: Kind,
    pub message: String,
    pub report: Option<Report>,
}
impl CliError {
    pub fn new(kind: Kind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            report: None,
        }
    }
    pub fn with_report(mut self, report: Report) -> Self {
        self.report = Some(report);
        self
    }
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(Kind::Usage, message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(Kind::Internal, message)
    }
}
impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Text,
}

/// Writes the envelope (JSON) or text to `out`, diagnostics to `err`; returns the exit code.
pub fn emit(
    format: Format,
    command: &str,
    result: Result<Report, CliError>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let (report, error) = match result {
        Ok(report) => (Some(report), None),
        Err(mut e) => (e.report.take(), Some(e)),
    };
    if let Some(report) = &report {
        for warning in &report.warnings {
            let _ = writeln!(err, "papers: warning: {warning}");
        }
    }
    if let Some(e) = &error {
        let _ = writeln!(err, "papers: error: {}", e.message);
    }
    match format {
        Format::Json => {
            let mut envelope = json!({
                "schema": SCHEMA,
                "command": command,
                "ok": error.is_none(),
            });
            if let Some(report) = &report {
                envelope["data"] = report.data.clone();
                envelope["warnings"] = json!(report.warnings);
            } else {
                envelope["warnings"] = json!([]);
            }
            if let Some(e) = &error {
                envelope["error"] = json!({
                    "code": e.kind.code(),
                    "exit_code": e.kind.exit_code(),
                    "message": e.message,
                });
            }
            let _ = writeln!(
                out,
                "{}",
                serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".into())
            );
        }
        Format::Text => {
            if let Some(report) = &report {
                for line in &report.text {
                    let _ = writeln!(out, "{line}");
                }
            }
        }
    }
    let _ = out.flush();
    error.map_or(exit::OK, |e| e.kind.exit_code())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_envelope_is_stable_and_errors_carry_codes() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = emit(
            Format::Json,
            "list",
            Ok(Report::new(json!({"papers": []}))),
            &mut out,
            &mut err,
        );
        assert_eq!(code, 0);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["schema"], SCHEMA);
        assert_eq!(v["ok"], true);
        assert_eq!(v["data"]["papers"], json!([]));
        assert!(err.is_empty());

        let (mut out, mut err) = (Vec::new(), Vec::new());
        let failure = CliError::new(Kind::Duplicate, "exact duplicate")
            .with_report(Report::new(json!({"preview": 1})));
        assert_eq!(
            emit(Format::Json, "add doi", Err(failure), &mut out, &mut err),
            4
        );
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "duplicate");
        assert_eq!(v["error"]["exit_code"], 4);
        assert_eq!(v["data"]["preview"], 1);
        assert!(String::from_utf8(err).unwrap().contains("exact duplicate"));
    }

    #[test]
    fn exit_codes_are_distinct_per_class() {
        let codes = [
            (Kind::Usage, 2),
            (Kind::InvalidInput, 2),
            (Kind::NotFound, 3),
            (Kind::Conflict, 4),
            (Kind::Duplicate, 4),
            (Kind::ConfirmationRequired, 4),
            (Kind::Unresolved, 4),
            (Kind::PermissionDenied, 5),
            (Kind::DaemonUnavailable, 6),
            (Kind::FeatureUnavailable, 7),
            (Kind::Internal, 1),
        ];
        for (kind, code) in codes {
            assert_eq!(kind.exit_code(), code, "{kind:?}");
        }
    }

    #[test]
    fn text_mode_keeps_stdout_for_data_only() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut report = Report::new(json!({})).line("hello");
        report.warnings.push("careful".into());
        assert_eq!(emit(Format::Text, "x", Ok(report), &mut out, &mut err), 0);
        assert_eq!(String::from_utf8(out).unwrap(), "hello\n");
        assert!(String::from_utf8(err).unwrap().contains("careful"));
    }
}
