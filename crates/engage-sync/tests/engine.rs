//! Real-engine sync journey: verified intake, durable inbox leases, import and outbound receipts.
use async_trait::async_trait;
use engage_core::Principal;
use engage_store::Board;
use engage_sync::{
    BoardApi, Capabilities, Credentials, Delivery, Provider, ProviderPost, Registry,
    ResolvedTarget, Secret, Slack, StaticCredentials, SyncError, SyncWorker, TargetKind,
    WebhookRequest, WebhookResponse,
};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

struct Local(Board);
#[async_trait]
impl BoardApi for Local {
    async fn request(
        &self,
        principal: &str,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Value,
    ) -> Result<Value, SyncError> {
        self.0
            .request(
                &Principal {
                    id: principal.into(),
                    handle: principal.into(),
                    name: principal.into(),
                    ..Default::default()
                },
                method,
                path,
                query,
                body,
            )
            .await
            .map_err(|error| SyncError::Board {
                status: error.status,
                code: error.code,
            })
    }
}
#[derive(Clone, Debug, Default)]
struct Sink(Arc<Mutex<Vec<String>>>);
#[async_trait]
impl Provider for Sink {
    fn platform(&self) -> &'static str {
        "sink"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            starts_threads: true,
            edits: true,
            reactions: true,
            ..Default::default()
        }
    }
    async fn receive(&self, _: &str, _: &WebhookRequest) -> Result<WebhookResponse, SyncError> {
        unreachable!("sink has no webhook")
    }
    async fn resolve_target(&self, target: &str) -> Result<ResolvedTarget, SyncError> {
        Ok(ResolvedTarget {
            target: target.into(),
            channel: target.into(),
            kind: TargetKind::Channel,
        })
    }
    fn root_message_id(&self, thread: &str) -> Option<String> {
        thread.split_once(':').map(|(_, message)| message.into())
    }
    async fn fetch_root(&self, _: &str) -> Result<Option<engage_sync::SyncRoot>, SyncError> {
        Ok(Some(engage_sync::SyncRoot {
            text: "Original root".into(),
            author: json!({"id":"sink:user:person","handle":"person","name":"Person","avatar":null,"isBot":false}),
            labels: BTreeMap::new(),
        }))
    }
    async fn react(
        &self,
        _thread: &str,
        _message: &str,
        emoji: &str,
        added: bool,
    ) -> Result<(), SyncError> {
        self.0
            .lock()
            .expect("sink lock")
            .push(format!("react:{emoji}:{added}"));
        Ok(())
    }
    async fn edit(
        &self,
        _thread: &str,
        _message: &str,
        body: &ProviderPost,
    ) -> Result<(), SyncError> {
        self.0
            .lock()
            .expect("sink lock")
            .push(format!("edit:{}", body.markdown));
        Ok(())
    }
    async fn backfill(
        &self,
        key: &str,
        _target: &str,
        _limit: usize,
        _after: Option<&str>,
    ) -> Result<engage_sync::Backfill, SyncError> {
        let thread = format!("{key}:C1:history-root");
        Ok(engage_sync::Backfill {
            incoming: vec![engage_sync::Incoming {
                id: "historical-event".into(),
                thread: thread.clone(),
                event: json!({"kind":"message","channel":format!("{key}:C1"),"thread":thread,"subthread":null,"message":"history-root","text":"Historical proof","isRoot":true,"context":null,"author":{"id":"sink:user:person","handle":"person","name":"Person","avatar":null,"isBot":false}}),
                raw: json!({}),
            }],
            more: false,
            next: None,
        })
    }
    async fn post(
        &self,
        _: &str,
        _: TargetKind,
        body: &ProviderPost,
    ) -> Result<Delivery, SyncError> {
        self.0
            .lock()
            .expect("sink lock")
            .push(body.markdown.clone());
        Ok(Delivery {
            thread: "C1:123".into(),
            message: "123".into(),
            files: vec![],
        })
    }
}
#[tokio::test]
async fn signed_slack_root_is_imported_durably_and_redelivery_stays_single() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("memory database");
    let api = Local(board.clone());
    let slack = Slack::new(StaticCredentials(Credentials {
        access_token: Secret::new("fixture-token"),
        webhook_secret: Secret::new("fixture-secret"),
        bot_user_id: None,
    }));
    let mut registry = Registry::new();
    registry
        .register("slack", "service:slack", slack)
        .expect("provider");
    registry
        .map_channel("slack", "C1", "work")
        .expect("mapping");
    let worker = SyncWorker::new(registry.clone());
    worker.register(&api).await.expect("register watcher");
    let body=serde_json::to_vec(&json!({"type":"event_callback","event_id":"E1","event":{"type":"message","channel":"C1","user":"U1","ts":"1.0","text":"From Slack"}})).expect("fixture JSON");
    let mut mac = Hmac::<Sha256>::new_from_slice(b"fixture-secret").expect("fixture key");
    mac.update(b"v0:1800000000:");
    mac.update(&body);
    let request = WebhookRequest {
        headers: BTreeMap::from([
            ("x-slack-request-timestamp".into(), "1800000000".into()),
            (
                "x-slack-signature".into(),
                format!("v0={}", hex::encode(mac.finalize().into_bytes())),
            ),
        ]),
        body,
        now_unix: 1_800_000_000,
    };
    let verified = registry
        .webhook("slack", &request)
        .await
        .expect("signature verifies");
    for _ in 0..2 {
        let body = json!({"events":verified.incoming.iter().map(|event|json!({"id":event.id,"event":event.event,"raw":event.raw})).collect::<Vec<_>>()});
        api.request("service:slack", "POST", "/inbox/slack", &[], body)
            .await
            .expect("durable intake");
        worker
            .run_once(&api, 10)
            .await
            .expect("inbox import and own-source suppression");
    }
    let page = board
        .request(
            &Principal::default(),
            "GET",
            "/threads",
            &[("channel".into(), "work".into())],
            Value::Null,
        )
        .await
        .expect("threads");
    let threads = page["threads"].as_array().expect("thread array");
    assert_eq!(
        threads.len(),
        1,
        "platform redelivery must not fork the thread"
    );
    assert_eq!(threads[0]["message"]["from"], "slack");
    assert_eq!(threads[0]["message"]["origin"]["thread"], "slack:C1:1.0");
    assert_eq!(threads[0]["message"]["text"], "From Slack");
}
#[tokio::test]
async fn board_root_reaches_configured_provider_and_receipt_prevents_repeat() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("memory database");
    let api = Local(board.clone());
    let sink = Sink::default();
    let calls = sink.0.clone();
    let mut registry = Registry::new();
    registry
        .register("sink", "service:sink", sink)
        .expect("provider");
    registry.map_channel("sink", "C1", "work").expect("mapping");
    let worker = SyncWorker::new(registry);
    worker.register(&api).await.expect("register watcher");
    let posted = board
        .request(
            &Principal::default(),
            "POST",
            "/messages",
            &[],
            json!({"channel":"work","text":"Build finished"}),
        )
        .await
        .expect("post");
    worker.run_once(&api, 10).await.expect("deliver");
    worker.run_once(&api, 10).await.expect("retry no-op");
    assert_eq!(
        calls.lock().expect("sink lock").len(),
        1,
        "one physical post for a completed watcher delivery"
    );
    let id = posted["message"]["id"]
        .as_str()
        .or(posted["id"].as_str())
        .expect("posted message ID");
    let copies = api
        .request(
            "service:sink",
            "GET",
            "/sync/copies",
            &[("message".into(), id.into())],
            Value::Null,
        )
        .await
        .expect("read receipt");
    assert_eq!(copies["copies"][id]["sink:C1"]["message"], "123");
}

