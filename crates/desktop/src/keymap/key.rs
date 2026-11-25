/// Canonical abstract keystroke, independent of GPUI and keyboard layout.
/// A native adapter must translate layout/shift before calling this layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyStroke {
    key: String,
    modifiers: u8,
}

const CMD: u8 = 1;
const CTRL: u8 = 2;
const ALT: u8 = 4;
const SHIFT: u8 = 8;
pub const MAX_KEY_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidKey;

impl KeyStroke {
    /// `Cmd+k`, `Ctrl+Shift+k`, `g`, `G`, `/`, `Enter`, `Esc`.
    /// Modified letters are lowercase; bare shifted characters are literal (`G`, not `Shift+g`).
    pub fn parse(token: &str) -> Result<Self, InvalidKey> {
        if token.is_empty() || token.len() > MAX_KEY_BYTES || !token.is_ascii() {
            return Err(InvalidKey);
        }
        let mut parts = token.split('+').peekable();
        let mut modifiers = 0;
        let mut key = "";
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                key = part;
                break;
            }
            let bit = match part {
                "Cmd" => CMD,
                "Ctrl" => CTRL,
                "Alt" => ALT,
                "Shift" => SHIFT,
                _ => return Err(InvalidKey),
            };
            if modifiers & bit != 0 {
                return Err(InvalidKey);
            }
            modifiers |= bit;
        }
        let printable = key.len() == 1 && key.as_bytes()[0].is_ascii_graphic();
        let named = matches!(
            key,
            "Enter"
                | "Esc"
                | "Tab"
                | "Backspace"
                | "Delete"
                | "Up"
                | "Down"
                | "Left"
                | "Right"
                | "Home"
                | "End"
                | "PageUp"
                | "PageDown"
                | "Space"
                | "Plus"
        );
        if !printable && !named {
            return Err(InvalidKey);
        }
        if printable
            && ((modifiers == SHIFT) || (modifiers != 0 && key.as_bytes()[0].is_ascii_uppercase()))
        {
            return Err(InvalidKey);
        }
        if modifiers & SHIFT != 0
            && ((printable && !key.as_bytes()[0].is_ascii_alphabetic()) || key == "Plus")
        {
            return Err(InvalidKey);
        }
        Ok(Self {
            key: key.into(),
            modifiers,
        })
    }

    /// Adapter from a platform key event. `primary` is the platform shortcut modifier
    /// (⌘ on macOS, Ctrl elsewhere) and maps to `Cmd`; `control` is an *additional* Control.
    /// Unmodified printable input uses the produced character (`G`, `/`), so layout and Shift
    /// are already applied. Returns None for keys outside the canonical vocabulary.
    pub fn from_platform(
        key: &str,
        key_char: Option<&str>,
        primary: bool,
        control: bool,
        alt: bool,
        shift: bool,
    ) -> Option<Self> {
        let named = match key {
            "enter" => Some("Enter"),
            "escape" => Some("Esc"),
            "tab" => Some("Tab"),
            "backspace" => Some("Backspace"),
            "delete" => Some("Delete"),
            "up" => Some("Up"),
            "down" => Some("Down"),
            "left" => Some("Left"),
            "right" => Some("Right"),
            "home" => Some("Home"),
            "end" => Some("End"),
            "pageup" => Some("PageUp"),
            "pagedown" => Some("PageDown"),
            "space" => Some("Space"),
            _ => None,
        };
        let mut token = String::new();
        if primary {
            token.push_str("Cmd+");
        }
        if control {
            token.push_str("Ctrl+");
        }
        if alt {
            token.push_str("Alt+");
        }
        if !primary && !control && !alt {
            if let Some(name) = named {
                if shift {
                    token.push_str("Shift+");
                }
                token.push_str(name);
            } else {
                let produced = key_char.filter(|c| c.len() == 1)?;
                token.push_str(produced);
            }
        } else {
            if shift {
                token.push_str("Shift+");
            }
            match named {
                Some(name) => token.push_str(name),
                None if key.len() == 1 => token.push_str(&key.to_ascii_lowercase()),
                None => return None,
            }
        }
        Self::parse(&token).ok()
    }

    pub(super) fn is_escape(&self) -> bool {
        self.key == "Esc" && self.modifiers == 0
    }
    pub(super) fn is_shortcut(&self) -> bool {
        self.modifiers & (CMD | CTRL) != 0
    }
}

impl std::fmt::Display for KeyStroke {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (bit, name) in [(CMD, "Cmd"), (CTRL, "Ctrl"), (ALT, "Alt"), (SHIFT, "Shift")] {
            if self.modifiers & bit != 0 {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&self.key)
    }
}
