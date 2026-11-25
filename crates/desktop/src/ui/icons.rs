//! #87 the app's icon set: Lucide (ISC), vendored under `crates/desktop/assets/icons/lucide/`
//! with only the icons listed here (license next to them and in `docs/LICENSES.md`).
//! The SVGs are compiled into the binary; the native host serves them to GPUI as assets
//! under `icons/<name>.svg` and tints them with the current text colour.

macro_rules! icons {
    ($($variant:ident => $file:literal,)*) => {
        /// One icon of the vendored set.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Icon {
            $($variant,)*
        }
        impl Icon {
            pub const ALL: &'static [Icon] = &[$(Icon::$variant,)*];
            /// Lucide file name without extension.
            pub fn name(self) -> &'static str {
                match self {
                    $(Icon::$variant => $file,)*
                }
            }
            /// Asset path GPUI loads (`svg().path(..)`).
            pub fn path(self) -> &'static str {
                match self {
                    $(Icon::$variant => concat!("icons/", $file, ".svg"),)*
                }
            }
            pub fn svg(self) -> &'static [u8] {
                match self {
                    $(Icon::$variant => include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"), "/assets/icons/lucide/", $file, ".svg"
                    )),)*
                }
            }
        }
    };
}

icons! {
    ArrowDown => "arrow-down",
    ArrowLeft => "arrow-left",
    ArrowUp => "arrow-up",
    Check => "check",
    ChevronDown => "chevron-down",
    ChevronRight => "chevron-right",
    ChevronUp => "chevron-up",
    CircleDot => "circle-dot",
    FileText => "file-text",
    Folder => "folder",
    Hourglass => "hourglass",
    Library => "library",
    Maximize => "maximize-2",
    Minimize => "minimize-2",
    Minus => "minus",
    Pencil => "pencil",
    Plus => "plus",
    Redo => "redo-2",
    Refresh => "refresh-cw",
    RotateCw => "rotate-cw",
    Settings => "settings",
    Square => "square",
    SquareCheck => "square-check",
    StickyNote => "sticky-note",
    Trash => "trash-2",
    TriangleAlert => "triangle-alert",
    Undo => "undo-2",
    X => "x",
}

/// Asset lookup for the native host: the SVG bytes of `icons/<name>.svg`.
pub fn load(path: &str) -> Option<&'static [u8]> {
    Icon::ALL
        .iter()
        .find(|icon| icon.path() == path)
        .map(|icon| icon.svg())
}

/// Checkbox glyph for a boolean option.
pub fn checkbox(on: bool) -> Icon {
    if on { Icon::SquareCheck } else { Icon::Square }
}
