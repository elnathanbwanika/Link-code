//! One active API key per base URL, with automatic failover.
//!
//! A [`RotatingProvider`] wraps the normal adapter for a [`ModelConfig`]. It
//! keeps the endpoint's key pool in memory and transparently retries with the
//! next key when the provider says the current one is rejected, rate limited or
//! out of quota. The caller (the agent loop) only sees the successful stream,
//! so a turn keeps running instead of stopping at the first exhausted key.
//!
//! Key material is supplied by the caller; this module never writes it to disk.
//! Metadata changes (which key is active, which ones are exhausted) are reported
//! through a [`KeyPoolObserver`] so the UI can persist them in `models.json`.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::{LinkModelError, ModelResult};
use crate::provider::{provider_for, ConnectionReport, ModelProvider, ModelStream, ProviderFactory};
use crate::registry::{ApiKeyEntry, ApiKeyPool, ModelConfig, ProviderKind};
use crate::types::{Capabilities, ChatRequest, ModelInfo, Usage};

/// A key with its in-memory secret.
#[derive(Clone)]
pub struct KeySlot {
    pub id: String,
    pub label: String,
    pub key: String,
    pub exhausted: bool,
    pub exhausted_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for KeySlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeySlot")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("key", &"***")
            .field("exhausted", &self.exhausted)
            .finish()
    }
}

/// The live pool for one base URL.
#[derive(Clone, Debug, Default)]
pub struct KeyPool {
    pub id: String,
    pub active: usize,
    pub slots: Vec<KeySlot>,
}

impl KeyPool {
    pub fn new(id: impl Into<String>, active: usize, slots: Vec<KeySlot>) -> Self {
        let mut pool = Self {
            id: id.into(),
            active,
            slots,
        };
        pool.clamp();
        pool
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    fn clamp(&mut self) {
        if self.slots.is_empty() {
            self.active = 0;
        } else {
            self.active = self.active.min(self.slots.len() - 1);
        }
    }

    /// Metadata-only view persisted in the registry.
    pub fn snapshot(&self) -> ApiKeyPool {
        ApiKeyPool {
            keys: self
                .slots
                .iter()
                .map(|slot| ApiKeyEntry {
                    id: slot.id.clone(),
                    label: slot.label.clone(),
                    exhausted: slot.exhausted,
                    exhausted_at: slot.exhausted_at,
                })
                .collect(),
            active: self.active,
        }
    }

    /// The index requests should try first: the active key when healthy, else
    /// the first healthy one, else the active key so all-exhausted pools still
    /// probe for a key whose quota has reset.
    fn effective_active(&self) -> Option<usize> {
        if self.slots.is_empty() {
            return None;
        }
        let active = self.active.min(self.slots.len() - 1);
        if !self.slots[active].exhausted {
            Some(active)
        } else {
            self.slots
                .iter()
                .position(|slot| !slot.exhausted)
                .or(Some(active))
        }
    }

    /// Pick the next untried key, preferring healthy ones. Exhausted keys are
    /// still probed after every healthy key has been tried, so a quota that
    /// resets silently starts working again without user action.
    fn next_candidate(&self, tried: &[bool]) -> Option<usize> {
        let len = self.slots.len();
        if len == 0 {
            return None;
        }
        let start = self.effective_active().unwrap_or(0);
        for step in 0..len {
            let index = (start + step) % len;
            if !tried.get(index).copied().unwrap_or(true) && !self.slots[index].exhausted {
                return Some(index);
            }
        }
        for step in 0..len {
            let index = (start + step) % len;
            if !tried.get(index).copied().unwrap_or(true) {
                return Some(index);
            }
        }
        None
    }

    fn mark_exhausted(&mut self, index: usize) -> bool {
        let Some(slot) = self.slots.get_mut(index) else {
            return false;
        };
        let changed = !slot.exhausted;
        slot.exhausted = true;
        slot.exhausted_at = Some(Utc::now());
        self.active = self.next_available(index);
        changed
    }

    fn next_available(&self, from: usize) -> usize {
        let len = self.slots.len();
        if len == 0 {
            return 0;
        }
        for step in 1..=len {
            let index = (from + step) % len;
            if !self.slots[index].exhausted {
                return index;
            }
        }
        (from + 1) % len
    }

    fn mark_usable(&mut self, index: usize) -> bool {
        let Some(slot) = self.slots.get_mut(index) else {
            return false;
        };
        let changed = slot.exhausted || slot.exhausted_at.is_some();
        slot.exhausted = false;
        slot.exhausted_at = None;
        changed
    }
}

/// A pool change worth persisting and, when `switched` is set, worth telling the
/// user about.
#[derive(Debug, Clone)]
pub struct KeyPoolUpdate {
    pub pool_id: String,
    pub pool: ApiKeyPool,
    /// The active key moved because another one ran out.
    pub switched: bool,
    /// A previously exhausted key worked again.
    pub recovered: bool,
    /// Label of the key that just exhausted, for a plain-language notice.
    pub exhausted_label: Option<String>,
}

/// Receives pool changes from the async provider thread. Implementations must
/// forward the snapshot to the UI thread rather than touching GTK state here.
pub type KeyPoolObserver = Arc<dyn Fn(KeyPoolUpdate) + Send + Sync>;

struct DefaultFactory;

impl ProviderFactory for DefaultFactory {
    fn create(&self, config: &ModelConfig, api_key: Option<String>) -> Arc<dyn ModelProvider> {
        provider_for(config, api_key)
    }
}

/// The provider the agent loop sees. It owns the key pool and rotates keys
/// before handing back a stream.
pub struct RotatingProvider {
    config: ModelConfig,
    pool: Arc<Mutex<KeyPool>>,
    observer: Option<KeyPoolObserver>,
    factory: Arc<dyn ProviderFactory>,
}

impl RotatingProvider {
    pub fn new(config: ModelConfig, pool: KeyPool, observer: Option<KeyPoolObserver>) -> Self {
        Self {
            config,
            pool: Arc::new(Mutex::new(pool)),
            observer,
            factory: Arc::new(DefaultFactory),
        }
    }

