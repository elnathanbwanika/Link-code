//! Error type shared by every Link Code crate.

use std::path::PathBuf;

/// Convenience alias used throughout the workspace.
pub type Result<T, E = LinkError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("path `{path}` escapes the workspace root `{root}`")]
    PathEscape { path: PathBuf, root: PathBuf },

    #[error("no such file or directory: {0}")]
    NotFound(PathBuf),

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    IoPlain(#[from] std::io::Error),

    #[error("blocked by the Link Code permission policy: {0}")]
    Denied(String),

    #[error("unknown tool `{0}`")]
    UnknownTool(String),

    #[error("invalid arguments for `{tool}`: {message}")]
    InvalidArgs { tool: String, message: String },

    #[error("edit failed: {0}")]
    EditFailed(String),

    #[error("model error: {0}")]
    Model(String),

    #[error("provider returned HTTP {status}: {body}")]
    Provider { status: u16, body: String },

    #[error("operation cancelled")]
    Cancelled,

    #[error("session error: {0}")]
    Session(String),

    #[error("settings error: {0}")]
    Settings(String),

    #[error("{0}")]
    Other(String),
}

impl LinkError {
    /// Build an [`LinkError::Io`] carrying the offending path.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::Other(message.into())
    }

    /// True when the error is a "user pressed Stop" cancellation, which callers
    /// generally want to swallow rather than surface as a red error block.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, LinkError::Cancelled)
    }
}

impl From<serde_json::Error> for LinkError {
    fn from(value: serde_json::Error) -> Self {
        LinkError::Other(format!("serialisation error: {value}"))
    }
}
