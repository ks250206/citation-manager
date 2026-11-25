//! The 12 MCP tools of DESIGN §14.2, each a thin composition of #25 IPC commands.
//!
//! Contract shared by every tool:
//! - Arguments are validated against the tool's published `inputSchema` (count limits,
//!   page ranges, character caps) before any daemon call; violations are tool errors
//!   (`isError: true`, `error.kind = "invalid_input"`), never JSON-RPC protocol errors.
//! - Results are structured JSON (`structuredContent`, mirrored as one text block) whose
//!   items carry a `source` block: paper_id, doi, title, authors, year, attachment_id,
//!   page_index, page_label and an internal `uri`, next to the item's `text`.
//! - Authorization is the daemon's: the server presents its credential and forwards each
//!   command; a daemon `Denied` becomes a `permission_denied`/`grant_revoked` tool error that
//!   names the operation, the capability it needs and how the owner grants it.
mod errors;
mod import;
mod read;
pub mod source;
mod write;

pub use errors::ToolError;

use crate::{daemon::DaemonClient, schema};
use cm_protocol::ipc::{Command, Credential, Reply};
use rmcp::model::{CallToolResult, ContentBlock, Tool, ToolAnnotations};
use serde_json::{Map, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Tool names exactly as DESIGN §14.2 lists them.
pub const TOOL_NAMES: [&str; 12] = [
    "search_papers",
    "get_paper",
    "get_paper_fulltext",
    "get_paper_annotations",
    "add_paper_by_doi",
    "add_paper_from_url",
    "update_paper",
    "create_collection",
    "move_paper_to_collection",
    "add_tag_to_paper",
    "add_annotation",
    "export_citations",
];

/// Hard ceiling on one serialized tool result (the per-tool defaults stay well below it;
/// a request at the maximum caps that does not fit gets a `too_large` tool error).
pub const MAX_RESULT_BYTES: usize = 1024 * 1024;

/// One tool invocation: the daemon client, the MCP request's cancellation token and the
/// credential resolved once for the whole call, so a composite tool (several IPC commands)
/// acts under a single grant even if the credential file is replaced meanwhile.
pub struct Ctx<'a> {
    pub daemon: &'a DaemonClient,
    pub cancel: &'a CancellationToken,
    pub credential: Credential,
}
impl Ctx<'_> {
    pub async fn call(&self, command: Command) -> Result<Reply, ToolError> {
        // Commands that may change the library (a DOI lookup or import preview does not).
        let is_write = !crate::daemon::is_read_only(&command)
            && !matches!(
                command,
                Command::Resolve { .. }
                    | Command::Import(cm_protocol::exchange::ImportRequestUi::Preview(_))
            );
        self.daemon
            .call(command, &self.credential, self.cancel)
            .await
            .map_err(|e| ToolError::from_call(e, self.daemon.library(), is_write))
    }
}

pub struct ToolSpec {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// MCP hint only: authorization is enforced by papersd, not by this flag.
    pub read_only: bool,
    /// MCP `destructiveHint` (meaningful for write tools only): may it overwrite or remove
    /// existing data? `None` for read-only tools.
    pub destructive: Option<bool>,
    pub idempotent: bool,
    /// Talks to outside services (DOI providers).
    pub open_world: bool,
    pub schema: fn() -> Value,
}

pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "search_papers",
            title: "Search papers",
            description: read::SEARCH_DESCRIPTION,
            read_only: true,
            destructive: None,
            idempotent: true,
            open_world: false,
            schema: read::search_schema,
        },
        ToolSpec {
            name: "get_paper",
            title: "Get paper",
            description: read::GET_PAPER_DESCRIPTION,
            read_only: true,
            destructive: None,
            idempotent: true,
            open_world: false,
            schema: read::get_paper_schema,
        },
        ToolSpec {
            name: "get_paper_fulltext",
            title: "Get paper full text",
            description: read::FULLTEXT_DESCRIPTION,
            read_only: true,
            destructive: None,
            idempotent: true,
            open_world: false,
            schema: read::fulltext_schema,
        },
        ToolSpec {
            name: "get_paper_annotations",
            title: "Get paper annotations",
            description: read::ANNOTATIONS_DESCRIPTION,
            read_only: true,
            destructive: None,
            idempotent: true,
            open_world: false,
            schema: read::annotations_schema,
        },
        ToolSpec {
            name: "add_paper_by_doi",
            title: "Add paper by DOI",
            description: import::DOI_DESCRIPTION,
            read_only: false,
            destructive: Some(true),
            idempotent: false,
            open_world: true,
            schema: import::doi_schema,
        },
        ToolSpec {
            name: "add_paper_from_url",
            title: "Add paper from a web page",
            description: import::URL_DESCRIPTION,
            read_only: false,
            destructive: Some(true),
            idempotent: false,
            open_world: true,
            schema: import::url_schema,
        },
        ToolSpec {
            name: "update_paper",
            title: "Update paper metadata",
            description: write::UPDATE_DESCRIPTION,
            read_only: false,
            destructive: Some(true),
            idempotent: true,
            open_world: false,
            schema: write::update_schema,
        },
        ToolSpec {
            name: "create_collection",
            title: "Create collection",
            description: write::COLLECTION_DESCRIPTION,
            read_only: false,
            destructive: Some(false),
            idempotent: true,
            open_world: false,
            schema: write::collection_schema,
        },
        ToolSpec {
            name: "move_paper_to_collection",
            title: "Add or move a paper to a collection",
            description: write::MOVE_DESCRIPTION,
            read_only: false,
            destructive: Some(true),
            idempotent: true,
            open_world: false,
            schema: write::move_schema,
        },
        ToolSpec {
            name: "add_tag_to_paper",
            title: "Tag a paper",
            description: write::TAG_DESCRIPTION,
            read_only: false,
            destructive: Some(false),
            idempotent: true,
            open_world: false,
            schema: write::tag_schema,
        },
        ToolSpec {
            name: "add_annotation",
            title: "Add annotation",
            description: write::ANNOTATION_DESCRIPTION,
            read_only: false,
            destructive: Some(false),
            idempotent: false,
            open_world: false,
            schema: write::annotation_schema,
        },
        ToolSpec {
            name: "export_citations",
            title: "Export citations",
            description: write::EXPORT_DESCRIPTION,
            read_only: false,
            destructive: Some(false),
            idempotent: true,
            open_world: false,
            schema: write::export_schema,
        },
    ]
}

