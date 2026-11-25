//! #87 design tokens: colour, spacing, radius, font size and shadow, for the Light and Dark
//! schemes and the Compact and Comfortable densities.
//!
//! Colours are `0xRRGGBB` (opaque) or `0xRRGGBBAA` (`*_rgba`, translucent overlays) so this
//! module stays free of GPUI; the native host converts them with `rgb`/`rgba`. The Light
//! palette is the look the app had before #87, so switching to tokens changed no pixel in
//! Light except the text-field border (darkened to reach 3:1 against its background).
pub use cm_protocol::settings::{Density, ThemeMode};

/// The scheme actually drawn: a `ThemeMode` resolved against the system appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scheme {
    Light,
    Dark,
}

impl Scheme {
    pub const ALL: [Scheme; 2] = [Scheme::Light, Scheme::Dark];
    pub fn palette(self) -> &'static Palette {
        match self {
            Scheme::Light => &LIGHT,
            Scheme::Dark => &DARK,
        }
    }
}

/// `System` follows the window appearance; the explicit modes ignore it.
pub fn resolve(mode: ThemeMode, system_dark: bool) -> Scheme {
    match mode {
        ThemeMode::Light => Scheme::Light,
        ThemeMode::Dark => Scheme::Dark,
        ThemeMode::System if system_dark => Scheme::Dark,
        ThemeMode::System => Scheme::Light,
    }
}

/// Parse a theme name as written in settings or typed in the palette (case-insensitive).
pub fn parse_theme(name: &str) -> Option<ThemeMode> {
    ThemeMode::ALL
        .into_iter()
        .find(|m| m.label().eq_ignore_ascii_case(name.trim()))
}

/// Parse a density name (case-insensitive).
pub fn parse_density(name: &str) -> Option<Density> {
    Density::ALL
        .into_iter()
        .find(|d| d.label().eq_ignore_ascii_case(name.trim()))
}

/// The palette's "Switch Theme": Light → Dark → System → Light.
pub fn next_theme(mode: ThemeMode) -> ThemeMode {
    match mode {
        ThemeMode::Light => ThemeMode::Dark,
        ThemeMode::Dark => ThemeMode::System,
        ThemeMode::System => ThemeMode::Light,
    }
}

/// The palette's "Toggle Density".
pub fn other_density(density: Density) -> Density {
    match density {
        Density::Compact => Density::Comfortable,
        Density::Comfortable => Density::Compact,
    }
}

/// Colour tokens. Names describe the role, not the hue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    /// Window background behind cards and panes.
    pub canvas: u32,
    /// Cards, panes, tables, menus, inputs.
    pub surface: u32,
    /// Table header and toolbars inside a surface.
    pub surface_alt: u32,
    /// Every other table row.
    pub row_alt: u32,
    /// Sidebar, segmented controls and key chips.
    pub sidebar: u32,
    /// Card, pane and table borders (decorative).
    pub border: u32,
    /// Thin separators inside a list.
    pub divider: u32,
    pub text: u32,
    /// Secondary text: hints, headers, non-title cells.
    pub muted: u32,
    /// Inactive controls (WCAG 1.4.3 exempts inactive components).
    pub disabled_text: u32,
    pub disabled_bg: u32,
    pub disabled_border: u32,
    /// Primary button, focus ring, selection marks.
    pub accent: u32,
    pub accent_hover: u32,
    pub accent_pressed: u32,
    /// Selected rows, active tabs and menu items.
    pub accent_soft: u32,
    /// Text on `accent_soft` and secondary buttons.
    pub accent_text: u32,
    /// Text on `accent`.
    pub on_accent: u32,
    pub button_border: u32,
    pub button_hover: u32,
    /// Row hover in tables and lists.
    pub hover: u32,
    /// Hover of navigation items on the sidebar.
    pub nav_hover: u32,
    /// Destructive actions and invalid values.
    pub danger: u32,
    pub danger_soft: u32,
    pub danger_border: u32,
    /// Error banners: `error_text` on `error_bg`.
    pub error_bg: u32,
    pub error_text: u32,
    pub warn_bg: u32,
    pub warn_text: u32,
    pub warn_border: u32,
    pub success_bg: u32,
    pub info_bg: u32,
    /// Search hit highlight behind `text`.
    pub match_bg: u32,
    /// Import review merge panel and its fields.
    pub merge_bg: u32,
    pub merge_field: u32,
    pub merge_field_active: u32,
    /// Sticky-note annotation popover.
    pub note_bg: u32,
    pub note_border: u32,
    /// Behind rendered PDF pages.
    pub pdf_canvas: u32,
    pub input_border: u32,
    pub input_disabled_bg: u32,
    pub input_disabled_text: u32,
    /// Translucent tokens (`0xRRGGBBAA`).
    pub placeholder_rgba: u32,
    pub text_selection_rgba: u32,
    pub scrim_rgba: u32,
    pub pdf_selection_rgba: u32,
    pub find_hit_rgba: u32,
    pub find_current_rgba: u32,
    /// Page badge over a PDF page; text is `on_accent`.
    pub badge_rgba: u32,
}