    /// Test seam: build inner providers through a custom factory.
    pub fn with_factory(
        config: ModelConfig,
        pool: KeyPool,
        observer: Option<KeyPoolObserver>,
        factory: Arc<dyn ProviderFactory>,
    ) -> Self {
        Self {
            config,
            pool: Arc::new(Mutex::new(pool)),
            observer,
            factory,
        }
    }

    /// Current pool snapshot (used by tests and by the UI after a turn).
    pub fn snapshot(&self) -> ApiKeyPool {
        self.lock().snapshot()
    }

    fn lock(&self) -> MutexGuard<'_, KeyPool> {
        self.pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn build(&self, pool: &KeyPool, index: Option<usize>) -> Arc<dyn ModelProvider> {
        let key = index
            .and_then(|index| pool.slots.get(index))
            .map(|slot| slot.key.trim().to_string())
            .filter(|key| !key.is_empty());
        self.factory.create(&self.config, key)
    }

    fn record_success(&self, index: usize) {
        let (update, should_notify) = {
            let mut pool = self.lock();
            let recovered = pool.mark_usable(index);
            let switched = pool.active != index;
            pool.active = index;
            let update = KeyPoolUpdate {
                pool_id: pool.id.clone(),
                pool: pool.snapshot(),
                switched: false,
                recovered,
                exhausted_label: None,
            };
            (update, recovered || switched)
        };
        if should_notify {
            if let Some(observer) = &self.observer {
                observer(update);
            }
        }
    }

    fn record_exhausted(&self, index: usize) {
        let (label, update) = {
            let mut pool = self.lock();
            let label = pool
                .slots
                .get(index)
                .map(|slot| slot.label.clone())
                .unwrap_or_else(|| "API key".to_string());
            let changed = pool.mark_exhausted(index);
            let switched = changed
                && pool
                    .slots
                    .iter()
                    .enumerate()
                    .any(|(other, slot)| other != index && !slot.exhausted);
            let update = KeyPoolUpdate {
                pool_id: pool.id.clone(),
                pool: pool.snapshot(),
                switched,
                recovered: false,
                exhausted_label: Some(label.clone()),
            };
            (label, if changed { Some(update) } else { None })
        };
        let _ = label;
        if let (Some(observer), Some(update)) = (&self.observer, update) {
            observer(update);
        }
    }
}

#[async_trait]
impl ModelProvider for RotatingProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn label(&self) -> &str {
        &self.config.display_name
    }

    fn provider_kind(&self) -> ProviderKind {
        self.config.provider
    }

    fn capabilities(&self) -> Capabilities {
        self.config.capabilities
    }

