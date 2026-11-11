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
//! * events reach widgets through an `async_channel`, drained with
//!   `glib::spawn_future_local`.

pub mod app;
pub mod markdown;
pub mod style;
pub mod window;

/// Entry point used by the `link-code` binary.
pub fn run() -> gtk::glib::ExitCode {
    app::run()
}

/// Application id used for `.desktop` integration and the GApplication name.
pub const APP_ID: &str = "org.linkecosystem.LinkCode";

/// Human-readable version string shown in About.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
