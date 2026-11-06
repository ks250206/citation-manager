use super::{Context, KeyStroke, Keymap, Mode};
use crate::commands::{Availability, CommandContext, CommandRegistry};

pub const SEQUENCE_TIMEOUT_MS: u64 = 1_000;

/// Host reports actual input/focus state. This struct is not evidence of real focus or IME support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputState {
    pub context: Context,
    pub mode: Mode,
    pub editing: bool,
    pub composing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    PassThrough,
    Pending,
    Cancelled,
    /// A match is a request, not execution. The host must recheck availability at dispatch time.
    Command {
        id: String,
        availability: Availability,
    },
}

/// One resolver per focus owner. Recreate on map replacement; reset on focus loss/cancellation.
#[derive(Debug)]
pub struct KeyResolver {
    keymap: Keymap,
    pending: Vec<KeyStroke>,
    state: Option<InputState>,
    last_key_ms: Option<u64>,
}

impl KeyResolver {
    pub fn new(keymap: Keymap) -> Self {
        Self {
            keymap,
            pending: vec![],
            state: None,
            last_key_ms: None,
        }
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.state = None;
        self.last_key_ms = None;
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Time is a host-supplied monotonic millisecond counter; backwards time also cancels prefixes.
    pub fn resolve(
        &mut self,
        key: &KeyStroke,
        input: InputState,
        now_ms: u64,
        registry: &CommandRegistry,
        commands: &CommandContext<'_>,
    ) -> Resolution {
        if self.state != Some(input)
            || self
                .last_key_ms
                .is_some_and(|last| now_ms < last || now_ms - last >= SEQUENCE_TIMEOUT_MS)
        {
            self.reset();
        }
        // Never intercept editing/IME events, including Escape/Enter and modified shortcuts.
        // Insert/VimOff additionally reserve non-Cmd/Ctrl keys for ordinary application input.
        if input.editing || input.composing || (input.mode != Mode::Normal && !key.is_shortcut()) {
            self.reset();
            return Resolution::PassThrough;
        }
        self.state = Some(input);
        self.last_key_ms = Some(now_ms);
        if key.is_escape() && !self.pending.is_empty() {
            self.reset();
            return Resolution::Cancelled;
        }
        self.pending.push(key.clone());
        let matched = self.find_match(input);
        if let Some(result) = matched {
            return self.finish(result, registry, commands);
        }
        // A mismatch cancels the old prefix and retries only this event as a fresh sequence.
        self.pending.clear();
        self.pending.push(key.clone());
        match self.find_match(input) {
            Some(result) => self.finish(result, registry, commands),
            None => {
                self.reset();
                Resolution::PassThrough
            }
        }
    }

    // None: no prefix; Some(None): incomplete prefix; Some(Some(id)): complete match.
    fn find_match(&self, input: InputState) -> Option<Option<String>> {
        self.keymap
            .bindings
            .iter()
            .find(|binding| {
                binding.context == input.context
                    && binding.mode == input.mode
                    && binding.keys.starts_with(&self.pending)
            })
            .map(|binding| {
                (binding.keys.len() == self.pending.len()).then(|| binding.command.clone())
            })
    }

    fn finish(
        &mut self,
        matched: Option<String>,
        registry: &CommandRegistry,
        commands: &CommandContext<'_>,
    ) -> Resolution {
        if let Some(id) = matched {
            self.reset();
            Resolution::Command {
                availability: registry.availability(&id, commands),
                id,
            }
        } else {
            Resolution::Pending
        }
    }
}
