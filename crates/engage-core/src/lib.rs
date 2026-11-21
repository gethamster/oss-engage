//! Public protocol identities and errors shared by Engage transports.
use serde::{Deserialize, Serialize};

/// An authenticated identity. Transports must obtain these fields from trusted credentials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Principal {
    pub id: String,
    pub handle: String,
    pub name: String,
    pub profile: Option<String>,
    pub groups: Vec<String>,
    pub owner: bool,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub facts: serde_json::Value,
}

impl Default for Principal {
    fn default() -> Self {
        Self {
            id: "local:user:owner".into(),
            handle: "owner".into(),
            name: "Local owner".into(),
            profile: None,
            groups: vec![],
            owner: true,
            scope: None,
            facts: serde_json::Value::Null,
        }
    }
}

impl Principal {
    /// The actor for this credential, narrowed to its profile when one was selected.
    pub fn actor(&self) -> String {
        self.profile
            .as_ref()
            .map_or_else(|| self.id.clone(), |p| format!("{}+{p}", self.id))
    }
}

/// A protocol error safe to return across the HTTP boundary.
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct BoardError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl BoardError {
    pub fn new(status: u16, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(400, "invalid_request", message)
    }
    pub fn missing(code: &str) -> Self {
        Self::new(404, code, "resource not found")
    }
    pub fn conflict(code: &str, message: &str) -> Self {
        Self::new(409, code, message)
    }
    /// Board's HTTP error envelope.
    pub fn body(&self) -> serde_json::Value {
        serde_json::json!({"error":{"code":self.code,"message":self.message}})
    }
}

mod middleware;
pub use middleware::{Write, WriteDecision, WriteKind, WritePatch, WriteSource};