    fn context_window(&self) -> usize {
        self.config.context_window
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        let len = self.lock().slots.len();
        if len == 0 {
            return self.build(&KeyPool::default(), None).list_models().await;
        }
        let mut tried = vec![false; len];
        let mut last_error = None;
        loop {
            // Drop the lock before awaiting: the candidate is a plain index.
            let Some(index) = ({
                let pool = self.lock();
                pool.next_candidate(&tried)
            }) else {
                break;
            };
            tried[index] = true;
            let provider = {
                let pool = self.lock();
                self.build(&pool, Some(index))
            };
            match provider.list_models().await {
                Ok(models) => {
                    self.record_success(index);
                    return Ok(models);
                }
                Err(error) if error.is_key_exhaustion() => {
                    self.record_exhausted(index);
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| LinkModelError::Other("no API key could be used".into())))
    }

    async fn stream_chat(&self, request: ChatRequest) -> ModelResult<ModelStream> {
        let len = self.lock().slots.len();
        if len == 0 {
            return self.build(&KeyPool::default(), None).stream_chat(request).await;
        }
        let mut tried = vec![false; len];
        let mut last_error = None;
        loop {
            // Drop the lock before awaiting: the candidate is a plain index.
            let Some(index) = ({
                let pool = self.lock();
                pool.next_candidate(&tried)
            }) else {
                break;
            };
            tried[index] = true;
            let provider = {
                let pool = self.lock();
                self.build(&pool, Some(index))
            };
            match provider.stream_chat(request.clone()).await {
                Ok(stream) => {
                    self.record_success(index);
                    return Ok(stream);
                }
                Err(error) if error.is_key_exhaustion() => {
                    tracing::warn!(
                        key = %self.lock().slots.get(index).map(|slot| slot.label.as_str()).unwrap_or("?"),
                        %error,
                        "API key ran out; trying the next key"
                    );
                    self.record_exhausted(index);
                    last_error = Some(error);
                    // Give a heavily rate-limited endpoint a moment before the
                    // next key is tried.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            LinkModelError::Auth {
                status: 401,
                url: self.config.base_url.clone(),
                body: "every API key for this endpoint was rejected or is out of quota".to_string(),
            }
        }))
    }

    async fn complete(&self, request: ChatRequest) -> ModelResult<(String, Usage)> {
        let len = self.lock().slots.len();
        if len == 0 {
            return self.build(&KeyPool::default(), None).complete(request).await;
        }
        let mut tried = vec![false; len];
        let mut last_error = None;
        loop {
            // Drop the lock before awaiting: the candidate is a plain index.
            let Some(index) = ({
                let pool = self.lock();
                pool.next_candidate(&tried)
            }) else {
                break;
            };
            tried[index] = true;
            let provider = {
                let pool = self.lock();
                self.build(&pool, Some(index))
            };
            match provider.complete(request.clone()).await {
                Ok(result) => {
                    self.record_success(index);
                    return Ok(result);
                }
                Err(error) if error.is_key_exhaustion() => {
                    self.record_exhausted(index);
                    last_error = Some(error);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| LinkModelError::Other("no API key could be used".into())))
    }

    async fn test_connection(&self) -> ModelResult<ConnectionReport> {
        let started = std::time::Instant::now();
        let mut request = ChatRequest::new(&self.config.model_id);
        request.messages = vec![crate::types::ChatMessage::user("ping")];
        request.max_tokens = Some(4);
        request.temperature = Some(0.0);
        let (text, usage) = self.complete(request).await?;
        let elapsed = started.elapsed();
        Ok(ConnectionReport {
            model: self.config.badge(),
            latency_ms: elapsed.as_millis() as u64,
            tokens_per_second: usage.tokens_per_second(elapsed),
            tool_calling: self.config.capabilities.tools,
            streaming: self.config.capabilities.streaming,
            message: format!("Replied with {}", crate::sse::truncate(text.trim(), 60)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ConnectionReport;
    use crate::types::{ChatMessage, StreamEvent};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeFactory {
        calls: AtomicUsize,
        /// Keys that should fail with a quota error.
        exhausted: Mutex<Vec<String>>,
        streams_started: AtomicUsize,
    }

    impl FakeFactory {
        fn new(exhausted: &[&str]) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                exhausted: Mutex::new(exhausted.iter().map(|key| key.to_string()).collect()),
                streams_started: AtomicUsize::new(0),
            }
        }
    }

    struct FakeProvider {
        key: Option<String>,
        should_fail: bool,
    }

    #[async_trait]
    impl ModelProvider for FakeProvider {
        fn id(&self) -> &str {
            "fake"
        }

        fn label(&self) -> &str {
            "fake"
        }

        fn provider_kind(&self) -> ProviderKind {
            ProviderKind::OpenAiCompatible
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }

        fn context_window(&self) -> usize {
            4096
        }

        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            if self.should_fail {
                return Err(LinkModelError::RateLimit {
                    retry_after: None,
                    body: "quota exhausted".into(),
                });
            }
            Ok(vec![ModelInfo::new("fake-model")])
        }

        async fn stream_chat(&self, _request: ChatRequest) -> ModelResult<ModelStream> {
            if self.should_fail {
                return Err(LinkModelError::Auth {
                    status: 401,
                    url: "https://example.test".into(),
                    body: "invalid key".into(),
                });
            }
            let key = self.key.clone().unwrap_or_default();
            let stream = futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(format!("ok:{key}"))),
                Ok(StreamEvent::Done {
                    finish_reason: crate::types::FinishReason::Stop,
                }),
            ]);
            Ok(Box::pin(stream))
        }

