use super::{Command, Requirement};

/// #26 API & Integrations settings provider. Opening the screen is a UI action; every
/// token/grant change it leads to is decided by the daemon (`Command::Api`).
pub mod ids {
    pub const OPEN_SETTINGS: &str = "api.open_settings";
    pub const NEW_TOKEN: &str = "api.new_token";
    pub const NEW_MCP_PROFILE: &str = "api.new_mcp_profile";
}

pub fn api_commands() -> Vec<Command> {
    use Requirement::Ui;
    [
        (
            ids::OPEN_SETTINGS,
            "Open API & Integrations Settings",
            "Local API on/off, address and port, tokens, MCP profiles and paired browser hosts",
            Ui,
        ),
        (
            ids::NEW_TOKEN,
            "Issue API Token…",
            "Create a scoped, expiring token for a REST or CLI client (shown once)",
            Ui,
        ),
        (
            ids::NEW_MCP_PROFILE,
            "New MCP Profile (Read-only)…",
            "Create a read-only token for an AI client; writes need a separate approval",
            Ui,
        ),
    ]
    .into_iter()
    .map(|(id, title, description, requirement)| Command {
        id,
        title,
        description,
        requirement,
    })
    .collect()
}