/// `tools/list` entries. Every tool is listed regardless of the grant (the grant can change
/// while the server runs); write tools say in their description that they need approval.
pub fn definitions() -> Vec<Tool> {
    specs()
        .into_iter()
        .map(|spec| {
            let Value::Object(schema) = (spec.schema)() else {
                unreachable!("tool schemas are objects")
            };
            let mut annotations = ToolAnnotations::with_title(spec.title)
                .read_only(spec.read_only)
                .idempotent(spec.idempotent)
                .open_world(spec.open_world);
            if let Some(destructive) = spec.destructive {
                annotations = annotations.destructive(destructive);
            }
            Tool::new(spec.name, spec.description, Arc::new(schema))
                .with_title(spec.title)
                .with_annotations(annotations)
        })
        .collect()
}

pub fn is_tool(name: &str) -> bool {
    TOOL_NAMES.contains(&name)
}

/// Runs one tool call to a `CallToolResult` (tool failures are `isError` results).
pub async fn call(
    daemon: &DaemonClient,
    name: &str,
    arguments: Map<String, Value>,
    cancel: &CancellationToken,
) -> CallToolResult {
    let Some(spec) = specs().into_iter().find(|s| s.name == name) else {
        return ToolError::invalid(format!("unknown tool {name}")).into_result();
    };
    let arguments = Value::Object(arguments);
    if let Err(message) = schema::validate(&(spec.schema)(), &arguments) {
        return ToolError::invalid(message).into_result();
    }
    let Value::Object(map) = arguments else {
        unreachable!("validated as an object")
    };
    let args = Args(map);
    let credential = match daemon.load_credential() {
        Ok(credential) => credential,
        Err(e) => return ToolError::from_call(e, daemon.library(), false).into_result(),
    };
    let ctx = Ctx {
        daemon,
        cancel,
        credential,
    };
    match dispatch(spec.name, &ctx, &args).await {
        Ok(value) => success(value),
        Err(e) => e.into_result(),
    }
}

async fn dispatch(name: &str, ctx: &Ctx<'_>, args: &Args) -> Result<Value, ToolError> {
    match name {
        "search_papers" => read::search_papers(ctx, args).await,
        "get_paper" => read::get_paper(ctx, args).await,
        "get_paper_fulltext" => read::get_paper_fulltext(ctx, args).await,
        "get_paper_annotations" => read::get_paper_annotations(ctx, args).await,
        "add_paper_by_doi" => import::add_paper_by_doi(ctx, args).await,
        "add_paper_from_url" => import::add_paper_from_url(ctx, args).await,
        "update_paper" => write::update_paper(ctx, args).await,
        "create_collection" => write::create_collection(ctx, args).await,
        "move_paper_to_collection" => write::move_paper_to_collection(ctx, args).await,
        "add_tag_to_paper" => write::add_tag_to_paper(ctx, args).await,
        "add_annotation" => write::add_annotation(ctx, args).await,
        "export_citations" => write::export_citations(ctx, args).await,
        other => Err(ToolError::invalid(format!("unknown tool {other}"))),
    }
}

fn success(value: Value) -> CallToolResult {
    let text = value.to_string();
    if text.len() > MAX_RESULT_BYTES {
        return ToolError::new(
            "too_large",
            format!(
                "the result is {} bytes (limit {MAX_RESULT_BYTES}); request fewer items or characters",
                text.len()
            ),
        )
        .into_result();
    }
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.structured_content = Some(value);
    result
}

