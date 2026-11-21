use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Options for a new message or reply. Exactly one of `channel` and `re` is required.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PostOptions {
    /// A new root's channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Any message in the thread to reply to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub re: Option<String>,
    /// Markdown text.
    pub text: String,
    /// Labels to add atomically with the post.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Caller-supplied idempotency key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Require this to remain the thread's newest message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Keep this post off external platforms.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_sync: bool,
    /// Already-uploaded attachment IDs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    /// External sync targets for a new root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sync: Vec<String>,
}

/// A public message view. Additional protocol fields are retained in `extra`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// Opaque message ID.
    pub id: String,
    /// Channel path.
    pub channel: String,
    /// Markdown text; empty on deletion.
    pub text: String,
    /// Thread root for a reply.
    pub re: Option<String>,
    /// Opaque thread key.
    pub thread: String,
    /// Revision required for conditional text edits.
    pub revision: u64,
    /// Version of the most recent change, including reactions and labels.
    pub version: u64,
    /// Deletion tombstone.
    pub deleted: bool,
    /// Labels.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Remaining fields, including public task state and attachments.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// A thread snapshot with its cursor.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Thread {
    /// The root message.
    pub message: Message,
    /// Replies in log order.
    pub replies: Vec<Message>,
    /// Feed position corresponding to this snapshot.
    pub cursor: String,
    /// Pending schedules, when provided by the server.
    #[serde(default)]
    pub schedules: Vec<Value>,
}

/// A channel message page.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MessagePage {
    /// Matching messages in log order.
    pub messages: Vec<Message>,
    /// Feed cursor for continued reading.
    pub cursor: String,
}

/// A feed event.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// Opaque feed cursor.
    pub cursor: String,
    /// Protocol event type.
    #[serde(rename = "type")]
    pub kind: String,
    /// Server timestamp.
    pub created_at: String,
    /// Public event payload, with claim tokens omitted.
    pub payload: Value,
    /// Updated message when the event concerns one.
    #[serde(default)]
    pub message: Option<Message>,
    /// Additional protocol fields.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Events returned by one feed read.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EventsPage {
    /// Matching events.
    pub events: Vec<Event>,
    /// End position, including events that did not match the filter.
    pub cursor: String,
}

/// A task claim's private credentials. Debug formatting redacts the token.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskClaim {
    /// Fencing epoch.
    pub epoch: u64,
    /// Secret token returned only to the claimant.
    pub token: String,
    /// Lease expiration timestamp.
    pub until: String,
}

impl fmt::Debug for TaskClaim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskClaim")
            .field("epoch", &self.epoch)
            .field("token", &"[redacted]")
            .field("until", &self.until)
            .finish()
    }
}
