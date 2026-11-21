use async_trait::async_trait;
use engage_sync::{
    BoardApi, Capabilities, Delivery, Provider, ProviderPost, Registry, ResolvedTarget, SyncConfig,
    SyncError, SyncRoot, SyncWorker, TargetKind, WebhookRequest, WebhookResponse,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

type Calls = Arc<Mutex<Vec<(String, String, Vec<(String, String)>, Value)>>>;
#[derive(Debug)]
struct MockBoard {
    calls: Calls,
    items: Vec<Value>,
    existing: bool,
    copies: Value,
    fail_import: bool,
}
impl MockBoard {
    fn new(items: Vec<Value>) -> Self {
        Self {
            calls: Default::default(),
            items,
            existing: false,
            copies: json!({}),
            fail_import: false,
        }
    }
    fn bodies(&self, path: &str) -> Vec<Value> {
        self.calls
            .lock()
            .expect("test lock")
            .iter()
            .filter(|(_, p, _, _)| p == path)
            .map(|(_, _, _, body)| body.clone())
            .collect()
    }
}
#[async_trait]
impl BoardApi for MockBoard {
    async fn request(
        &self,
        principal: &str,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Value,
    ) -> Result<Value, SyncError> {
        assert_eq!(principal, "service:sync");
        self.calls.lock().expect("test lock").push((
            method.into(),
            path.into(),
            query.to_vec(),
            body,
        ));
        if path == "/sync/imports" && self.fail_import {
            return Err(SyncError::Board {
                status: 403,
                code: "forbidden".into(),
            });
        }
        Ok(match path {
            "/inbox/demo/next" => json!({"items":self.items}),
            "/sync/find" => {
                json!({"thread":if self.existing{Some("root-1")}else{None},"message":if self.existing&&query.iter().any(|(name,_)|name=="message"){Some("message-1")}else{None}})
            }
            "/sync/imports" => json!({"message":"root-1"}),
            "/sync/threads/t1" => json!({"targets":["demo:C1"],"external":{}}),
            "/sync/copies" => json!({"copies":self.copies}),
            _ => json!({"ok":true}),
        })
    }
}
#[derive(Clone, Debug, Default)]
struct MockProvider {
    posts: Arc<Mutex<Vec<(String, TargetKind, String)>>>,
    edits: Arc<Mutex<Vec<String>>>,
}
#[async_trait]
impl Provider for MockProvider {
    fn platform(&self) -> &'static str {
        "chat"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            starts_threads: true,
            edits: true,
            ..Default::default()
        }
    }
    async fn receive(&self, _: &str, _: &WebhookRequest) -> Result<WebhookResponse, SyncError> {
        unreachable!("worker doesn't accept unsigned webhook events")
    }
    async fn resolve_target(&self, target: &str) -> Result<ResolvedTarget, SyncError> {
        Ok(ResolvedTarget {
            target: target.into(),
            channel: target.into(),
            kind: TargetKind::Channel,
        })
    }
    async fn post(
        &self,
        target: &str,
        kind: TargetKind,
        body: &ProviderPost,
    ) -> Result<Delivery, SyncError> {
        self.posts
            .lock()
            .expect("test lock")
            .push((target.into(), kind, body.markdown.clone()));
        Ok(Delivery {
            thread: "C1:1.0".into(),
            message: "1.0".into(),
            files: vec![],
        })
    }
    async fn edit(
        &self,
        _thread: &str,
        _message: &str,
        body: &ProviderPost,
    ) -> Result<(), SyncError> {
        self.edits
            .lock()
            .expect("test lock")
            .push(body.markdown.clone());
        Ok(())
    }
    async fn backfill(
        &self,
        key: &str,
        _target: &str,
        _limit: usize,
        _after: Option<&str>,
    ) -> Result<engage_sync::Backfill, SyncError> {
        let event = inbox_message(true)["event"].clone();
        Ok(engage_sync::Backfill {
            incoming: vec![engage_sync::Incoming {
                id: "history-1".into(),
                thread: format!("{key}:C1:1.0"),
                event,
                raw: json!({}),
            }],
            more: true,
            next: Some("page-2".into()),
        })
    }
    async fn fetch_root(&self, _: &str) -> Result<Option<SyncRoot>, SyncError> {
        Ok(Some(SyncRoot {
            text: "Earlier root".into(),
            author: json!({"id":"chat:user:person","handle":"person","name":"Person","avatar":null,"isBot":false}),
            labels: BTreeMap::new(),
        }))
    }
}
fn registry(provider: MockProvider) -> Registry {
    let mut registry = Registry::new();
    registry
        .register("demo", "service:sync", provider)
        .expect("valid config");
    registry
        .map_channel("demo", "C1", "work")
        .expect("valid route");
    registry
}
fn inbox_message(is_root: bool) -> Value {
    json!({"id":"delivery-1","lease":"lease-1","event":{"kind":"message","channel":"demo:C1","thread":"demo:C1:1.0","subthread":null,"message":"1.1","text":"Hello @[chat:user:person]","isRoot":is_root,"context":null,"author":{"id":"chat:user:person","handle":"person","name":"Person","avatar":null,"isBot":false}},"raw":{}})
}

