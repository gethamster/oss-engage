use engage_core::Principal;
use engage_store::Board;
use serde_json::{Value, json};

async fn request(
    board: &Board,
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    body: Value,
) -> Value {
    let query = query
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect::<Vec<_>>();
    board
        .request(&Principal::default(), method, path, &query, body)
        .await
        .expect("protocol operation")
}

async fn post(board: &Board, body: Value) -> Value {
    request(board, "POST", "/messages", &[], body).await["message"].clone()
}

fn ids(page: &Value, field: &str) -> Vec<Value> {
    page[field]
        .as_array()
        .expect("listing")
        .iter()
        .map(|s| s["message"]["id"].clone())
        .collect()
}

#[tokio::test]
async fn forward_thread_pages_keep_the_first_activity_after_the_cursor() {
    let board = Board::connect(":memory:").await.expect("board");
    let mut roots = Vec::new();
    for text in ["first", "second", "third", "fourth"] {
        roots.push(post(&board, json!({"channel":"work", "text":text})).await);
    }
    let page = request(
        &board,
        "GET",
        "/threads",
        &[
            ("channel", "work"),
            ("after", roots[0]["id"].as_str().expect("ID")),
            ("limit", "2"),
        ],
        Value::Null,
    )
    .await;
    assert_eq!(
        ids(&page, "threads"),
        vec![roots[1]["id"].clone(), roots[2]["id"].clone()]
    );
}

#[tokio::test]
async fn thread_activity_ignores_edits_reactions_and_deleted_replies() {
    let board = Board::connect(":memory:").await.expect("board");
    let first = post(&board, json!({"channel":"work", "text":"first"})).await;
    let second = post(&board, json!({"channel":"work", "text":"second"})).await;
    let reply = post(&board, json!({"re":first["id"], "text":"reply"})).await;
    request(
        &board,
        "POST",
        &format!("/messages/{}/delete", reply["id"].as_str().expect("ID")),
        &[],
        json!({}),
    )
    .await;
    request(
        &board,
        "POST",
        &format!("/messages/{}/edit", first["id"].as_str().expect("ID")),
        &[],
        json!({"text":"edited"}),
    )
    .await;
    let page = request(
        &board,
        "GET",
        "/threads",
        &[("channel", "work"), ("limit", "1")],
        Value::Null,
    )
    .await;
    assert_eq!(ids(&page, "threads"), vec![second["id"].clone()]);
    let older = page["older"].as_str().expect("older page");
    let previous = request(
        &board,
        "GET",
        "/threads",
        &[("channel", "work"), ("before", older), ("limit", "1")],
        Value::Null,
    )
    .await;
    assert_eq!(ids(&previous, "threads"), vec![first["id"].clone()]);
    assert_eq!(previous["threads"][0]["replies"], 0);
    assert!(previous["threads"][0]["last"].is_null());
}

#[tokio::test]
async fn text_filter_matches_a_phrase_instead_of_unordered_words() {
    let board = Board::connect(":memory:").await.expect("board");
    let hit = post(&board, json!({"channel":"work", "text":"A Red Fox runs"})).await;
    post(
        &board,
        json!({"channel":"work", "text":"fox comes before red"}),
    )
    .await;
    let page = request(
        &board,
        "GET",
        "/threads",
        &[("channel", "work"), ("text", "red fox")],
        Value::Null,
    )
    .await;
    assert_eq!(ids(&page, "threads"), vec![hit["id"].clone()]);
    let events = request(
        &board,
        "GET",
        "/events",
        &[("after", "0"), ("text", "red fox")],
        Value::Null,
    )
    .await;
    assert_eq!(events["events"].as_array().expect("events").len(), 1);
}

#[tokio::test]
async fn latest_tasks_are_ordered_by_messages_rather_than_claim_changes() {
    let board = Board::connect(":memory:").await.expect("board");
    let first = request(
        &board,
        "POST",
        "/tasks",
        &[],
        json!({"channel":"work", "text":"first"}),
    )
    .await["task"]
        .clone();
    let second = request(
        &board,
        "POST",
        "/tasks",
        &[],
        json!({"channel":"work", "text":"second"}),
    )
    .await["task"]
        .clone();
    request(
        &board,
        "POST",
        &format!("/tasks/{}/claim", first["id"].as_str().expect("ID")),
        &[],
        json!({"lease":60}),
    )
    .await;
    let page = request(
        &board,
        "GET",
        "/tasks",
        &[("channel", "work"), ("latest", "true"), ("limit", "1")],
        Value::Null,
    )
    .await;
    assert_eq!(ids(&page, "tasks"), vec![second["id"].clone()]);
}

#[tokio::test]
async fn queue_claim_prefers_assigned_work_to_an_older_unassigned_task() {
    let board = Board::connect(":memory:").await.expect("board");
    request(
        &board,
        "POST",
        "/tasks",
        &[],
        json!({"channel":"work", "text":"unassigned"}),
    )
    .await;
    let assigned = request(
        &board,
        "POST",
        "/tasks",
        &[],
        json!({"channel":"work", "text":"assigned", "for":Principal::default().actor()}),
    )
    .await["task"]
        .clone();
    let claim = request(
        &board,
        "POST",
        "/tasks/claim",
        &[],
        json!({"channel":"work"}),
    )
    .await;
    assert_eq!(claim["task"]["id"], assigned["id"]);
}

#[tokio::test]
async fn false_all_flag_does_not_include_canceled_schedules() {
    let board = Board::connect(":memory:").await.expect("board");
    let schedule = request(
        &board,
        "POST",
        "/schedules",
        &[],
        json!({"channel":"work", "text":"later", "in":"1h"}),
    )
    .await["schedule"]
        .clone();
    request(
        &board,
        "POST",
        &format!("/schedules/{}/cancel", schedule["id"].as_str().expect("ID")),
        &[],
        json!({}),
    )
    .await;
    for flag in ["false", "0"] {
        let page = request(&board, "GET", "/schedules", &[("all", flag)], Value::Null).await;
        assert!(
            page["schedules"].as_array().expect("schedules").is_empty(),
            "all={flag}"
        );
    }
    for flag in ["true", "1"] {
        let page = request(&board, "GET", "/schedules", &[("all", flag)], Value::Null).await;
        assert_eq!(
            page["schedules"].as_array().expect("schedules").len(),
            1,
            "all={flag}"
        );
    }
}
