//! Ordered write middleware. Its borrowed board executes within the existing transaction,
//! so responses cannot deadlock by acquiring the board clock for a second time.
use std::{fmt::Debug, sync::Arc};

use async_trait::async_trait;
use engage_core::{BoardError, Principal, Write, WriteDecision, WriteKind, WriteSource};
use serde_json::{Value, json};

use crate::{db::Tx, messages, public, read_message};

/// Middleware may transform, refuse, or respond to an authorized post or edit.
/// Implementations should avoid long network operations while the write is pending.
#[async_trait]
pub trait Middleware: Debug + Send + Sync {
    /// Runs once per newly admitted write. A landed idempotent retry bypasses it.
    async fn handle(
        &self,
        context: &mut MiddlewareContext<'_, '_>,
    ) -> Result<WriteDecision, BoardError>;
}

/// The pending write, its provenance, and a transaction-local board capability.
/// Use `board()` for responses rather than re-entering the owning `Board` handle.
pub struct MiddlewareContext<'a, 'db> {
    write: Write,
    author: Value,
    source: WriteSource,
    tx: &'a mut Tx<'db>,
    responses: Vec<String>,
}

impl Debug for MiddlewareContext<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MiddlewareContext")
            .field("kind", &self.write.kind)
            .field("channel", &self.write.channel)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl<'a, 'db> MiddlewareContext<'a, 'db> {
    pub fn write(&self) -> &Write {
        &self.write
    }
    pub fn author(&self) -> &Value {
        &self.author
    }
    pub fn source(&self) -> &WriteSource {
        &self.source
    }

    /// Records the reason returned to the writer if this message is dropped.
    pub fn respond(&mut self, text: impl Into<String>) {
        self.responses.push(text.into());
    }

    /// A borrowed board acting as `board:middleware`; its writes bypass middleware.
    pub fn board<'b>(&'b mut self) -> MiddlewareBoard<'b, 'db> {
        MiddlewareBoard { tx: self.tx }
    }
}

/// A board capability bound to the enclosing write transaction.
pub struct MiddlewareBoard<'a, 'db> {
    tx: &'a mut Tx<'db>,
}
impl Debug for MiddlewareBoard<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MiddlewareBoard")
    }
}
fn identity() -> Principal {
    Principal {
        id: "board:middleware".into(),
        handle: "middleware".into(),
        name: "Board middleware".into(),
        ..Principal::default()
    }
}
impl MiddlewareBoard<'_, '_> {
    /// Posts a board response in the same transaction and skips the middleware chain.
    /// # Errors
    /// Returns validation or storage errors; none of the enclosing writes commit on error.
    pub async fn post(&mut self, channel: &str, text: &str) -> Result<Value, BoardError> {
        messages::post(
            self.tx,
            &identity(),
            &json!({"channel":channel,"text":text}),
        )
        .await
    }
    /// Replies to an existing thread in the same transaction.
    /// # Errors
    /// Returns `message_not_found` for an unknown thread, or validation/storage errors.
    pub async fn reply(&mut self, re: &str, text: &str) -> Result<Value, BoardError> {
        messages::post(self.tx, &identity(), &json!({"re":re,"text":text})).await
    }
    /// Reads a message through the middleware's board capability.
    /// # Errors
    /// Returns `message_not_found` or a storage error.
    pub async fn message(&mut self, id: &str) -> Result<Value, BoardError> {
        read_message(self.tx, &identity(), id).await.map(public)
    }
}

/// Runs a chain without exposing the SQL transaction to application middleware.
pub(crate) async fn run(
    tx: &mut Tx<'_>,
    principal: &Principal,
    mut write: Write,
    source: WriteSource,
) -> Result<Write, BoardError> {
    if principal.id == "board:middleware" {
        return Ok(write);
    }
    let chain: Vec<Arc<dyn Middleware>> = tx.middleware.clone();
    for handler in chain {
        let mut context = MiddlewareContext {
            write,
            author: crate::actor(principal),
            source: source.clone(),
            tx,
            responses: Vec::new(),
        };
        let decision = handler.handle(&mut context).await?;
        write = context.write;
        match decision {
            WriteDecision::Continue(patch) => {
                if let Some(text) = patch.text {
                    write.text = text;
                }
                if write.kind == WriteKind::Post {
                    write.labels.extend(patch.labels);
                }
            }
            WriteDecision::Drop => {
                return Err(BoardError::new(
                    422,
                    "dropped",
                    if context.responses.is_empty() {
                        "the board dropped this message".to_owned()
                    } else {
                        context.responses.join("\n")
                    },
                ));
            }
            _ => {
                return Err(BoardError::new(
                    500,
                    "middleware_error",
                    "unsupported middleware decision",
                ));
            }
        }
    }
    if write.text.trim().is_empty()
        && (write.kind == WriteKind::Edit || write.attachments.is_empty())
    {
        return Err(BoardError::new(
            422,
            "dropped",
            "the board dropped this message",
        ));
    }
    Ok(write)
}
