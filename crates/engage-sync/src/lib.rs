//! Rust-native, signature-verified synchronization providers for Engage.
//! Credentials are resolved per operation; durable inboxes and receipts belong to Engage.
mod backfill;
mod config;
mod credentials;
mod github;
mod http;
mod linear;
mod linear_read;
mod model;
mod slack;
mod slack_links;
mod worker;

use async_trait::async_trait;
use serde_json::Value;
use std::{collections::BTreeMap, fmt, sync::Arc};

pub use config::{ProviderConfig, SyncConfig};
pub use credentials::{
    CredentialResolver, Credentials, EnvironmentCredentials, Secret, StaticCredentials,
};
pub use github::GitHub;
pub use linear::Linear;
pub use model::{
    Backfill, BackfillProgress, Capabilities, Delivery, Incoming, ProviderFile, ProviderPost,
    ResolvedTarget, SourceJoin, SourceRead, SyncRoot, TargetKind, WebhookRequest, WebhookResponse,
};
pub use slack::Slack;
pub use worker::{BoardApi, SyncWorker};

/// Errors contain no response bodies or secrets from a platform.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("invalid webhook signature")]
    Signature,
    #[error("webhook timestamp is outside the allowed window")]
    Stale,
    #[error("invalid platform payload: {0}")]
    Payload(String),
    #[error("credential resolution failed: {0}")]
    Credentials(String),
    #[error("platform request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("platform rejected request: {code} (HTTP {status})")]
    Platform { status: u16, code: String },
    #[error("operation is unsupported: {0}")]
    Unsupported(&'static str),
    #[error("board request failed: {code} (HTTP {status})")]
    Board { status: u16, code: String },
    #[error("no provider registered for {0}")]
    UnknownProvider(String),
}

/// Platforms normalize webhooks into Board's external event contract.
#[async_trait]
pub trait Provider: Send + Sync + fmt::Debug {
    fn platform(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    fn outbound_enabled(&self) -> bool {
        true
    }
    async fn receive(
        &self,
        key: &str,
        request: &WebhookRequest,
    ) -> Result<WebhookResponse, SyncError>;
    async fn resolve_target(&self, target: &str) -> Result<ResolvedTarget, SyncError>;
    async fn post(
        &self,
        target: &str,
        kind: TargetKind,
        body: &ProviderPost,
    ) -> Result<Delivery, SyncError>;
    async fn edit(
        &self,
        _thread: &str,
        _message: &str,
        _body: &ProviderPost,
    ) -> Result<(), SyncError> {
        Err(SyncError::Unsupported("edit"))
    }
    async fn delete(&self, _thread: &str, _message: &str) -> Result<(), SyncError> {
        Err(SyncError::Unsupported("delete"))
    }
    async fn react(
        &self,
        _thread: &str,
        _message: &str,
        _emoji: &str,
        _added: bool,
    ) -> Result<(), SyncError> {
        Err(SyncError::Unsupported("reaction"))
    }
    async fn typing(&self, _thread: &str, _status: Option<&str>) -> Result<(), SyncError> {
        Err(SyncError::Unsupported("typing"))
    }
    async fn read(
        &self,
        _target: &str,
        _limit: usize,
        _after: Option<&str>,
    ) -> Result<SourceRead, SyncError> {
        Err(SyncError::Unsupported("source read"))
    }
    async fn join(&self, _target: &str) -> Result<SourceJoin, SyncError> {
        Err(SyncError::Unsupported("source join"))
    }
    async fn backfill(
        &self,
        _key: &str,
        _target: &str,
        _limit: usize,
        _after: Option<&str>,
    ) -> Result<Backfill, SyncError> {
        Err(SyncError::Unsupported("historical backfill"))
    }
    async fn find_by_email(&self, _email: &str) -> Result<Option<Value>, SyncError> {
        Ok(None)
    }
    async fn fetch_root(&self, _thread: &str) -> Result<Option<SyncRoot>, SyncError> {
        Ok(None)
    }
    fn locate(&self, _url: &str) -> Option<(String, Option<String>)> {
        None
    }
    async fn fetch_link(&self, _link: &Value, _linker: &Value) -> Result<Value, SyncError> {
        Ok(serde_json::json!({"snapshot":null,"unavailable":"unsupported"}))
    }
    async fn fetch_file(&self, _file: &Value) -> Result<Option<Vec<u8>>, SyncError> {
        Ok(None)
    }
    async fn url(&self, _thread: &str, _message: &str) -> Result<Option<String>, SyncError> {
        Ok(None)
    }
    fn mention_by_id(&self, _actor: &str) -> Option<String> {
        None
    }
}

#[derive(Clone)]
struct Registration {
    provider: Arc<dyn Provider>,
    principal: String,
    channels: BTreeMap<String, String>,
}

/// Named provider instances, each bound to a trusted board service principal.
#[derive(Clone, Default)]
pub struct Registry {
    entries: BTreeMap<String, Registration>,
}
impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(
                self.entries
                    .iter()
                    .map(|(key, entry)| (key, entry.provider.platform())),
            )
            .finish()
    }
}
impl Registry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(
        &mut self,
        key: impl Into<String>,
        principal: impl Into<String>,
        provider: impl Provider + 'static,
    ) -> Result<(), SyncError> {
        let key = key.into();
        if key.is_empty()
            || key.len() > 64
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(SyncError::Payload("invalid sync key".into()));
        }
        let principal = principal.into();
        if principal.is_empty()
            || principal.len() > 300
            || principal.contains('\0')
            || self.entries.contains_key(&key)
        {
            return Err(SyncError::Payload(
                "a sync registration needs a unique key and principal".into(),
            ));
        }
        self.entries.insert(
            key,
            Registration {
                provider: Arc::new(provider),
                principal,
                channels: BTreeMap::new(),
            },
        );
        Ok(())
    }
    pub fn provider(&self, key: &str) -> Result<&dyn Provider, SyncError> {
        self.entries
            .get(key)
            .map(|entry| entry.provider.as_ref())
            .ok_or_else(|| SyncError::UnknownProvider(key.into()))
    }
    pub fn principal(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(|entry| entry.principal.as_str())
    }
    pub fn discovery(&self) -> Vec<Value> {
        self.entries
            .iter()
            .map(|(key, entry)| serde_json::json!({"key":key,"platform":entry.provider.platform()}))
            .collect()
    }
    pub async fn webhook(
        &self,
        key: &str,
        request: &WebhookRequest,
    ) -> Result<WebhookResponse, SyncError> {
        self.provider(key)?.receive(key, request).await
    }
}
