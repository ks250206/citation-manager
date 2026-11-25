use super::Command;

/// Known design examples only. Their feature owners replace these descriptors when connected.
/// Every former placeholder (#21's PDF/search commands, #26's `api.open_settings`) is
/// connected now, so the provider is empty; it stays as the slot for future designs that are
/// registered before their feature lands (`Requirement::Deferred`).
pub fn deferred_commands() -> Vec<Command> {
    Vec::new()
}
