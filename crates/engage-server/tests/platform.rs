use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use engage_core::Principal;
use engage_server::{BlobConfig, ServerConfig, router};
use engage_store::Board;
use engage_sync::{
    Capabilities, Delivery, Provider, ProviderPost, Registry, ResolvedTarget, SyncError,
    TargetKind, WebhookRequest, WebhookResponse,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Debug)]
struct Probe {
    typing: Arc<Mutex<Vec<TypingCall>>>,
}
type TypingCall = (String, Option<String>);

#[async_trait]
impl Provider for Probe {
    fn platform(&self) -> &'static str {
        "slack"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            typing: true,
            ..Default::default()
        }
    }
    async fn receive(
        &self,
        _key: &str,
        _request: &WebhookRequest,
    ) -> Result<WebhookResponse, SyncError> {
        Err(SyncError::Unsupported("test webhook"))
    }
    async fn resolve_target(&self, _target: &str) -> Result<ResolvedTarget, SyncError> {
        Err(SyncError::Unsupported("test target"))
    }
    async fn post(
        &self,
        _target: &str,
        _kind: TargetKind,
        _body: &ProviderPost,
    ) -> Result<Delivery, SyncError> {
        Err(SyncError::Unsupported("test post"))
    }
    async fn typing(&self, thread: &str, status: Option<&str>) -> Result<(), SyncError> {
        self.typing
            .lock()
            .expect("test typing lock")
            .push((thread.into(), status.map(str::to_owned)));
        Ok(())
    }
    fn locate(&self, url: &str) -> Option<(String, Option<String>)> {
        (url == "https://chat.example.test/C1/1.0").then(|| ("C1:1.0".into(), Some("1.0".into())))
    }
}

#[tokio::test]
async fn typing_reaches_a_configured_provider_without_a_durable_event() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("board opens");
    let owner = Principal::default();
    let post = board
        .request(
            &owner,
            "POST",
            "/messages",
            &[],
            json!({"channel":"work","text":"working"}),
        )
        .await
        .expect("thread creates");
    let id = post["message"]["id"].as_str().expect("message ID");
    board.request(&owner,"POST","/sync/receipts",&[],json!({"kind":"thread","message":id,"target":"slack:C1","targetKind":"channel","cause":"config"})).await.expect("thread target records");
    board.request(&owner,"POST","/sync/receipts",&[],json!({"kind":"message","message":id,"target":"slack:C1","external":{"thread":"slack:C1:1.0","message":"1.0"}})).await.expect("platform copy records");
    let before = board
        .request(&owner, "GET", "/events", &[], Value::Null)
        .await
        .expect("cursor reads")["cursor"]
        .clone();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut registry = Registry::new();
    registry
        .register(
            "slack",
            &owner.id,
            Probe {
                typing: calls.clone(),
            },
        )
        .expect("provider registers");
    let app = router(
        board.clone(),
        ServerConfig {
            sync: Some(registry),
            blobs: BlobConfig::Memory,
            ..Default::default()
        },
    )
    .await
    .expect("router initializes");
    let request = Request::builder()
        .method("POST")
        .uri(format!("/messages/{id}/typing"))
        .header("host", "127.0.0.1:4100")
        .body(Body::from(json!({"status":"working"}).to_string()))
        .expect("typing request builds");
    let response = app.oneshot(request).await.expect("typing dispatches");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response reads")
        .to_bytes();
    let response: Value = serde_json::from_slice(&bytes).expect("response parses");
    assert_eq!(response["targets"], json!(["slack:C1"]));
    assert_eq!(
        *calls.lock().expect("typing capture lock"),
        vec![("C1:1.0".into(), Some("working".into()))]
    );
    let after = board
        .request(&owner, "GET", "/events", &[], Value::Null)
        .await
        .expect("cursor reads");
    assert_eq!(after["cursor"], before, "typing is ephemeral");
}

#[tokio::test]
async fn a_platform_link_reads_the_callers_authorized_imported_thread() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("board opens");
    let owner = Principal::default();
    board
        .request(
            &owner,
            "POST",
            "/watchers",
            &[],
            json!({"watch":{},"sync":[{"key":"slack","platform":"slack"}]}),
        )
        .await
        .expect("sync owner registers before importing");
    let imported=board.request(&owner,"POST","/sync/imports",&[],json!({"op":"post","channel":"work","text":"platform proof","from":"slack","author":{"id":"slack:U1","handle":"alice","name":"Alice","avatar":null,"isBot":false},"origin":{"channel":"slack:C1","thread":"slack:C1:1.0","message":"1.0","subthread":null,"context":null},"id":"link-test"})).await.expect("platform root imports");
    let mut registry = Registry::new();
    registry
        .register(
            "slack",
            &owner.id,
            Probe {
                typing: Arc::default(),
            },
        )
        .expect("provider registers");
    let app = router(
        board,
        ServerConfig {
            sync: Some(registry),
            blobs: BlobConfig::Memory,
            ..Default::default()
        },
    )
    .await
    .expect("router initializes");
    let request = Request::builder()
        .uri("/links?url=https%3A%2F%2Fchat.example.test%2FC1%2F1.0")
        .header("host", "127.0.0.1:4100")
        .body(Body::empty())
        .expect("request builds");
    let response = app.oneshot(request).await.expect("link dispatches");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("thread reads")
        .to_bytes();
    let view: Value = serde_json::from_slice(&bytes).expect("thread parses");
    assert_eq!(view["message"]["id"], imported["message"]);
    assert_eq!(view["message"]["text"], "platform proof");
}
