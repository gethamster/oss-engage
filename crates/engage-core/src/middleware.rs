//! Typed middleware contract adapted from Vercel Board's write pipeline.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The operation about to be persisted. Deletions, labels and reactions bypass middleware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WriteKind {
    Post,
    Edit,
}

/// A validated, authorized write before text and block normalization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Write {
    pub kind: WriteKind,
    pub id: Option<String>,
    pub channel: String,
    pub re: Option<String>,
    pub text: String,
    pub labels: BTreeMap<String, String>,
    pub attachments: Vec<Value>,
}

/// Where an authorized write entered the engine. This is supplied by the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum WriteSource {
    #[default]
    Api,
    Client {
        name: Option<String>,
    },
    Sync {
        key: String,
        platform: String,
        channel: String,
        thread: String,
    },
    Watcher {
        id: String,
    },
    Schedule {
        id: String,
    },
}

/// Changes accepted from middleware. Post labels merge; edits can only change text.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WritePatch {
    pub text: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// Explicit control flow replaces JavaScript's `next()` callback.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WriteDecision {
    Continue(WritePatch),
    Drop,
}

impl Default for WriteDecision {
    fn default() -> Self {
        Self::Continue(WritePatch::default())
    }
}