#[tokio::test]
async fn inbox_imports_as_keyed_platform_identity_and_acks_after_post() {
    let worker = SyncWorker::new(registry(MockProvider::default()));
    let board = MockBoard::new(vec![inbox_message(true)]);
    assert_eq!(
        worker
            .inbox(&board, "demo", 50)
            .await
            .expect("inbox imported"),
        1
    );
    let bodies = board.bodies("/sync/imports");
    assert_eq!(bodies.len(), 1);
    let body = &bodies[0];
    assert_eq!(body["author"]["id"], "demo:user:person");
    assert_eq!(body["text"], "Hello @[demo:user:person]");
    assert_eq!(body["channel"], "work");
    assert_eq!(body["source"]["key"], "demo");
    assert!(body["id"].as_str().expect("derived ID").len() < 200);
    assert_eq!(
        board.bodies("/inbox/demo/delivery-1/ack"),
        vec![json!({"lease":"lease-1"})]
    );
    let calls = board.calls.lock().expect("test lock");
    assert!(
        calls
            .iter()
            .position(|(_, path, _, _)| path == "/sync/imports")
            .expect("post")
            < calls
                .iter()
                .position(|(_, path, _, _)| path.ends_with("/ack"))
                .expect("ack")
    );
}
#[tokio::test]
async fn replay_after_import_does_not_post_again() {
    let worker = SyncWorker::new(registry(MockProvider::default()));
    let mut board = MockBoard::new(vec![inbox_message(true)]);
    board.existing = true;
    worker
        .inbox(&board, "demo", 1)
        .await
        .expect("replay acknowledged");
    assert!(board.bodies("/sync/imports").is_empty());
    assert_eq!(board.bodies("/inbox/demo/delivery-1/ack").len(), 1);
}
#[tokio::test]
async fn first_reply_backfills_root_and_keeps_reply_in_that_thread() {
    let worker = SyncWorker::new(registry(MockProvider::default()));
    let board = MockBoard::new(vec![inbox_message(false)]);
    worker
        .inbox(&board, "demo", 1)
        .await
        .expect("reply imported");
    let bodies = board.bodies("/sync/imports");
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0]["text"], "Earlier root");
    assert_eq!(bodies[1]["re"], "root-1");
    assert_ne!(bodies[0]["id"], bodies[1]["id"]);
}
#[tokio::test]
async fn failed_import_is_retried_with_a_symbolic_error() {
    let worker = SyncWorker::new(registry(MockProvider::default()));
    let mut board = MockBoard::new(vec![inbox_message(true)]);
    board.fail_import = true;
    assert!(worker.inbox(&board, "demo", 1).await.is_err());
    assert_eq!(
        board.bodies("/inbox/demo/delivery-1/ack"),
        vec![json!({"lease":"lease-1","error":"forbidden"})]
    );
}
#[tokio::test]
async fn outbound_delivery_records_receipts_and_suppresses_origin_loop() {
    let provider = MockProvider::default();
    let posts = provider.posts.clone();
    let worker = SyncWorker::new(registry(provider));
    let board = MockBoard::new(vec![]);
    let mut event = json!({"type":"message.posted","cursor":"1","message":{"id":"m1","thread":"t1","channel":"work","re":null,"text":"Hello","from":null,"noSync":false,"attachments":[],"author":{"name":"Ada"}}});
    worker
        .deliver(&board, "service:sync", &event)
        .await
        .expect("outbound delivered");
    assert_eq!(posts.lock().expect("test lock").len(), 1);
    let receipts = board.bodies("/sync/receipts");
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[1]["external"]["thread"], "demo:C1:1.0");
    event["message"]["from"] = json!("demo");
    worker
        .deliver(&board, "service:sync", &event)
        .await
        .expect("own import suppressed");
    assert_eq!(posts.lock().expect("test lock").len(), 1);
}
#[tokio::test]
async fn copied_post_is_not_sent_twice() {
    let provider = MockProvider::default();
    let posts = provider.posts.clone();
    let worker = SyncWorker::new(registry(provider));
    let mut board = MockBoard::new(vec![]);
    board.copies = json!({"m1":{"demo:C1":{"thread":"demo:C1:1.0","message":"1.0"}}});
    let event = json!({"type":"message.posted","cursor":"1","message":{"id":"m1","thread":"t1","channel":"work","re":null,"text":"Hello","from":null,"attachments":[]}});
    worker
        .deliver(&board, "service:sync", &event)
        .await
        .expect("receipt recognized");
    assert!(posts.lock().expect("test lock").is_empty());
}
#[test]
fn config_references_environment_and_rejects_inline_credentials() {
    let config:SyncConfig=serde_json::from_value(json!({"providers":[{"key":"slack","platform":"slack","principal":"service:sync","token_env":"SLACK_TOKEN","signing_secret_env":"SLACK_SECRET","channels":{"C1":"work"}}]})).expect("valid config");
    let registry = Registry::from_config(&config).expect("construct without accessing secrets");
    assert_eq!(registry.principal("slack"), Some("service:sync"));
    assert!(serde_json::from_value::<SyncConfig>(json!({"providers":[{"key":"slack","platform":"slack","principal":"service:sync","token_env":"TOKEN","signing_secret_env":"SECRET","token":"do-not-embed"}]})).is_err());
}

