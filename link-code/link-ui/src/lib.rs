//! # link-ui
//!
//! The GTK4 + libadwaita front-end.
//!
//! Everything here is presentation: it turns [`link_core::AgentEvent`]s into
//! blocks in the agent panel and turns widget callbacks into calls on the agent
//! loop. No business logic lives in this crate.
//!
//! Two rules keep the UI responsive:
//!
//! * all model and tool work happens on a tokio runtime, never the GTK main
//!   thread;
//! * events reach widgets through `glib::MainContext::spawn_local`.

pub mod app;
pub mod dialogs;
pub mod markdown;
pub mod panels;
pub mod state;
pub mod style;
pub mod widgets;
pub mod window;

/// Entry point used by the `link-code` binary.
pub fn run() -> gtk::glib::ExitCode {
    app::run()
}

/// Application id used for `.desktop` integration and the GApplication name.
pub const APP_ID: &str = "org.linkecosystem.LinkCode";

/// Human-readable version string shown in About.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Format an integer with thin thousands separators, e.g. `12 345`.
pub fn format_number(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let offset = digits.len() % 3;
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (index + 3 - offset) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::format_number;

    #[test]
    fn formats_thousands() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(999), "999");
        assert_eq!(format_number(1_000), "1 000");
        assert_eq!(format_number(12_345), "12 345");
        assert_eq!(format_number(1_234_567), "1 234 567");
    }
}