pub const LIGHT: Palette = Palette {
    canvas: 0xf5f6f8,
    surface: 0xffffff,
    surface_alt: 0xf8f9fb,
    row_alt: 0xfafbfc,
    sidebar: 0xeef1f5,
    border: 0xd5dce5,
    divider: 0xeef1f5,
    text: 0x1c2533,
    muted: 0x5f6b7c,
    disabled_text: 0x9aa3af,
    disabled_bg: 0xf2f3f5,
    disabled_border: 0xe1e5ea,
    accent: 0x3b73d9,
    accent_hover: 0x2f62c2,
    accent_pressed: 0x2654a8,
    accent_soft: 0xdde8fb,
    accent_text: 0x163c80,
    on_accent: 0xffffff,
    button_border: 0xc3cfe0,
    button_hover: 0xeef3fd,
    hover: 0xf0f4fa,
    nav_hover: 0xe2e7ee,
    danger: 0xb42318,
    danger_soft: 0xfde8e4,
    danger_border: 0xf1c4bf,
    error_bg: 0xffe5e5,
    error_text: 0x9b1c1c,
    warn_bg: 0xfff4d6,
    warn_text: 0x6b4e00,
    warn_border: 0xe0b84d,
    success_bg: 0xe5efe8,
    info_bg: 0xe0eaff,
    match_bg: 0xffe38a,
    merge_bg: 0xf0e9dc,
    merge_field: 0xf5eee3,
    merge_field_active: 0xffe3ad,
    note_bg: 0xfffbea,
    note_border: 0xf0d58a,
    pdf_canvas: 0xe7e9ed,
    input_border: 0x828d9d,
    input_disabled_bg: 0xf0f2f5,
    input_disabled_text: 0x7a8494,
    placeholder_rgba: 0x00000066,
    text_selection_rgba: 0x3311ff30,
    scrim_rgba: 0x0f172a40,
    pdf_selection_rgba: 0x3b73d955,
    find_hit_rgba: 0xffb34755,
    find_current_rgba: 0xff8c0088,
    badge_rgba: 0x1c253399,
};

