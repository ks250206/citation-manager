//! #87 design tokens: WCAG AA contrast of every pairing the native views draw, theme and
//! density settings, and the vendored icon set.
use cm_desktop::settings::Settings;
use cm_desktop::ui::icons::{self, Icon};
use cm_desktop::ui::theme::{
    self, DARK, Density, LIGHT, PAIRINGS, Requirement, Scheme, ThemeMode, contrast_ratio,
};

#[test]
fn contrast_math_matches_wcag_reference_values() {
    assert!((contrast_ratio(0x000000, 0xffffff) - 21.0).abs() < 1e-9);
    assert!((contrast_ratio(0xffffff, 0xffffff) - 1.0).abs() < 1e-9);
    // Symmetric, and #767676 on white is the classic 4.54:1 AA boundary.
    assert_eq!(
        contrast_ratio(0x767676, 0xffffff),
        contrast_ratio(0xffffff, 0x767676)
    );
    let grey = contrast_ratio(0x767676, 0xffffff);
    assert!((4.53..4.55).contains(&grey), "{grey}");
    assert!(contrast_ratio(0x777777, 0xffffff) < 4.5);
    assert_eq!(Requirement::Text.minimum(), 4.5);
    assert_eq!(Requirement::LargeTextOrUi.minimum(), 3.0);
}

#[test]
fn every_used_pairing_meets_wcag_aa_in_both_schemes() {
    assert!(PAIRINGS.len() >= 30);
    for scheme in Scheme::ALL {
        let palette = scheme.palette();
        let mut report = Vec::new();
        for pairing in PAIRINGS {
            let ratio = contrast_ratio((pairing.fg)(palette), (pairing.bg)(palette));
            report.push(format!("{scheme:?} {}: {ratio:.2}", pairing.name));
        }
        let failing = theme::failing_pairings(palette);
        assert!(
            failing.is_empty(),
            "{scheme:?} misses WCAG AA: {failing:?}\n{}",
            report.join("\n")
        );
    }
}

#[test]
fn pairing_names_are_unique_and_text_pairs_cover_every_surface() {
    let mut names: Vec<_> = PAIRINGS.iter().map(|p| p.name).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "duplicate pairing");
    for surface in ["canvas", "surface", "surface_alt", "sidebar", "accent_soft"] {
        assert!(names.contains(&format!("text on {surface}").as_str()));
    }
}

#[test]
fn light_keeps_the_pre_87_look_and_dark_differs() {
    // The colours the views used as constants before #87.
    assert_eq!(LIGHT.accent, 0x3b73d9);
    assert_eq!(LIGHT.surface, 0xffffff);
    assert_eq!(LIGHT.canvas, 0xf5f6f8);
    assert_eq!(LIGHT.text, 0x1c2533);
    assert_eq!(LIGHT.muted, 0x5f6b7c);
    assert_ne!(LIGHT, DARK);
    // Dark really is dark: the canvas is darker than its text.
    assert!(theme::relative_luminance(DARK.canvas) < theme::relative_luminance(DARK.text));
    assert!(theme::relative_luminance(LIGHT.canvas) > theme::relative_luminance(LIGHT.text));
}

#[test]
fn system_mode_follows_the_window_appearance() {
    assert_eq!(theme::resolve(ThemeMode::System, true), Scheme::Dark);
    assert_eq!(theme::resolve(ThemeMode::System, false), Scheme::Light);
    for dark in [false, true] {
        assert_eq!(theme::resolve(ThemeMode::Light, dark), Scheme::Light);
        assert_eq!(theme::resolve(ThemeMode::Dark, dark), Scheme::Dark);
    }
    assert_eq!(theme::parse_theme(" dark "), Some(ThemeMode::Dark));
    assert_eq!(theme::parse_theme("SYSTEM"), Some(ThemeMode::System));
    assert_eq!(theme::parse_theme("sepia"), None);
    assert_eq!(theme::parse_density("compact"), Some(Density::Compact));
    assert_eq!(theme::parse_density("cozy"), None);
    // Palette commands cycle through every mode and come back.
    let mut mode = ThemeMode::Light;
    let mut seen = vec![];
    for _ in 0..3 {
        seen.push(mode);
        mode = theme::next_theme(mode);
    }
    assert_eq!(mode, ThemeMode::Light);
    assert_eq!(seen, ThemeMode::ALL);
    assert_eq!(theme::other_density(Density::Compact), Density::Comfortable);
    assert_eq!(theme::other_density(Density::Comfortable), Density::Compact);
}

