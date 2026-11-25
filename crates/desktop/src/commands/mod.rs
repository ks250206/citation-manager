//! Descriptions and UI availability only. Command IDs are never authorization grants.
mod deferred;
mod library;

use std::collections::BTreeMap;

pub use deferred::deferred_commands;
pub use library::{ids, library_commands};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisabledReason {
    NotConnected,
    LibraryUnavailable,
    SelectionRequired,
    Busy,
    Deferred { issue: u32, reason: &'static str },
    NotRegistered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Enabled,
    Disabled(DisabledReason),
}

/// Host-supplied facts, not capabilities or permissions. Default is disconnected.
#[derive(Debug, Default, Clone, Copy)]
pub struct CommandContext<'a> {
    pub connected_handlers: &'a [&'a str],
    pub library_ready: bool,
    pub has_selection: bool,
    pub busy: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum Requirement {
    Ui,
    Library,
    Selection,
    Deferred { issue: u32, reason: &'static str },
}

/// Feature modules may register additional descriptors without extending a central enum.
#[derive(Debug, Clone, Copy)]
pub struct Command {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub requirement: Requirement,
}

impl Command {
    pub fn availability(&self, context: &CommandContext<'_>) -> Availability {
        use Availability::{Disabled, Enabled};
        use DisabledReason::*;
        if let Requirement::Deferred { issue, reason } = self.requirement {
            return Disabled(Deferred { issue, reason });
        }
        if !context.connected_handlers.contains(&self.id) {
            return Disabled(NotConnected);
        }
        if matches!(self.requirement, Requirement::Ui) {
            return Enabled;
        }
        if context.busy {
            return Disabled(Busy);
        }
        if !context.library_ready {
            return Disabled(LibraryUnavailable);
        }
        if matches!(self.requirement, Requirement::Selection) && !context.has_selection {
            return Disabled(SelectionRequired);
        }
        Enabled
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    DuplicateId(&'static str),
    InvalidId(&'static str),
}

#[derive(Debug, Default)]
pub struct CommandRegistry {
    commands: BTreeMap<&'static str, Command>,
}

impl CommandRegistry {
    pub fn register(&mut self, command: Command) -> Result<(), RegistryError> {
        // Stable dotted ASCII identifiers; labels/descriptions may be Unicode.
        if command.id.len() > 96
            || command.id.split('.').count() < 2
            || command.id.split('.').any(|part| {
                part.is_empty()
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit())
            })
        {
            return Err(RegistryError::InvalidId(command.id));
        }
        if self.commands.contains_key(command.id) {
            return Err(RegistryError::DuplicateId(command.id));
        }
        self.commands.insert(command.id, command);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Command> {
        self.commands.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Command> {
        self.commands.values()
    }

    pub fn availability(&self, id: &str, context: &CommandContext<'_>) -> Availability {
        self.get(id)
            .map_or(Availability::Disabled(DisabledReason::NotRegistered), |c| {
                c.availability(context)
            })
    }

    pub fn m1() -> Self {
        let mut registry = Self::default();
        for command in library_commands().into_iter().chain(deferred_commands()) {
            registry
                .register(command)
                .expect("built-in IDs are unique and valid");
        }
        registry
    }
}