pub const DARK: Palette = Palette {
    canvas: 0x15181d,
    surface: 0x1d2127,
    surface_alt: 0x22272e,
    row_alt: 0x20242a,
    sidebar: 0x191c21,
    border: 0x343b46,
    divider: 0x2a3038,
    text: 0xe6e9ee,
    muted: 0xa3adbb,
    disabled_text: 0x6b7480,
    disabled_bg: 0x22262c,
    disabled_border: 0x2e343c,
    accent: 0x3b73d9,
    accent_hover: 0x3468cc,
    accent_pressed: 0x2f62c2,
    accent_soft: 0x223552,
    accent_text: 0xa9c6f5,
    on_accent: 0xffffff,
    button_border: 0x3a4452,
    button_hover: 0x26303d,
    hover: 0x262c34,
    nav_hover: 0x2a313b,
    danger: 0xf28b82,
    danger_soft: 0x3a2220,
    danger_border: 0x6b2b26,
    error_bg: 0x3d1f1f,
    error_text: 0xffb4ab,
    warn_bg: 0x3a2f12,
    warn_text: 0xf5d37a,
    warn_border: 0x8a6d1f,
    success_bg: 0x1f3328,
    info_bg: 0x1f2d4a,
    match_bg: 0x6b5300,
    merge_bg: 0x2e2a22,
    merge_field: 0x29261f,
    merge_field_active: 0x5a4615,
    note_bg: 0x2b2614,
    note_border: 0x7a6420,
    pdf_canvas: 0x0f1115,
    input_border: 0x6b7685,
    input_disabled_bg: 0x20242a,
    input_disabled_text: 0x8a93a0,
    placeholder_rgba: 0xffffff73,
    text_selection_rgba: 0x5b8cff55,
    scrim_rgba: 0x00000099,
    pdf_selection_rgba: 0x3b73d955,
    find_hit_rgba: 0xffb34755,
    find_current_rgba: 0xff8c0088,
    badge_rgba: 0x000000cc,
};

/// Corner radii in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Radius {
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
}
pub const RADIUS: Radius = Radius {
    sm: 2.,
    md: 6.,
    lg: 8.,
};

/// Drop shadow of floating surfaces (menus, palette). `color_rgba` is `0xRRGGBBAA`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadow {
    pub offset_y: f32,
    pub blur: f32,
    pub color_rgba: u32,
}
pub fn shadow(scheme: Scheme) -> Shadow {
    match scheme {
        Scheme::Light => Shadow {
            offset_y: 4.,
            blur: 16.,
            color_rgba: 0x0f172a33,
        },
        Scheme::Dark => Shadow {
            offset_y: 4.,
            blur: 16.,
            color_rgba: 0x00000099,
        },
    }
}

/// Spacing and font-size tokens for one density (logical pixels).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Height of a library table row.
    pub row_height: f32,
    /// Vertical / horizontal padding of buttons.
    pub control_py: f32,
    pub control_px: f32,
    /// Vertical padding of list and navigation items.
    pub item_py: f32,
    /// Height of the status bar and text fields.
    pub bar_height: f32,
    pub field_height: f32,
    /// Gap between related controls.
    pub gap: f32,
    /// Font sizes: section headings, secondary text, body, card titles.
    pub font_caption: f32,
    pub font_small: f32,
    pub font_body: f32,
    pub font_title: f32,
    /// Icon edge length.
    pub icon: f32,
}

/// Comfortable is the spacing used before #87; Compact fits about a third more rows.
pub fn metrics(density: Density) -> Metrics {
    match density {
        Density::Comfortable => Metrics {
            row_height: 32.,
            control_py: 4.,
            control_px: 12.,
            item_py: 4.,
            bar_height: 34.,
            field_height: 32.,
            gap: 8.,
            font_caption: 11.,
            font_small: 12.,
            font_body: 13.,
            font_title: 15.,
            icon: 14.,
        },
        Density::Compact => Metrics {
            row_height: 24.,
            control_py: 2.,
            control_px: 8.,
            item_py: 2.,
            bar_height: 28.,
            field_height: 26.,
            gap: 6.,
            font_caption: 10.,
            font_small: 11.,
            font_body: 12.,
            font_title: 14.,
            icon: 13.,
        },
    }
}

