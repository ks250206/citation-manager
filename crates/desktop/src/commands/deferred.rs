use super::{Command, Requirement};

/// Known design examples only. Their feature owners replace these descriptors when connected.
pub fn deferred_commands() -> Vec<Command> {
    [(
        "api.open_settings",
        "Open API Settings",
        26,
        "API settings are not connected",
    )]
    .into_iter()
    .map(|(id, title, issue, reason)| Command {
        id,
        title,
        description: reason,
        requirement: Requirement::Deferred { issue, reason },
    })
    .collect()
}
