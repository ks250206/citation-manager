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

    pub(super) fn is_escape(&self) -> bool {
        self.key == "Esc" && self.modifiers == 0
    }
    pub(super) fn is_shortcut(&self) -> bool {
        self.modifiers & (CMD | CTRL) != 0
    }
}