/// WCAG 2.x relative luminance of an opaque `0xRRGGBB` colour.
pub fn relative_luminance(color: u32) -> f64 {
    let channel = |shift: u32| {
        let v = f64::from((color >> shift) & 0xff) / 255.;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// WCAG contrast ratio (1.0 ..= 21.0) of two opaque colours.
pub fn contrast_ratio(a: u32, b: u32) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// What a foreground/background pairing has to meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    /// Normal-size text: 4.5:1 (WCAG 1.4.3 AA).
    Text,
    /// Large text and UI components / focus indicators: 3:1 (WCAG 1.4.3 / 1.4.11 AA).
    LargeTextOrUi,
}

impl Requirement {
    pub fn minimum(self) -> f64 {
        match self {
            Requirement::Text => 4.5,
            Requirement::LargeTextOrUi => 3.0,
        }
    }
}

/// One foreground/background combination the native views actually draw.
#[derive(Debug, Clone, Copy)]
pub struct Pairing {
    pub name: &'static str,
    pub fg: fn(&Palette) -> u32,
    pub bg: fn(&Palette) -> u32,
    pub requirement: Requirement,
}

macro_rules! pair {
    ($fg:ident on $bg:ident, $req:ident) => {
        Pairing {
            name: concat!(stringify!($fg), " on ", stringify!($bg)),
            fg: |p| p.$fg,
            bg: |p| p.$bg,
            requirement: Requirement::$req,
        }
    };
}

/// Every opaque text/background and indicator/background pairing used by `native/`.
/// Disabled controls (`disabled_text`, `input_disabled_text`) are left out on purpose:
/// WCAG 1.4.3 exempts inactive user-interface components. Translucent overlays drawn over
/// PDF pages are not listed because their backdrop is the document itself.
pub const PAIRINGS: &[Pairing] = &[
    // Body text on every surface it is drawn on.
    pair!(text on canvas, Text),
    pair!(text on surface, Text),
    pair!(text on surface_alt, Text),
    pair!(text on row_alt, Text),
    pair!(text on sidebar, Text),
    pair!(text on hover, Text),
    pair!(text on nav_hover, Text),
    pair!(text on accent_soft, Text),
    pair!(text on success_bg, Text),
    pair!(text on info_bg, Text),
    pair!(text on match_bg, Text),
    pair!(text on merge_bg, Text),
    pair!(text on merge_field, Text),
    pair!(text on merge_field_active, Text),
    pair!(text on note_bg, Text),
    pair!(text on warn_bg, Text),
    pair!(text on danger_soft, Text),
    pair!(text on error_bg, Text),
    // Secondary text.
    pair!(muted on canvas, Text),
    pair!(muted on surface, Text),
    pair!(muted on surface_alt, Text),
    pair!(muted on row_alt, Text),
    pair!(muted on sidebar, Text),
    pair!(muted on hover, Text),
    // Selection and buttons.
    pair!(accent_text on accent_soft, Text),
    pair!(accent_text on surface, Text),
    pair!(accent_text on surface_alt, Text),
    pair!(accent_text on button_hover, Text),
    pair!(accent_text on sidebar, Text),
    pair!(on_accent on accent, Text),
    pair!(on_accent on accent_hover, Text),
    pair!(on_accent on accent_pressed, Text),
    // Status.
    pair!(danger on surface, Text),
    pair!(danger on danger_soft, Text),
    pair!(danger on canvas, Text),
    pair!(error_text on error_bg, Text),
    pair!(error_text on surface, Text),
    pair!(warn_text on warn_bg, Text),
    // UI components and focus indicators (3:1).
    pair!(accent on surface, LargeTextOrUi),
    pair!(accent on note_bg, LargeTextOrUi),
    pair!(input_border on surface, LargeTextOrUi),
    pair!(input_border on canvas, LargeTextOrUi),
    pair!(muted on accent_soft, LargeTextOrUi),
];

/// Pairings of `palette` that miss their requirement, with the measured ratio.
pub fn failing_pairings(palette: &Palette) -> Vec<(&'static str, f64)> {
    PAIRINGS
        .iter()
        .filter_map(|p| {
            let ratio = contrast_ratio((p.fg)(palette), (p.bg)(palette));
            (ratio < p.requirement.minimum()).then_some((p.name, ratio))
        })
        .collect()
}