#[tokio::test]
async fn historical_backfill_runs_through_real_inbox_and_can_be_repeated() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("memory database");
    let api = Local(board.clone());
    let mut registry = Registry::new();
    registry
        .register("sink", "service:sink", Sink::default())
        .expect("provider");
    registry.map_channel("sink", "C1", "work").expect("mapping");
    let worker = SyncWorker::new(registry);
    worker.register(&api).await.expect("register");
    for _ in 0..2 {
        let progress = worker
            .backfill(&api, "sink", "C1", 50, None)
            .await
            .expect("history queue");
        assert_eq!(progress.queued, 1);
        worker.run_once(&api, 10).await.expect("history imported");
    }
    let page = board
        .request(
            &Principal::default(),
            "GET",
            "/threads",
            &[("channel".into(), "work".into())],
            Value::Null,
        )
        .await
        .expect("thread list");
    let threads = page["threads"].as_array().expect("array");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0]["message"]["text"], "Historical proof");
}

#[tokio::test]
async fn a_board_reaction_to_imported_history_mirrors_back_to_its_source_copy() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("memory database");
    let api = Local(board.clone());
    let sink = Sink::default();
    let edits = sink.0.clone();
    let mut registry = Registry::new();
    registry
        .register("sink", "service:sink", sink)
        .expect("provider");
    registry.map_channel("sink", "C1", "work").expect("mapping");
    let worker = SyncWorker::new(registry);
    worker.register(&api).await.expect("register");
    worker
        .backfill(&api, "sink", "C1", 50, None)
        .await
        .expect("queue history");
    worker.run_once(&api, 10).await.expect("import history");
    let threads = board
        .request(
            &Principal::default(),
            "GET",
            "/threads",
            &[("channel".into(), "work".into())],
            Value::Null,
        )
        .await
        .expect("threads");
    let id = threads["threads"][0]["message"]["id"]
        .as_str()
        .expect("root ID");
    board
        .request(
            &Principal::default(),
            "POST",
            &format!("/messages/{id}/reactions"),
            &[],
            json!({"emoji":"heart"}),
        )
        .await
        .expect("react to imported root");
    worker.run_once(&api, 10).await.expect("mirror reaction");
    let edits = edits.lock().expect("sink lock");
    assert_eq!(
        edits.len(),
        1,
        "board-origin reaction must reach its imported source copy"
    );
    assert_eq!(edits[0], "react:heart:true");
}