#[tokio::test]
async fn native_backfill_uses_durable_inbox_and_preserves_provider_paging() {
    let worker = SyncWorker::new(registry(MockProvider::default()));
    let board = MockBoard::new(vec![]);
    let progress = worker
        .backfill(&board, "demo", "C1", 50, None)
        .await
        .expect("history queued");
    assert_eq!(progress.queued, 1);
    assert!(progress.more);
    assert_eq!(progress.next.as_deref(), Some("page-2"));
    let bodies = board.bodies("/inbox/demo");
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["events"][0]["id"], "history-1");
    assert_eq!(bodies[0]["events"][0]["event"]["kind"], "message");
}

#[tokio::test]
async fn a_human_edit_to_an_imported_message_returns_to_its_platform() {
    let provider = MockProvider::default();
    let edits = provider.edits.clone();
    let worker = SyncWorker::new(registry(provider));
    let mut board = MockBoard::new(vec![]);
    board.copies = json!({"m1":{"demo:C1":{"thread":"demo:C1:1.0","message":"1.0"}}});
    let event = json!({"type":"message.edited","cursor":"2","payload":{"from":null},"message":{"id":"m1","thread":"t1","channel":"work","re":null,"text":"Human correction","from":"demo","origin":{"thread":"demo:C1"},"attachments":[]}});
    worker
        .deliver(&board, "service:sync", &event)
        .await
        .expect("human edit mirrors back");
    assert_eq!(edits.lock().expect("test lock").len(), 1);
}