#[test]
fn compact_is_denser_than_comfortable_and_comfortable_is_the_old_spacing() {
    let comfortable = theme::metrics(Density::Comfortable);
    let compact = theme::metrics(Density::Compact);
    // Pre-#87 values: 32 px table rows, 34 px status bar, 13 px body text.
    assert_eq!(comfortable.row_height, 32.);
    assert_eq!(comfortable.bar_height, 34.);
    assert_eq!(comfortable.font_body, 13.);
    assert!(compact.row_height < comfortable.row_height);
    assert!(compact.control_py < comfortable.control_py);
    assert!(compact.font_body < comfortable.font_body);
    // Compact text stays legible and rows still fit their text.
    assert!(compact.font_caption >= 10.);
    assert!(compact.row_height >= compact.font_body * 1.8);
    const { assert!(theme::RADIUS.sm < theme::RADIUS.md && theme::RADIUS.md < theme::RADIUS.lg) };
    use theme::Elevation::{Floating, Raised};
    for scheme in Scheme::ALL {
        let (raised, floating) = (
            theme::shadow(scheme, Raised),
            theme::shadow(scheme, Floating),
        );
        assert!(raised.blur < floating.blur && raised.offset_y < floating.offset_y);
    }
    // Dark shadows are denser so they still separate surfaces.
    assert!(
        theme::shadow(Scheme::Dark, Floating).color_rgba & 0xff
            > theme::shadow(Scheme::Light, Floating).color_rgba & 0xff
    );
}

#[test]
fn theme_and_density_are_saved_with_the_other_ui_settings() {
    // Files written before #87 load with System + Comfortable.
    let old = serde_json::to_value(Settings::default()).unwrap();
    let mut legacy = old.as_object().unwrap().clone();
    legacy.remove("theme");
    legacy.remove("density");
    let loaded: Settings = serde_json::from_value(legacy.into()).unwrap();
    assert_eq!(loaded.theme, ThemeMode::System);
    assert_eq!(loaded.density, Density::Comfortable);
    assert!(loaded.valid());
    // A chosen theme and density round-trip.
    let chosen = Settings {
        theme: ThemeMode::Dark,
        density: Density::Compact,
        ..Default::default()
    };
    let json = serde_json::to_string(&chosen).unwrap();
    assert!(json.contains(r#""theme":"Dark""#) && json.contains(r#""density":"Compact""#));
    let back: Settings = serde_json::from_str(&json).unwrap();
    assert_eq!(back, chosen);
    assert!(back.valid());
    // Unknown names are rejected rather than silently replaced.
    let bad = json.replace(r#""theme":"Dark""#, r#""theme":"Sepia""#);
    assert!(serde_json::from_str::<Settings>(&bad).is_err());
}

#[test]
fn every_icon_is_vendored_once_and_nothing_unused_is_vendored() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons/lucide");
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".svg"))
        .collect();
    files.sort();
    let mut used: Vec<String> = Icon::ALL
        .iter()
        .map(|i| format!("{}.svg", i.name()))
        .collect();
    used.sort();
    assert_eq!(files, used, "vendor exactly the icons in Icon::ALL");
    let license = std::fs::read_to_string(dir.join("LICENSE")).unwrap();
    assert!(license.starts_with("ISC License"));
    for icon in Icon::ALL {
        let svg = std::str::from_utf8(icon.svg()).unwrap();
        assert!(svg.contains("@license lucide-static"), "{icon:?}");
        assert!(
            svg.contains("<svg") && svg.contains("viewBox=\"0 0 24 24\""),
            "{icon:?}"
        );
        assert_eq!(icons::load(icon.path()), Some(icon.svg()));
    }
    assert_eq!(icons::load("icons/missing.svg"), None);
    assert_eq!(icons::checkbox(true), Icon::SquareCheck);
    assert_eq!(icons::checkbox(false), Icon::Square);
}

#[test]
fn third_party_license_record_names_the_icon_set() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let licenses = std::fs::read_to_string(root.join("docs/LICENSES.md")).unwrap();
    assert!(licenses.contains("Lucide"));
    assert!(licenses.contains("crates/desktop/assets/icons/lucide/LICENSE"));
}
