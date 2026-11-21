//! Platform-neutral synchronization contracts.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An authenticated, normalized platform event, with a durable deduplication key.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Incoming {
    pub id: String,
    pub thread: String,
    pub event: Value,
    pub raw: Value,
}

/// The unmodified HTTP request used for platform signature verification.
#[derive(Clone, Default)]
pub struct WebhookRequest {
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub now_unix: i64,
}
impl std::fmt::Debug for WebhookRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookRequest")
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .field("body_bytes", &self.body.len())
            .field("now_unix", &self.now_unix)
            .finish()
    }
}
impl WebhookRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// A platform acknowledgment and the events that must land durably before acknowledgment.
#[derive(Clone, Debug, Default)]
pub struct WebhookResponse {
    pub status: u16,
    pub body: Value,
    pub incoming: Vec<Incoming>,
}
impl WebhookResponse {
    pub(crate) fn accepted(incoming: Vec<Incoming>) -> Self {
        Self {
            status: 200,
            body: serde_json::json!({"ok":true}),
            incoming,
        }
    }
}

/// A platform channel or existing thread.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TargetKind {
    Channel,
    Thread,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub target: String,
    pub channel: String,
    pub kind: TargetKind,
}

/// Bytes to upload; URLs and credentials are never serialized into board events.
#[derive(Clone, Debug)]
pub struct ProviderFile {
    pub data: Vec<u8>,
    pub filename: String,
    pub mime_type: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ProviderPost {
    pub markdown: String,
    pub files: Vec<ProviderFile>,
    /// Text imported from another platform cannot broadcast-mention people here.
    pub external: bool,
    pub sender_name: Option<String>,
    pub sender_avatar: Option<String>,
    /// Idempotency identity where supported by the provider.
    pub operation_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Delivery {
    pub thread: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SourceRead {
    pub channel: String,
    pub name: Option<String>,
    pub thread: Option<String>,
    pub messages: Vec<Value>,
    pub more: bool,
    pub next: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceJoin {
    pub channel: String,
    pub name: Option<String>,
    pub joined: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub starts_threads: bool,
    pub uploads_files: bool,
    pub edits: bool,
    pub deletes: bool,
    pub reactions: bool,
    pub typing: bool,
    pub joins_channels: bool,
}

/// A platform thread root fetched when the first observed event is a reply.
#[derive(Clone, Debug)]
pub struct SyncRoot {
    pub text: String,
    pub author: Value,
    pub labels: BTreeMap<String, String>,
}

/// A page of authenticated historical platform events ready for the durable inbox.
#[derive(Clone, Debug, Default)]
pub struct Backfill {
    pub incoming: Vec<Incoming>,
    pub more: bool,
    pub next: Option<String>,
}

/// Historical events durably accepted for normal inbox processing.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackfillProgress {
    pub queued: usize,
    pub more: bool,
    pub next: Option<String>,
}
