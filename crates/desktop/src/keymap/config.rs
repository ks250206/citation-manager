use super::{Binding, Context, KeyStroke, Mode};
use crate::commands::CommandRegistry;
use serde::Deserialize;

pub const MAX_CONFIG_BYTES: usize = 64 * 1024;
pub const MAX_BINDINGS: usize = 256;
pub const MAX_SEQUENCE_KEYS: usize = 4;
pub const MAX_JSON_DEPTH: usize = 8;
const MAX_DIAGNOSTICS: usize = 64;
const DEFAULT_JSON: &[u8] = include_bytes!("default.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticCode {
    TooManyBytes,
    TooDeep,
    InvalidJson,
    InvalidSchema,
    UnsupportedVersion,
    TooManyBindings,
    InvalidSequenceLength,
    InvalidKey,
    UnknownCommand,
    DuplicateBinding,
    PrefixConflict,
}

/// Paths are schema paths, never user input. Diagnostics do not echo configuration contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub path: String,
    pub related_binding: Option<usize>,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

impl Diagnostic {
    fn at(code: DiagnosticCode, path: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            related_binding: None,
            line: None,
            column: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    bindings: Vec<RawBinding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBinding {
    context: Context,
    mode: Mode,
    keys: Vec<String>,
    command: String,
}

/// Constructible only after whole-document validation. Bindings are private and immutable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    pub(super) bindings: Vec<Binding>,
}

impl Keymap {
    pub fn parse(bytes: &[u8], registry: &CommandRegistry) -> Result<Self, Vec<Diagnostic>> {
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(vec![Diagnostic::at(DiagnosticCode::TooManyBytes, "$")]);
        }
        // Scan structural nesting before serde allocation/recursion, respecting quoted/escaped text.
        let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
        for &byte in bytes {
            if quoted {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    quoted = false;
                }
            } else {
                match byte {
                    b'"' => quoted = true,
                    b'{' | b'[' => {
                        depth += 1;
                        if depth > MAX_JSON_DEPTH {
                            return Err(vec![Diagnostic::at(DiagnosticCode::TooDeep, "$")]);
                        }
                    }
                    b'}' | b']' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
        }
        let document: Document = serde_json::from_slice(bytes).map_err(|error| {
            let mut diagnostic = Diagnostic::at(
                if error.is_data() {
                    DiagnosticCode::InvalidSchema
                } else {
                    DiagnosticCode::InvalidJson
                },
                "$",
            );
            diagnostic.line = Some(error.line());
            diagnostic.column = Some(error.column());
            vec![diagnostic]
        })?;
        if document.version != 1 {
            return Err(vec![Diagnostic::at(
                DiagnosticCode::UnsupportedVersion,
                "$.version",
            )]);
        }
        if document.bindings.len() > MAX_BINDINGS {
            return Err(vec![Diagnostic::at(
                DiagnosticCode::TooManyBindings,
                "$.bindings",
            )]);
        }
        let mut diagnostics = Vec::new();
        let mut bindings: Vec<(usize, Binding)> = Vec::new();
        for (index, raw) in document.bindings.into_iter().enumerate() {
            let path = format!("$.bindings[{index}]");
            if raw.keys.is_empty() || raw.keys.len() > MAX_SEQUENCE_KEYS {
                diagnostics.push(Diagnostic::at(
                    DiagnosticCode::InvalidSequenceLength,
                    format!("{path}.keys"),
                ));
            } else if registry.get(&raw.command).is_none() {
                diagnostics.push(Diagnostic::at(
                    DiagnosticCode::UnknownCommand,
                    format!("{path}.command"),
                ));
            } else {
                let mut keys = Vec::new();
                for (key_index, key) in raw.keys.iter().enumerate() {
                    match KeyStroke::parse(key) {
                        Ok(key) => keys.push(key),
                        Err(_) => diagnostics.push(Diagnostic::at(
                            DiagnosticCode::InvalidKey,
                            format!("{path}.keys[{key_index}]"),
                        )),
                    }
                }
                if keys.len() == raw.keys.len() {
                    let binding = Binding {
                        context: raw.context,
                        mode: raw.mode,
                        keys,
                        command: raw.command,
                    };
                    for (previous_index, previous) in &bindings {
                        if previous.context == binding.context
                            && previous.mode == binding.mode
                            && (previous.keys.starts_with(&binding.keys)
                                || binding.keys.starts_with(&previous.keys))
                        {
                            let mut diagnostic = Diagnostic::at(
                                if previous.keys == binding.keys {
                                    DiagnosticCode::DuplicateBinding
                                } else {
                                    DiagnosticCode::PrefixConflict
                                },
                                format!("{path}.keys"),
                            );
                            diagnostic.related_binding = Some(*previous_index);
                            diagnostics.push(diagnostic);
                        }
                    }
                    bindings.push((index, binding));
                }
            }
            if diagnostics.len() >= MAX_DIAGNOSTICS {
                break;
            }
        }
        if !diagnostics.is_empty() {
            diagnostics.truncate(MAX_DIAGNOSTICS);
            return Err(diagnostics);
        }
        Ok(Self {
            bindings: bindings.into_iter().map(|(_, b)| b).collect(),
        })
    }

    pub fn defaults(registry: &CommandRegistry) -> Result<Self, Vec<Diagnostic>> {
        Self::parse(DEFAULT_JSON, registry)
    }

    pub fn binding_count(&self) -> usize {
        self.bindings.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    User,
    Defaults,
}

#[derive(Debug)]
pub struct LoadedKeymap {
    pub keymap: Keymap,
    pub source: ConfigSource,
    /// Rejected user-document diagnostics, empty for a fully valid user configuration.
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug)]
pub struct DefaultKeymapError {
    pub rejected_config: Vec<Diagnostic>,
    pub invalid_defaults: Vec<Diagnostic>,
}

/// Invalid input is never partly applied. Fallback is explicit and validated against the same registry.
/// A host with an incomplete built-in registry gets an error, never a silently weakened default map.
pub fn load_or_default(
    bytes: &[u8],
    registry: &CommandRegistry,
) -> Result<LoadedKeymap, DefaultKeymapError> {
    match Keymap::parse(bytes, registry) {
        Ok(keymap) => Ok(LoadedKeymap {
            keymap,
            source: ConfigSource::User,
            diagnostics: vec![],
        }),
        Err(diagnostics) => match Keymap::defaults(registry) {
            Ok(keymap) => Ok(LoadedKeymap {
                keymap,
                source: ConfigSource::Defaults,
                diagnostics,
            }),
            Err(invalid_defaults) => Err(DefaultKeymapError {
                rejected_config: diagnostics,
                invalid_defaults,
            }),
        },
    }
}