#[tokio::test]
async fn reply_first_import_keeps_canonical_root_identity_for_replay_and_edits() {
    let board = Board::connect("sqlite::memory:").await.expect("database");
    let api = Local(board.clone());
    let mut registry = Registry::new();
    registry
        .register("sink", "service:sink", Sink::default())
        .expect("provider");
    registry.map_channel("sink", "C1", "work").expect("mapping");
    let worker = SyncWorker::new(registry);
    worker.register(&api).await.expect("register");
    let author = json!({"id":"sink:user:person","handle":"person","name":"Person","avatar":null,"isBot":false});
    for (id, message, is_root, text) in [
        ("reply", "1.1", false, "First observed reply"),
        ("late-root", "1.0", true, "Original root"),
    ] {
        api.request("service:sink", "POST", "/inbox/sink", &[], json!({"events":[{"id":id,"event":{"kind":"message","channel":"sink:C1","thread":"sink:C1:1.0","message":message,"isRoot":is_root,"text":text,"author":author}}]})).await.expect("enqueue");
        worker.run_once(&api, 10).await.expect("import");
    }
    let threads = board
        .request(
            &Principal::default(),
            "GET",
            "/threads",
            &[("channel".into(), "work".into())],
            Value::Null,
        )
        .await
        .expect("threads");
    let root = threads["threads"][0]["message"]["id"]
        .as_str()
        .expect("root ID");
    let view = board
        .request(
            &Principal::default(),
            "GET",
            &format!("/messages/{root}"),
            &[],
            Value::Null,
        )
        .await
        .expect("thread");
    assert_eq!(
        view["replies"].as_array().expect("replies").len(),
        1,
        "later root delivery must not become a duplicate reply"
    );
    assert_eq!(
        view["message"]["origin"]["message"], "1.0",
        "synthetic root keeps platform identity"
    );
    api.request("service:sink", "POST", "/inbox/sink", &[], json!({"events":[{"id":"edit-root","event":{"kind":"edited","channel":"sink:C1","thread":"sink:C1:1.0","message":"1.0","text":"Root correction","author":author}}]})).await.expect("queue root edit");
    worker.run_once(&api, 10).await.expect("root edit");
    let view = board
        .request(
            &Principal::default(),
            "GET",
            &format!("/messages/{root}"),
            &[],
            Value::Null,
        )
        .await
        .expect("thread after edit");
    assert_eq!(view["message"]["text"], "Root correction");
}
