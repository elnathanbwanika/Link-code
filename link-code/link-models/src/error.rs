//! Provider errors, categorised so the UI can explain them in plain language.

use std::time::Duration;

/// Broad cause of a provider failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    Connection,
    Auth,
    RateLimit,
    ContextOverflow,
    Malformed,
    Config,
    Cancelled,
    Other,
}

#[derive(Debug, thiserror::Error)]
pub enum LinkModelError {
    #[error("could not reach {url}")]
    Connection {
        url: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("HTTP {status} from {url}: {body}")]
    Http {
        status: u16,
        url: String,
        body: String,
    },

    #[error("the endpoint rejected the API key (HTTP {status}) at {url}: {body}")]
    Auth {
        status: u16,
        url: String,
        body: String,
    },

    #[error("no API key was sent to {url} \u{2014} add one in Models (Ctrl+M)")]
    MissingApiKey { url: String },

    #[error("rate limited by the provider")]
    RateLimit {
        retry_after: Option<Duration>,
        body: String,
    },

    #[error("request exceeds the model's context window: {0}")]
    ContextOverflow(String),

    #[error("the model returned malformed output: {0}")]
    Malformed(String),

    #[error("model configuration error: {0}")]
    Config(String),

    #[error("the stream ended unexpectedly: {0}")]
    Stream(String),

    #[error("cancelled")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

pub type ModelResult<T> = Result<T, LinkModelError>;

impl LinkModelError {
    pub fn category(&self) -> ErrorCategory {
        match self {
            LinkModelError::Connection { .. } => ErrorCategory::Connection,
            LinkModelError::Auth { .. } | LinkModelError::MissingApiKey { .. } => {
                ErrorCategory::Auth
            }
            LinkModelError::RateLimit { .. } => ErrorCategory::RateLimit,
            LinkModelError::ContextOverflow(_) => ErrorCategory::ContextOverflow,
            LinkModelError::Malformed(_) => ErrorCategory::Malformed,
            LinkModelError::Config(_) => ErrorCategory::Config,
            LinkModelError::Cancelled => ErrorCategory::Cancelled,
            LinkModelError::Http { .. } | LinkModelError::Stream(_) | LinkModelError::Other(_) => {
                ErrorCategory::Other
            }
        }
    }

    /// Whether retrying the same request could plausibly succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            LinkModelError::Connection { .. } | LinkModelError::RateLimit { .. } => true,
            LinkModelError::Http { status, .. } => *status >= 500,
            _ => false,
        }
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            LinkModelError::RateLimit { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// Whether this failure means the credential is used up, rather than the
    /// request being malformed or the endpoint being unreachable.
    ///
    /// This is the signal the key-pool provider uses to move to the next key:
    /// rejected credentials (401/403), rate limits (429) and payment/quota
    /// failures (402, or a body that talks about quota, credits or billing).
    pub fn is_key_exhaustion(&self) -> bool {
        match self {
            LinkModelError::Auth { .. } | LinkModelError::RateLimit { .. } => true,
            LinkModelError::Http { status, body, .. } => {
                *status == 402 || looks_like_key_exhaustion(body)
            }
            _ => false,
        }
    }

    pub fn cancelled() -> Self {
        LinkModelError::Cancelled
    }
}

/// Build the right error variant from an HTTP status and body.
pub fn classify_http(url: &str, status: reqwest::StatusCode, headers: &reqwest::header::HeaderMap, body: String) -> LinkModelError {
    let code = status.as_u16();
    match code {
        401 | 403 => LinkModelError::Auth {
            status: code,
            url: url.to_string(),
            body: if body.trim().is_empty() {
                "(empty response body)".to_string()
            } else {
                body
            },
        },
        429 => LinkModelError::RateLimit {
            retry_after: parse_retry_after(headers),
            body,
        },
        _ => {
            if looks_like_context_overflow(&body) {
                LinkModelError::ContextOverflow(body)
            } else {
                LinkModelError::Http {
                    status: code,
                    url: url.to_string(),
                    body,
                }
            }
        }
    }
}

/// Providers disagree about how to express "too many tokens"; recognise the
/// common wordings so the UI can offer `/compact` instead of "unknown error".
pub fn looks_like_context_overflow(body: &str) -> bool {
    let lowered = body.to_ascii_lowercase();
    [
        "context length",
        "context_length_exceeded",
        "maximum context",
        "too many tokens",
        "prompt is too long",
        "input is too long",
        "exceeds the maximum",
        "request too large",
        "token limit",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

/// Phrases providers use when a key has no quota or credit left. They often
/// arrive as a generic 400/403 rather than 429, so the body is the only signal.
pub fn looks_like_key_exhaustion(body: &str) -> bool {
    let lowered = body.to_ascii_lowercase();
    [
        "insufficient_quota",
        "insufficient quota",
        "quota exceeded",
        "quota_exceeded",
        "exceeded your current quota",
        "usage limit",
        "usage_limit",
        "credit balance",
        "credit_balance",
        "out of credits",
        "no credits",
        "billing",
        "payment required",
        "account is out of",
        "key has been revoked",
        "invalid api key",
        "incorrect api key",
        "api key not valid",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    // HTTP-date form is rare; fall back to a sane default.
    Some(Duration::from_secs(5))
}