        async fn complete(&self, _request: ChatRequest) -> ModelResult<(String, Usage)> {
            if self.should_fail {
                return Err(LinkModelError::RateLimit {
                    retry_after: None,
                    body: "quota exhausted".into(),
                });
            }
            Ok((format!("ok:{}", self.key.clone().unwrap_or_default()), Usage::default()))
        }

        async fn test_connection(&self) -> ModelResult<ConnectionReport> {
            Ok(ConnectionReport {
                model: "fake".into(),
                latency_ms: 1,
                tokens_per_second: 0.0,
                tool_calling: true,
                streaming: true,
                message: "ok".into(),
            })
        }
    }

    impl ProviderFactory for FakeFactory {
        fn create(&self, _config: &ModelConfig, api_key: Option<String>) -> Arc<dyn ModelProvider> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.streams_started.fetch_add(1, Ordering::SeqCst);
            let should_fail = api_key
                .as_deref()
                .map(|key| {
                    self.exhausted
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|candidate| candidate == key)
                })
                .unwrap_or(false);
            Arc::new(FakeProvider {
                key: api_key,
                should_fail,
            })
        }
    }

    fn config() -> ModelConfig {
        let mut config = ModelConfig::new("test", ProviderKind::OpenAiCompatible, "m");
        config.base_url = "https://example.test/v1".into();
        config
    }

    fn pool(keys: &[&str]) -> KeyPool {
        KeyPool::new(
            "https://example.test/v1",
            0,
            keys.iter()
                .enumerate()
                .map(|(index, key)| KeySlot {
                    id: format!("k{index}"),
                    label: format!("Key {}", index + 1),
                    key: (*key).to_string(),
                    exhausted: false,
                    exhausted_at: None,
                })
                .collect(),
        )
    }

    #[tokio::test]
    async fn rotates_to_the_next_key_when_the_active_one_is_exhausted() {
        let factory = Arc::new(FakeFactory::new(&["bad"]));
        let updates = Arc::new(Mutex::new(Vec::new()));
        let observer_updates = updates.clone();
        let observer: KeyPoolObserver = Arc::new(move |update| {
            observer_updates.lock().unwrap().push(update);
        });
        let provider = RotatingProvider::with_factory(
            config(),
            pool(&["bad", "good"]),
            Some(observer),
            factory.clone(),
        );

        let stream = provider
            .stream_chat(ChatRequest::new("m"))
            .await
            .expect("the second key should answer");
        let events: Vec<_> = futures_util::StreamExt::collect::<Vec<_>>(stream)
            .await
            .into_iter()
            .map(|event| event.unwrap())
            .collect();
        assert!(matches!(&events[0], StreamEvent::TextDelta(text) if text == "ok:good"));
        assert_eq!(provider.snapshot().active, 1);
        assert!(provider.snapshot().keys[0].exhausted);
        assert!(!provider.snapshot().keys[1].exhausted);
        assert_eq!(factory.calls.load(Ordering::SeqCst), 2);
        let updates = updates.lock().unwrap();
        assert!(updates.iter().any(|update| update.switched));
    }

    #[tokio::test]
    async fn complete_continues_on_the_next_key() {
        let factory = Arc::new(FakeFactory::new(&["bad"]));
        let provider =
            RotatingProvider::with_factory(config(), pool(&["bad", "good"]), None, factory);
        let (text, _usage) = provider.complete(ChatRequest::new("m")).await.unwrap();
        assert_eq!(text, "ok:good");
        assert_eq!(provider.snapshot().active, 1);
    }

    #[tokio::test]
    async fn all_keys_failing_returns_the_last_error_and_marks_every_key() {
        let factory = Arc::new(FakeFactory::new(&["one", "two"]));
        let provider =
            RotatingProvider::with_factory(config(), pool(&["one", "two"]), None, factory);
        let error = provider.stream_chat(ChatRequest::new("m")).await.err().unwrap();
        assert!(error.is_key_exhaustion());
        let snapshot = provider.snapshot();
        assert!(snapshot.keys.iter().all(|key| key.exhausted));
    }

    #[tokio::test]
    async fn successful_key_clears_a_previous_exhausted_mark() {
        let factory = Arc::new(FakeFactory::new(&[]));
        let mut initial = pool(&["good"]);
        initial.slots[0].exhausted = true;
        let provider = RotatingProvider::with_factory(config(), initial, None, factory);
        let _stream = provider.stream_chat(ChatRequest::new("m")).await.unwrap();
        assert!(!provider.snapshot().keys[0].exhausted);
    }

    #[test]
    fn empty_pool_still_builds_a_provider() {
        let factory = Arc::new(FakeFactory::new(&[]));
        let provider =
            RotatingProvider::with_factory(config(), KeyPool::default(), None, factory.clone());
        let _ = provider.id();
        assert_eq!(factory.calls.load(Ordering::SeqCst), 0);
    }
}
