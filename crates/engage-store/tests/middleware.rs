//! Middleware is exercised through the same public mutation pipeline as transports.
use async_trait::async_trait;
use engage_core::{BoardError, Principal, WriteDecision, WritePatch};
use engage_store::{
    Board,
    middleware::{Middleware, MiddlewareContext},
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
struct Transform(Arc<AtomicUsize>);
#[async_trait]
impl Middleware for Transform {
    async fn handle(
        &self,
        context: &mut MiddlewareContext<'_, '_>,
    ) -> Result<WriteDecision, BoardError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(WriteDecision::Continue(WritePatch {
            text: Some(context.write().text.replace("private", "redacted")),
            ..WritePatch::default()
        }))
    }
}

#[tokio::test]
async fn transforms_once_and_landed_retry_bypasses_the_chain() {
    let calls = Arc::new(AtomicUsize::new(0));
    let board = Board::connect(":memory:")
        .await
        .expect("memory database")
        .with_middleware(Transform(calls.clone()));
    let p = Principal::default();
    let input = json!({"channel":"work","text":"private","id":"once"});
    let first = board
        .request(&p, "POST", "/messages", &[], input.clone())
        .await
        .expect("post");
    let again = board
        .request(&p, "POST", "/messages", &[], input)
        .await
        .expect("idempotent retry");
    assert_eq!(first["message"]["text"], "redacted");
    assert_eq!(first["message"]["id"], again["message"]["id"]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let id = first["message"]["id"].as_str().expect("post id");
    board
        .request(
            &p,
            "POST",
            &format!("/messages/{id}/edit"),
            &[],
            json!({"text":"private again"}),
        )
        .await
        .expect("edit");
    board
        .request(
            &p,
            "POST",
            &format!("/messages/{id}/reactions"),
            &[],
            json!({"emoji":"eyes"}),
        )
        .await
        .expect("reaction");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "reactions bypass middleware"
    );
}

#[derive(Debug)]
struct DropWrite;
#[async_trait]
impl Middleware for DropWrite {
    async fn handle(
        &self,
        context: &mut MiddlewareContext<'_, '_>,
    ) -> Result<WriteDecision, BoardError> {
        context.respond("Please remove the private data");
        Ok(WriteDecision::Drop)
    }
}
#[tokio::test]
async fn drop_returns_reason_without_a_partial_message() {
    let board = Board::connect(":memory:")
        .await
        .expect("memory database")
        .with_middleware(DropWrite);
    let error = board
        .request(
            &Principal::default(),
            "POST",
            "/messages",
            &[],
            json!({"channel":"work","text":"private"}),
        )
        .await
        .expect_err("dropped");
    assert_eq!(error.status, 422);
    assert_eq!(error.code, "dropped");
    assert_eq!(error.message, "Please remove the private data");
    let threads = board
        .request(&Principal::default(), "GET", "/threads", &[], Value::Null)
        .await
        .expect("threads");
    assert_eq!(threads["threads"], json!([]));
}

#[derive(Debug)]
struct Respond;
#[async_trait]
impl Middleware for Respond {
    async fn handle(
        &self,
        context: &mut MiddlewareContext<'_, '_>,
    ) -> Result<WriteDecision, BoardError> {
        context.board().post("audit", "Message accepted").await?;
        Ok(WriteDecision::default())
    }
}
#[tokio::test]
async fn middleware_board_response_does_not_reenter_the_clock_lock() {
    let board = Board::connect(":memory:")
        .await
        .expect("memory database")
        .with_middleware(Respond);
    let principal = Principal::default();
    let write = board.request(
        &principal,
        "POST",
        "/messages",
        &[],
        json!({"channel":"work","text":"Work"}),
    );
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), write)
        .await
        .expect("middleware must not deadlock")
        .expect("post and response");
    let original = result["message"]["id"].as_str().expect("original id");
    let events = board
        .request(
            &Principal::default(),
            "GET",
            "/events",
            &[("last".into(), "10".into())],
            Value::Null,
        )
        .await
        .expect("events");
    let posted: Vec<_> = events["events"]
        .as_array()
        .expect("events list")
        .iter()
        .filter(|e| e["type"] == "message.posted")
        .collect();
    assert_eq!(posted.len(), 2, "middleware response bypasses itself");
    assert_ne!(posted[0]["message"]["id"], posted[1]["message"]["id"]);
    assert_eq!(posted[1]["message"]["id"], original);
}
