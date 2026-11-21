//! Regressions for untrusted protocol input and authorization during delayed delivery.
use engage_core::Principal;
use engage_store::Board;
use serde_json::{Value, json};

#[tokio::test]
async fn revoked_watcher_cannot_pull_queued_private_content() {
    let board = Board::connect(":memory:").await.expect("board");
    let owner = Principal::default();
    let reader = Principal {
        id: "local:user:reader".into(),
        handle: "reader".into(),
        owner: false,
        ..Principal::default()
    };
    let grant = board
        .request(
            &owner,
            "POST",
            "/grants",
            &[],
            json!({"principals":[{"kind":"user","userId":"reader"}],"can":{"private":["read"]}}),
        )
        .await
        .expect("grant")["grant"]
        .clone();
    board
        .request(
            &reader,
            "POST",
            "/watchers",
            &[],
            json!({"watch":{"reader":{"tail":"private"}}}),
        )
        .await
        .expect("register");
    board
        .request(
            &owner,
            "POST",
            "/messages",
            &[],
            json!({"channel":"private","text":"private content"}),
        )
        .await
        .expect("post");
    board
        .request(
            &owner,
            "POST",
            &format!("/grants/{}/revoke", grant["id"].as_str().expect("grantID")),
            &[],
            json!({}),
        )
        .await
        .expect("revoke");
    let queue = board
        .request(&reader, "GET", "/watchers/matches", &[], Value::Null)
        .await
        .expect("pull");
    assert_eq!(queue["matches"], json!([]));
}

#[tokio::test]
async fn claimant_cannot_forge_feed_identity_or_message_id() {
    let board = Board::connect(":memory:").await.expect("board");
    let owner = Principal::default();
    let task = board
        .request(
            &owner,
            "POST",
            "/tasks",
            &[],
            json!({"channel":"work","text":"task"}),
        )
        .await
        .expect("task")["task"]
        .clone();
    let id = task["id"].as_str().expect("ID");
    let mut proof = board
        .request(
            &owner,
            "POST",
            &format!("/tasks/{id}/claim"),
            &[],
            json!({}),
        )
        .await
        .expect("claim")["claim"]
        .clone();
    proof["id"] = "forged".into();
    proof["by"] = json!({"actor":"attacker"});
    proof["from"] = "forged".into();
    board
        .request(&owner, "POST", &format!("/tasks/{id}/complete"), &[], proof)
        .await
        .expect("complete");
    let feed = board
        .request(
            &owner,
            "GET",
            "/events",
            &[("after".into(), "0".into())],
            Value::Null,
        )
        .await
        .expect("feed");
    let event = feed["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|e| e["type"] == "task.completed")
        .expect("completed");
    assert_eq!(event["payload"]["id"], id);
    assert_eq!(event["payload"]["by"]["actor"], owner.actor());
    assert!(event["payload"].get("from").is_none());
    assert!(event["payload"].get("token").is_none());
}

#[tokio::test]
async fn malformed_unicode_and_submillisecond_durations_are_refused() {
    let board = Board::connect(":memory:").await.expect("board");
    for duration in ["é", "0s", "0.0001s", "1e3s", "NaNs", "Infinitys"] {
        let error = board
            .request(
                &Principal::default(),
                "POST",
                "/schedules",
                &[],
                json!({"channel":"work","text":"later","every":duration}),
            )
            .await
            .expect_err("invalid duration");
        assert_eq!(error.status, 400, "duration {duration}");
    }
    board.tick().await.expect("scheduler remains usable");
}

#[tokio::test]
async fn invalid_scheduled_messages_cannot_block_a_due_valid_send() {
    let board = Board::connect(":memory:").await.expect("board");
    let owner = Principal::default();
    for text in [
        "bad\0text".to_string(),
        "x".repeat(65537),
        "```json#missing@1\n{}\n```".to_string(),
    ] {
        let error = board
            .request(
                &owner,
                "POST",
                "/schedules",
                &[],
                json!({"channel":"work","text":text,"in":"1s"}),
            )
            .await
            .expect_err("invalid scheduled text");
        assert_eq!(error.status, 400);
    }
    let at = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    board
        .request(
            &owner,
            "POST",
            "/schedules",
            &[],
            json!({"channel":"work","text":"valid due message","at":at}),
        )
        .await
        .expect("valid schedule");
    board.tick().await.expect("due work drains");
    let messages = board
        .request(
            &owner,
            "GET",
            "/messages",
            &[("channel".into(), "work".into())],
            Value::Null,
        )
        .await
        .expect("messages");
    assert_eq!(messages["messages"].as_array().expect("messages").len(), 1);
    assert_eq!(messages["messages"][0]["text"], "valid due message");
    assert!(messages["messages"][0]["schedule"]["id"].is_string());
}

#[tokio::test]
async fn sync_copies_are_scoped_to_the_service_that_delivered_them() {
    let board = Board::connect(":memory:").await.expect("board");
    let owner = Principal::default();
    let service = Principal {
        id: "service:sink".into(),
        handle: "sink".into(),
        ..Principal::default()
    };
    board
        .request(
            &service,
            "POST",
            "/watchers",
            &[],
            json!({"watch":{},"sync":[{"key":"sink","platform":"sink"}]}),
        )
        .await
        .expect("register");
    let root = board
        .request(
            &owner,
            "POST",
            "/messages",
            &[],
            json!({"channel":"work","text":"root"}),
        )
        .await
        .expect("post");
    let id = root["message"]["id"].as_str().expect("ID");
    board.request(&service,"POST","/sync/receipts",&[],json!({"kind":"thread","message":id,"target":"sink:C1","targetKind":"channel","cause":"config"})).await.expect("attach");
    board.request(&service,"POST","/sync/receipts",&[],json!({"kind":"message","message":id,"target":"sink:C1","external":{"thread":"sink:C1:123","message":"123"}})).await.expect("delivery");
    let query = [("message".into(), id.to_string())];
    let own = board
        .request(&service, "GET", "/sync/copies", &query, Value::Null)
        .await
        .expect("own receipt");
    assert_eq!(
        own["copies"][id]["sink:C1"]["message"], "123",
        "response {own}"
    );
    let other = board
        .request(&owner, "GET", "/sync/copies", &query, Value::Null)
        .await
        .expect("other service copies");
    assert_eq!(other["copies"][id], json!({}));
    let find = board
        .request(
            &service,
            "GET",
            "/sync/find",
            &[
                ("thread".into(), "sink:C1:123".into()),
                ("message".into(), "123".into()),
            ],
            Value::Null,
        )
        .await
        .expect("external lookup");
    assert_eq!(find["message"], id);
}
