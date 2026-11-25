//! Bounded configuration and deterministic input policy. No I/O, UI, dispatch or persistence.
mod config;
mod key;
mod resolver;

pub use config::{
    ConfigSource, DefaultKeymapError, Diagnostic, DiagnosticCode, Keymap, LoadedKeymap,
    MAX_BINDINGS, MAX_CONFIG_BYTES, MAX_JSON_DEPTH, MAX_SEQUENCE_KEYS, load_or_default,
};
pub use key::{InvalidKey, KeyStroke, MAX_KEY_BYTES};
pub use resolver::{InputState, KeyResolver, Resolution, SEQUENCE_TIMEOUT_MS};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Context {
    Library,
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Normal,
    Insert,
    VimOff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Binding {
    context: Context,
    mode: Mode,
    keys: Vec<KeyStroke>,
    command: String,
}
