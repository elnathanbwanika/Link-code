//! # link-models
//!
//! One [`ModelProvider`](provider::ModelProvider) trait, many endpoints.
//!
//! Every provider — OpenAI-compatible servers, Ollama, Anthropic and Gemini —
//! is normalised into the same internal message and tool-call format, with SSE
//! streaming, partial-JSON tolerance, rate-limit awareness and cancellation.
//!
//! Providers never panic on malformed output: a model that emits broken JSON
//! produces a [`LinkModelError`](error::LinkModelError) that the agent loop
//! turns into a corrective retry, not a crash.

pub mod anthropic;
pub mod discovery;
pub mod error;
pub mod gemini;
pub mod keypool;
pub mod ollama;
pub mod openai;
pub mod pricing;
pub mod provider;
pub mod proxy;
pub mod registry;
pub mod sse;
pub mod toolcall;
pub mod types;

pub use error::{LinkModelError, ModelResult};
pub use keypool::{
    KeyPool, KeyPoolObserver, KeyPoolUpdate, KeySlot, RetryNotice, RetryObserver, RotatingProvider,
};
pub use openai::sanitize_api_key;
pub use provider::{ModelProvider, ModelStream, ProviderFactory};
pub use proxy::{ProxyProtocol, ProxySettings};
pub use registry::{
    ApiKeyEntry, ApiKeyPool, ModelConfig, ModelRegistry, ModelRole, ModelStats, ProviderKind,
    MAX_KEYS_PER_POOL,
};
pub use types::{
    Capabilities, ChatMessage, ChatRequest, FinishReason, ImageAttachment, ModelInfo, Role,
    StreamEvent, ToolCallRequest, Usage,
};

/// Version of the on-disk model registry format.
pub const REGISTRY_FORMAT_VERSION: u32 = 1;
