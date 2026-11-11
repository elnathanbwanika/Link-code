//! Bundled assets: fonts, the app icon and the global stylesheet.
//!
//! Link Code never relies on the system theme. The three brand families are
//! embedded in the binary and written to `~/.local/share/link-code/fonts` on
//! first run, then referenced from the stylesheet with `file://` URLs. That
//! keeps the look identical on a bare XFCE box and on a fully themed GNOME 46
//! desktop, without shipping a font package.

use std::path::PathBuf;

use gtk::gdk;

use link_core::config as config_paths;

/// Embedded font files: `(file name, bytes)`.
pub const FONTS: &[(&str, &[u8])] = &[
    (
        "Syncopate-Regular.ttf",
        include_bytes!("../resources/fonts/Syncopate-Regular.ttf"),
    ),
    (
        "Syncopate-Bold.ttf",
        include_bytes!("../resources/fonts/Syncopate-Bold.ttf"),
    ),
    (
        "JetBrainsMono-Regular.ttf",
        include_bytes!("../resources/fonts/JetBrainsMono-Regular.ttf"),
    ),
    (
        "JetBrainsMono-Medium.ttf",
        include_bytes!("../resources/fonts/JetBrainsMono-Medium.ttf"),
    ),
    (
        "JetBrainsMono-Bold.ttf",
        include_bytes!("../resources/fonts/JetBrainsMono-Bold.ttf"),
    ),
    (
        "Inter-Regular.otf",
        include_bytes!("../resources/fonts/Inter-Regular.otf"),
    ),
    (
        "Inter-Medium.otf",
        include_bytes!("../resources/fonts/Inter-Medium.otf"),
    ),
    (
        "Inter-SemiBold.otf",
        include_bytes!("../resources/fonts/Inter-SemiBold.otf"),
    ),
];

/// The Link Code logo mark, used in the header bar and as the app icon.
pub const LOGO_SVG: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">
  <rect width="64" height="64" rx="14" fill="#000000"/>
  <path d="M32 12 L44 34 L38 34 L38 52 L26 52 L26 34 L20 34 Z" fill="#ff6600"/>
  <rect x="18" y="20" width="28" height="5" rx="2.5" fill="#ff6600" opacity="0.55"/>
</svg>
"##;

/// Write the embedded fonts next to the user's data and return the directory.
pub fn install_fonts() -> PathBuf {
    let dir = config_paths::data_dir().join("fonts");
    if std::fs::create_dir_all(&dir).is_err() {
        return dir;
    }
    for (name, bytes) in FONTS {
        let path = dir.join(name);
        let needs_write = match std::fs::metadata(&path) {
            Ok(meta) => meta.len() != bytes.len() as u64,
            Err(_) => true,
        };
        if needs_write {
            if let Err(error) = std::fs::write(&path, bytes) {
                tracing::warn!(?error, path = %path.display(), "could not install a bundled font");
            }
        }
    }
    dir
}

/// Install the app icon into a private icon theme directory and add it to the
/// theme search path.
pub fn install_icons() -> PathBuf {
    let dir = config_paths::data_dir().join("icons");
    let _ = std::fs::create_dir_all(&dir);
    let icon_path = dir.join("link-code.svg");
    let needs_write = std::fs::read_to_string(&icon_path)
        .map(|existing| existing != LOGO_SVG)
        .unwrap_or(true);
    if needs_write {
        let _ = std::fs::write(&icon_path, LOGO_SVG);
    }
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(&dir);
    }
    dir
}

/// The full stylesheet: the bundled design system plus `@font-face` rules that
/// point at the installed font files.
pub fn stylesheet() -> String {
    let font_dir = install_fonts();
    let mut css = String::new();
    for (name, family, weight) in [
        ("Syncopate-Regular.ttf", "Syncopate", 400),
        ("Syncopate-Bold.ttf", "Syncopate", 700),
        ("JetBrainsMono-Regular.ttf", "JetBrains Mono", 400),
        ("JetBrainsMono-Medium.ttf", "JetBrains Mono", 500),
        ("JetBrainsMono-Bold.ttf", "JetBrains Mono", 700),
        ("Inter-Regular.otf", "Inter", 400),
        ("Inter-Medium.otf", "Inter", 500),
        ("Inter-SemiBold.otf", "Inter", 600),
    ] {
        css.push_str(&format!(
            "@font-face {{ font-family: \"{family}\"; src: url(\"file://{}/{}\"); font-weight: {weight}; }}\n",
            font_dir.display(),
            name
        ));
    }
    css.push_str(include_str!("../resources/style.css"));
    css
}

/// Install fonts, icons and the stylesheet onto the default display.
pub fn install() {
    install_icons();
    let provider = gtk::CssProvider::new();
    provider.load_from_data(&stylesheet());
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// Apply the user's appearance preferences.
pub fn apply_preferences(appearance: &link_core::config::AppearanceSettings) {
    let _ = appearance.high_contrast;
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(true);
    }
}
