//! Bundled assets: fonts, the app icon and the global stylesheet.
//!
//! Link Code never relies on the system theme. The three brand families are
//! embedded in the binary and written to `~/.local/share/fonts` on first run,
//! where fontconfig picks them up, then referenced by name from the stylesheet.
//! That keeps the look identical on a bare XFCE box and on a fully themed
//! GNOME 46 desktop, without shipping a font package.
//!
//! GTK's CSS dialect has no `@font-face` rule, so the fonts must reach Pango
//! through fontconfig instead of the style sheet.

use std::path::PathBuf;

use gtk::gdk;
use gtk::glib;

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

/// Write the embedded fonts into the user's fontconfig directory and return it.
///
/// `~/.local/share/fonts` is scanned by fontconfig by default, so the families
/// become resolvable by name for Pango/GTK.
pub fn install_fonts() -> PathBuf {
    let dir = glib::user_data_dir().join("fonts");
    if std::fs::create_dir_all(&dir).is_err() {
        return dir;
    }
    let mut wrote_any = false;
    for (name, bytes) in FONTS {
        let path = dir.join(name);
        let needs_write = match std::fs::metadata(&path) {
            Ok(meta) => meta.len() != bytes.len() as u64,
            Err(_) => true,
        };
        if needs_write {
            match std::fs::write(&path, bytes) {
                Ok(()) => wrote_any = true,
                Err(error) => {
                    tracing::warn!(?error, path = %path.display(), "could not install a bundled font")
                }
            }
        }
    }
    // Refresh fontconfig's cache for the directory we just populated. This is
    // best-effort: without `fc-cache` the fonts still resolve on the next login.
    if wrote_any {
        let _ = std::process::Command::new("fc-cache")
            .arg("-f")
            .arg(&dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
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

/// The bundled design system, copied verbatim.
pub fn stylesheet() -> String {
    include_str!("../resources/style.css").to_string()
}

/// Install fonts, icons and the stylesheet onto the default display.
pub fn install() {
    install_fonts();
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
        // libadwaita drives its own windows from `adw::StyleManager` below.
        // Plain `gtk::Window`s (dialogs) follow the GTK theme instead, so pin
        // the dark variant explicitly; without this the command palette and
        // models dialog render on white.
        if settings
            .gtk_theme_name()
            .map(|name| name != "Adwaita-dark")
            .unwrap_or(true)
        {
            settings.set_gtk_theme_name(Some("Adwaita-dark"));
        }
    }
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
}
