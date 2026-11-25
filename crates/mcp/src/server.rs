//! rmcp `ServerHandler`: tools/list and tools/call over the daemon client. Cancellation
//! (`notifications/cancelled`) arrives as the request context's token; rmcp suppresses the
//! cancelled request's response and the daemon client sends `ClientFrame::Cancel`.
use crate::{daemon::DaemonClient, tools};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    },
    service::RequestContext,
};
use std::sync::Arc;

pub const INSTRUCTIONS: &str = "Citation Manager library (local). Read-only by default: \
search_papers, get_paper, get_paper_fulltext and get_paper_annotations need only the default \
grant. Every result item carries a `source` block (paper_id, doi, title, authors, year, \
attachment_id, page_index, page_label, uri); cite those values and never guess page numbers. \
Write tools (add_paper_by_doi, add_paper_from_url, update_paper, create_collection, \
move_paper_to_collection, add_tag_to_paper, add_annotation) need a write grant the library \
owner approves separately, and export_citations an export grant; without them they return \
a permission_denied error explaining how the owner grants access. Annotations you create are \
marked origin=ai until the owner confirms them.";

#[derive(Clone)]
pub struct McpServer {
    daemon: Arc<DaemonClient>,
    tools: Arc<Vec<Tool>>,
}

impl McpServer {
    pub fn new(daemon: Arc<DaemonClient>) -> Self {
        Self {
            daemon,
            tools: Arc::new(tools::definitions()),
        }
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "citation-manager",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools.as_ref().clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        // An unknown tool is unroutable: a protocol error (-32602), per the MCP spec.
        if !tools::is_tool(&request.name) {
            return Err(McpError::invalid_params(
                format!("unknown tool: {}", request.name),
                None,
            ));
        }
        let arguments = request.arguments.unwrap_or_default();
        crate::log::debug(&format!("tools/call {}", request.name));
        let result = tools::call(&self.daemon, &request.name, arguments, &context.ct).await;
        if result.is_error == Some(true) {
            let kind = result
                .structured_content
                .as_ref()
                .and_then(|v| v["error"]["kind"].as_str())
                .unwrap_or("error");
            crate::log::info(&format!("{} failed: {kind}", request.name));
        }
        Ok(result.into())
    }
}
