use engage_core::Principal;
use engage_store::Board;
use serde_json::{Value, json};

async fn write(board: &Board, path: &str, body: Value) -> Value {
    board
        .request(&Principal::default(), "POST", path, &[], body)
        .await
        .expect("protocol write")
}
async fn read(board: &Board, path: &str) -> Value {
    board
        .request(&Principal::default(), "GET", path, &[], Value::Null)
        .await
        .expect("protocol read")
}

#[tokio::test]
async fn revision_and_reply_preconditions_are_atomic() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("memory board");
    let root = write(
        &board,
        "/messages",
        json!({"channel":"work","text":"original"}),
    )
    .await["message"]
        .clone();
    let id = root["id"].as_str().expect("ID");
    let revision = root["revision"].clone();
    write(
        &board,
        &format!("/messages/{id}/reactions"),
        json!({"emoji":"eyes"}),
    )
    .await;
    assert_eq!(
        read(&board, &format!("/messages/{id}")).await["message"]["revision"],
        revision
    );
    let principal = Principal::default();
    let edit_path = format!("/messages/{id}/edit");
    let first = board.request(
        &principal,
        "POST",
        &edit_path,
        &[],
        json!({"text":"first","revision":revision}),
    );
    let second = board.request(
        &principal,
        "POST",
        &edit_path,
        &[],
        json!({"text":"second","revision":revision}),
    );
    let (first, second) = tokio::join!(first, second);
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert_eq!(
        first.err().or(second.err()).expect("losing race").code,
        "revision_conflict"
    );
    write(
        &board,
        "/messages",
        json!({"re":id,"after":id,"text":"reply"}),
    )
    .await;
    let stale = board
        .request(
            &Principal::default(),
            "POST",
            "/messages",
            &[],
            json!({"re":id,"after":id,"text":"late"}),
        )
        .await
        .expect_err("stale precondition");
    assert_eq!(stale.code, "newer_message");
}

#[tokio::test]
async fn claims_fence_stale_workers_and_never_publish_tokens() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("memory board");
    let root =
        write(&board, "/tasks", json!({"channel":"work","text":"task"})).await["task"].clone();
    let id = root["id"].as_str().expect("ID");
    let claim =
        write(&board, &format!("/tasks/{id}/claim"), json!({"lease":60})).await["claim"].clone();
    let token = claim["token"].as_str().expect("token").to_owned();
    assert!(
        !read(&board, &format!("/messages/{id}"))
            .await
            .to_string()
            .contains(&token)
    );
    write(&board, &format!("/tasks/{id}/release"), claim.clone()).await;
    let next =
        write(&board, &format!("/tasks/{id}/claim"), json!({"lease":60})).await["claim"].clone();
    assert!(next["epoch"].as_u64() > claim["epoch"].as_u64());
    let lost = board
        .request(
            &Principal::default(),
            "POST",
            &format!("/tasks/{id}/complete"),
            &[],
            claim,
        )
        .await
        .expect_err("stale worker");
    assert_eq!(lost.code, "claim_lost");
    let mut complete = next;
    complete["result"] = "done".into();
    write(&board, &format!("/tasks/{id}/complete"), complete).await;
    let events = board
        .request(
            &Principal::default(),
            "GET",
            "/events",
            &[("after".into(), "0".into())],
            Value::Null,
        )
        .await
        .expect("events");
    assert!(!events.to_string().contains(&token));
}

#[tokio::test]
async fn refused_batches_leave_no_threads() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("memory board");
    let bad=board.request(&Principal::default(),"POST","/tasks/batch",&[],json!({"channel":"work","tasks":[{"key":"a","text":"first","needs":[{"key":"b"}]},{"key":"b","text":"second","needs":[{"key":"a"}]}]})).await.expect_err("cyclic dependencies");
    assert_eq!(bad.code, "invalid_needs");
    let threads = read(&board, "/threads").await;
    assert!(threads["threads"].as_array().expect("threads").is_empty());
}

#[tokio::test]
async fn separate_connections_preserve_committed_cursor_order() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("board.db").display()
    );
    let first = Board::connect(&url).await.expect("first board");
    let second = Board::connect(&url).await.expect("second board");
    let (a, b) = tokio::join!(
        write(&first, "/messages", json!({"channel":"work","text":"a"})),
        write(&second, "/messages", json!({"channel":"work","text":"b"}))
    );
    assert_ne!(a["message"]["id"], b["message"]["id"]);
    let events = first
        .request(
            &Principal::default(),
            "GET",
            "/events",
            &[("after".into(), "0".into())],
            Value::Null,
        )
        .await
        .expect("events");
    assert_eq!(events["events"].as_array().expect("events").len(), 2);
}
