//! Authorization boundary regressions for grant delegation and atomic batches.
use engage_core::Principal;
use engage_store::Board;
use serde_json::{Value, json};

#[tokio::test]
async fn narrowed_owner_cannot_delegate_through_hidden_descendant() {
    let board = Board::connect(":memory:").await.expect("memory database");
    let owner = Principal {
        scope: Some("*=all;work/secret/=".into()),
        ..Principal::default()
    };
    let result = board
        .request(
            &owner,
            "POST",
            "/grants",
            &[],
            json!({"principals":[{"kind":"project","project":"ci"}],"can":{"work/":["read"]}}),
        )
        .await;
    assert_eq!(
        result
            .expect_err("cannot delegate through scope exception")
            .status,
        403
    );
    let list = board
        .request(&Principal::default(), "GET", "/grants", &[], Value::Null)
        .await
        .expect("owner lists grants");
    assert_eq!(
        list["grants"],
        json!([]),
        "refused delegation must not persist"
    );
}

#[tokio::test]
async fn invalid_last_grant_leaves_batch_atomic() {
    let board = Board::connect(":memory:").await.expect("memory database");
    let owner = Principal::default();
    let result = board
        .request(
            &owner,
            "POST",
            "/grants/batch",
            &[],
            json!({"grants":[
                {"principals":[{"kind":"user","userId":"a"}],"can":{"work/":["read"]}},
                {"principals":[{"kind":"user","userId":"b"}],"can":{"work/*":["read"]}}
            ]}),
        )
        .await;
    assert_eq!(result.expect_err("invalid pattern").code, "invalid_grant");
    let list = board
        .request(&owner, "GET", "/grants", &[], Value::Null)
        .await
        .expect("owner lists grants");
    assert_eq!(list["grants"], json!([]));
}

#[tokio::test]
async fn everyone_does_not_admit_an_outsider() {
    let board = Board::connect(":memory:").await.expect("memory database");
    let owner = Principal::default();
    board
        .request(
            &owner,
            "POST",
            "/grants",
            &[],
            json!({"principals":[{"kind":"everyone"}],"can":{"*":"all"}}),
        )
        .await
        .expect("everyone grant");
    let other = Principal {
        id: "local:user:outsider".into(),
        owner: false,
        ..Principal::default()
    };
    let query = vec![("channel".into(), "work".into())];
    let explanation = board
        .request(&other, "GET", "/grants/explain", &query, Value::Null)
        .await
        .expect("own access explanation");
    assert_eq!(explanation["can"], json!([]));
    assert_eq!(explanation["pattern"], Value::Null);
}
