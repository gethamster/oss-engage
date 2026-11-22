use engage_core::Principal;
use engage_store::Board;
use serde_json::{Value, json};

#[tokio::test]
async fn malformed_query_values_are_not_silently_defaulted_or_capped() {
    let board = Board::connect(":memory:").await.expect("database");
    let owner = Principal::default();
    for (path, params) in [
        ("/messages", vec![("channel", "work"), ("limit", "-1")]),
        (
            "/messages",
            vec![("channel", "work"), ("limit", "1"), ("limit", "2")],
        ),
        ("/events", vec![("all", "sometimes")]),
        ("/events", vec![("wait", "NaN")]),
        ("/events", vec![("last", "1"), ("after", "1")]),
        ("/threads", vec![("before", "1"), ("after", "2")]),
        ("/tasks", vec![("state", "nearly-finished")]),
    ] {
        let query = params
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect::<Vec<_>>();
        let error = board
            .request(&owner, "GET", path, &query, Value::Null)
            .await
            .expect_err("invalid query");
        assert_eq!(error.status, 400, "{path} {query:?}");
    }
}
#[tokio::test]
async fn malformed_optional_fields_are_rejected_at_boundary() {
    let board = Board::connect(":memory:").await.expect("database");
    let owner = Principal::default();
    for body in [
        json!({"channel":"work","text":"valid","noSync":"false"}),
        json!({"channel":"work","text":"valid","attachments":"file"}),
        json!({"channel":"work","text":"\u{0}"}),
    ] {
        let error = board
            .request(&owner, "POST", "/messages", &[], body)
            .await
            .expect_err("invalid body");
        assert_eq!(error.status, 400);
    }
}

#[tokio::test]
async fn claim_wait_is_a_bounded_number_at_the_shared_boundary() {
    let board = Board::connect(":memory:").await.expect("database");
    for wait in [json!(-1), json!(301), json!("1"), Value::Null] {
        let error = board
            .request(
                &Principal::default(),
                "POST",
                "/tasks/claim",
                &[],
                json!({"channel":"work", "wait":wait}),
            )
            .await
            .expect_err("invalid wait");
        assert_eq!(error.status, 400, "wait={wait}");
    }
}