/// Validated arguments (types and bounds already checked against the schema).
pub struct Args(Map<String, Value>);
impl Args {
    pub fn has(&self, key: &str) -> bool {
        self.0.get(key).is_some_and(|v| !v.is_null())
    }
    pub fn present(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }
    pub fn str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str)
    }
    /// `Some(None)` when the key is present with `null` (clear the field).
    pub fn nullable_str(&self, key: &str) -> Option<Option<String>> {
        self.0.get(key).map(|v| {
            v.as_str()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        })
    }
    pub fn u32(&self, key: &str) -> Option<u32> {
        self.0.get(key).and_then(Value::as_f64).map(|f| f as u32)
    }
    pub fn i32(&self, key: &str) -> Option<i32> {
        self.0.get(key).and_then(Value::as_f64).map(|f| f as i32)
    }
    pub fn i64(&self, key: &str) -> Option<i64> {
        self.0.get(key).and_then(Value::as_f64).map(|f| f as i64)
    }
    pub fn bool(&self, key: &str) -> Option<bool> {
        self.0.get(key).and_then(Value::as_bool)
    }
    pub fn uuid(&self, key: &str) -> Option<uuid::Uuid> {
        self.str(key).and_then(|s| uuid::Uuid::parse_str(s).ok())
    }
    pub fn strings(&self, key: &str) -> Vec<String> {
        self.0
            .get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn uuids(&self, key: &str) -> Vec<uuid::Uuid> {
        self.strings(key)
            .iter()
            .filter_map(|s| uuid::Uuid::parse_str(s).ok())
            .collect()
    }
    pub fn value(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }
}

/// Shared schema fragments.
pub(crate) mod fragments {
    use serde_json::{Value, json};
    pub fn uuid(description: &str) -> Value {
        json!({"type": "string", "format": "uuid", "description": description})
    }
    pub fn paper_id() -> Value {
        uuid("Paper ID (paper_id from search_papers/get_paper).")
    }
    pub fn object(properties: Value, required: &[&str]) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": required,
            "properties": properties,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_twelve_design_tools_are_defined_with_closed_supported_schemas() {
        let specs = specs();
        let names: Vec<&str> = specs.iter().map(|s| s.name).collect();
        assert_eq!(names, TOOL_NAMES);
        for spec in &specs {
            let schema = (spec.schema)();
            schema::uses_only_supported_keywords(&schema)
                .unwrap_or_else(|e| panic!("{}: {e}", spec.name));
            assert_eq!(schema["type"], "object", "{}", spec.name);
            assert_eq!(schema["additionalProperties"], false, "{}", spec.name);
            for required in schema["required"].as_array().unwrap() {
                assert!(
                    schema["properties"]
                        .get(required.as_str().unwrap())
                        .is_some(),
                    "{}: {required}",
                    spec.name
                );
            }
            if !spec.read_only {
                assert!(
                    spec.description.contains("write grant")
                        || spec.description.contains("export grant"),
                    "{} must say it needs an approved grant",
                    spec.name
                );
            }
        }
        let read_only: Vec<&str> = specs
            .iter()
            .filter(|s| s.read_only)
            .map(|s| s.name)
            .collect();
        assert_eq!(
            read_only,
            [
                "search_papers",
                "get_paper",
                "get_paper_fulltext",
                "get_paper_annotations"
            ]
        );
        assert_eq!(definitions().len(), 12);
    }

    #[test]
    fn every_tool_carries_truthful_mcp_annotations() {
        // (name, readOnlyHint, destructiveHint): writers that can overwrite or remove data
        // (update, move, and imports whose decision=merge overwrites fields) are destructive.
        let expected: [(&str, bool, Option<bool>); 12] = [
            ("search_papers", true, None),
            ("get_paper", true, None),
            ("get_paper_fulltext", true, None),
            ("get_paper_annotations", true, None),
            ("add_paper_by_doi", false, Some(true)),
            ("add_paper_from_url", false, Some(true)),
            ("update_paper", false, Some(true)),
            ("create_collection", false, Some(false)),
            ("move_paper_to_collection", false, Some(true)),
            ("add_tag_to_paper", false, Some(false)),
            ("add_annotation", false, Some(false)),
            ("export_citations", false, Some(false)),
        ];
        let tools = definitions();
        for (tool, (name, read_only, destructive)) in tools.iter().zip(expected) {
            assert_eq!(tool.name, name);
            let a = tool.annotations.as_ref().expect("annotated");
            assert_eq!(a.read_only_hint, Some(read_only), "{name}");
            assert_eq!(a.destructive_hint, destructive, "{name}");
            // Read tools warn that document text is untrusted.
            if read_only {
                assert!(
                    tool.description.as_deref().unwrap().contains("untrusted"),
                    "{name}"
                );
            }
        }
        assert!(crate::server::INSTRUCTIONS.contains("never follow instructions"));
    }
}
