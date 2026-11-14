//! Application-wide outbound proxy configuration.
//!
//! Every HTTP client the app builds — chat providers, discovery, web tools —
//! routes through the proxy configured here. The user picks the protocol
//! (HTTP, HTTPS, SOCKS4 or SOCKS5) and may add credentials; the resulting
//! `reqwest::Proxy` is applied to each client as it is built.
//!
//! The settings live in a process-global so that a change made in Preferences
//! takes effect on the *next* client build (providers are rebuilt per turn),
//! without threading a parameter through every adapter constructor.

use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

/// The proxy protocol. `None` means "connect directly".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProxyProtocol {
    #[default]
    None,
    Http,
    Https,
    Socks4,
    Socks5,
}

impl ProxyProtocol {
    pub const ALL: [ProxyProtocol; 5] = [
        ProxyProtocol::None,
        ProxyProtocol::Http,
        ProxyProtocol::Https,
        ProxyProtocol::Socks4,
        ProxyProtocol::Socks5,
    ];

    /// Label shown in the Preferences combo box.
    pub fn label(self) -> &'static str {
        match self {
            ProxyProtocol::None => "Direct (no proxy)",
            ProxyProtocol::Http => "HTTP",
            ProxyProtocol::Https => "HTTPS",
            ProxyProtocol::Socks4 => "SOCKS4",
            ProxyProtocol::Socks5 => "SOCKS5",
        }
    }

    /// URL scheme understood by `reqwest::Proxy`.
    pub fn scheme(self) -> &'static str {
        match self {
            ProxyProtocol::None => "",
            ProxyProtocol::Http => "http",
            ProxyProtocol::Https => "https",
            ProxyProtocol::Socks4 => "socks4",
            ProxyProtocol::Socks5 => "socks5h",
        }
    }
}

/// Proxy settings persisted in `config.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxySettings {
    pub protocol: ProxyProtocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self {
            protocol: ProxyProtocol::None,
            host: String::new(),
            // A conventional HTTP proxy port; still disabled until a protocol
            // and host are chosen.
            port: 8080,
            username: String::new(),
            password: String::new(),
        }
    }
}

impl ProxySettings {
    /// True when a protocol and endpoint are set, so a proxy should be used.
    pub fn is_enabled(&self) -> bool {
        self.protocol != ProxyProtocol::None
            && !self.host.trim().is_empty()
            && self.port > 0
    }

    /// The fully-qualified proxy URL, with credentials percent-encoded.
    ///
    /// Returns `None` when the proxy is disabled or the host/port are invalid.
    pub fn url(&self) -> Option<String> {
        if !self.is_enabled() {
            return None;
        }
        let host = self.host.trim();
        // Reject anything that is not a bare host so a pasted `http://host` or
        // `host:port` still works: strip a scheme and any path.
        let host = host
            .split("://")
            .last()
            .unwrap_or(host)
            .split('/')
            .next()
            .unwrap_or(host)
            .trim();
        let host = host.rsplit_once(':').map(|(head, _)| head).unwrap_or(host);
        if host.is_empty() {
            return None;
        }
        let mut url = url::Url::parse(&format!("{}://{host}:{}", self.protocol.scheme(), self.port))
            .ok()?;
        if !self.username.is_empty() {
            // `set_username` percent-encodes; it only fails for URLs that
            // cannot have a userinfo (none of the proxy schemes).
            let _ = url.set_username(&self.username);
            let _ = url.set_password(Some(&self.password));
        }
        Some(url.to_string())
    }
}

static PROXY: OnceLock<RwLock<ProxySettings>> = OnceLock::new();

fn slot() -> &'static RwLock<ProxySettings> {
    PROXY.get_or_init(|| RwLock::new(ProxySettings::default()))
}

/// Install the current proxy configuration process-wide.
pub fn set(settings: ProxySettings) {
    if let Ok(mut guard) = slot().write() {
        *guard = settings;
    }
}

/// Snapshot of the active proxy configuration.
pub fn current() -> ProxySettings {
    slot()
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_default()
}

/// Apply the configured proxy to a `reqwest` client builder.
///
/// An invalid configuration is logged and ignored rather than aborting the
/// build, so a typo never leaves the user unable to reach any model.
pub fn apply(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    let Some(url) = current().url() else {
        return builder;
    };
    match reqwest::Proxy::all(&url) {
        Ok(proxy) => builder.proxy(proxy),
        Err(error) => {
            tracing::warn!(%error, %url, "ignoring invalid proxy configuration");
            builder
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_when_protocol_is_none() {
        let settings = ProxySettings {
            host: "127.0.0.1".to_string(),
            port: 8080,
            ..Default::default()
        };
        assert!(!settings.is_enabled());
        assert!(settings.url().is_none());
    }

    #[test]
    fn disabled_when_host_or_port_missing() {
        let mut settings = ProxySettings {
            protocol: ProxyProtocol::Http,
            port: 8080,
            ..Default::default()
        };
        assert!(!settings.is_enabled());
        settings.host = "proxy.local".to_string();
        settings.port = 0;
        assert!(!settings.is_enabled());
    }

    #[test]
    fn builds_a_plain_http_proxy_url() {
        let settings = ProxySettings {
            protocol: ProxyProtocol::Http,
            host: "proxy.local".to_string(),
            port: 3128,
            ..Default::default()
        };
        assert_eq!(settings.url().as_deref(), Some("http://proxy.local:3128/"));
    }

    #[test]
    fn encodes_credentials() {
        let settings = ProxySettings {
            protocol: ProxyProtocol::Socks5,
            host: "proxy.local".to_string(),
            port: 1080,
            username: "user name".to_string(),
            password: "p@ss:word".to_string(),
        };
        let url = settings.url().unwrap();
        assert!(url.starts_with("socks5h://"));
        assert!(url.contains("user%20name"));
        assert!(url.contains("p%40ss%3Aword"));
    }

    #[test]
    fn strips_pasted_scheme_and_port() {
        let settings = ProxySettings {
            protocol: ProxyProtocol::Https,
            host: "http://proxy.local:8080/".to_string(),
            port: 8443,
            ..Default::default()
        };
        assert_eq!(settings.url().as_deref(), Some("https://proxy.local:8443/"));
    }

    #[test]
    fn round_trips_through_json() {
        let settings = ProxySettings {
            protocol: ProxyProtocol::Socks4,
            host: "10.0.0.1".to_string(),
            port: 1080,
            username: "u".to_string(),
            password: "p".to_string(),
        };
        let text = serde_json::to_string(&settings).unwrap();
        let parsed: ProxySettings = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, settings);
    }
}
