//! OIDC grants bind an issuer's shared claims to a bounded subject identity.
use engage_core::Principal;
use engage_store::Board;
use serde_json::{Value, json};

async fn grant(board: &Board, claims: Value) -> Result<Value, engage_core::BoardError> {
    board
        .request(
            &Principal::default(),
            "POST",
            "/grants",
            &[],
            json!({
                "principals":[{"kind":"oidc", "issuer":"https://issuer.example", "claims":claims}],
                "can":{"work":["read"]}
            }),
        )
        .await
}

fn caller(issuer: &str, sub: &str, audience: &str) -> Principal {
    Principal {
        id: format!("oidc:issuer.example:{sub}"),
        owner: false,
        facts: json!({"issuer":"oidc", "iss":issuer, "claims":{"sub":sub,"aud":audience}}),
        ..Principal::default()
    }
}

async fn can_read(board: &Board, p: Principal) -> bool {
    let query = vec![("channel".into(), "work".into())];
    let explanation = board
        .request(&p, "GET", "/grants/explain", &query, Value::Null)
        .await
        .expect("explain");
    explanation["can"]
        .as_array()
        .expect("permissions")
        .contains(&json!("read"))
}

#[tokio::test]
async fn added_oidc_grants_require_a_subject_and_reject_broad_or_malformed_patterns() {
    let board = Board::connect(":memory:").await.expect("board");
    for claims in [
        json!({"aud":"shared"}),
        json!({"sub":"*"}),
        json!({"sub":":*"}),
        json!({"sub":"/*"}),
        json!({"sub":"project_path:acme/api*"}),
        json!({"sub":"project_path:acme/*:main"}),
        json!({"sub":["project_path:acme/api:main", "*"]}),
    ] {
        assert_eq!(
            grant(&board, claims)
                .await
                .expect_err("unsafe subject")
                .code,
            "invalid_grant"
        );
    }
    let subject =
        json!({"kind":"oidc", "issuer":"https://issuer.example", "claims":{"sub":"exact"}});
    let good = grant(&board, subject["claims"].clone())
        .await
        .expect("exact subject");
    let path = format!(
        "/grants/{}",
        good["grant"]["id"].as_str().expect("grant ID")
    );
    let result = board.request(&Principal::default(), "PATCH", &path, &[], json!({"add":[{"kind":"oidc", "issuer":"https://issuer.example", "claims":{"aud":"shared"}}]})).await;
    assert_eq!(
        result.expect_err("patch needs subject").code,
        "invalid_grant"
    );
}

#[tokio::test]
async fn subject_prefixes_preserve_project_issuer_and_other_claim_boundaries() {
    let board = Board::connect(":memory:").await.expect("board");
    grant(
        &board,
        json!({"sub":["project_path:acme/api:*", "service/jobs/*"], "aud":"shared"}),
    )
    .await
    .expect("bounded prefixes");
    for sub in [
        "project_path:acme/api:main",
        "project_path:acme/api:release:prod",
        "service/jobs/42",
    ] {
        assert!(
            can_read(&board, caller("https://issuer.example", sub, "shared")).await,
            "subject {sub}"
        );
    }
    for sub in [
        "project_path:acme/api-evil:main",
        "project_path:acme/api",
        "service/jobs-evil/42",
    ] {
        assert!(
            !can_read(&board, caller("https://issuer.example", sub, "shared")).await,
            "subject {sub}"
        );
    }
    assert!(
        !can_read(
            &board,
            caller("https://other.example", "service/jobs/42", "shared")
        )
        .await
    );
    assert!(
        !can_read(
            &board,
            caller("https://issuer.example", "service/jobs/42", "other")
        )
        .await
    );
}

#[tokio::test]
async fn exact_subjects_remain_exact_and_legacy_unscoped_subjects_can_be_removed() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("board.db").display()
    );
    let board = Board::connect(&url).await.expect("board");
    grant(&board, json!({"sub":"service/jobs/42", "aud":"shared"}))
        .await
        .expect("exact subject");
    assert!(
        can_read(
            &board,
            caller("https://issuer.example", "service/jobs/42", "shared")
        )
        .await
    );
    assert!(
        !can_read(
            &board,
            caller("https://issuer.example", "service/jobs/420", "shared")
        )
        .await
    );
    let legacy =
        json!({"kind":"oidc", "issuer":"https://issuer.example", "claims":{"aud":"shared"}});
    let record = json!({"id":"legacy", "principals":[legacy, {"kind":"user", "userId":"retained", "email":null}], "can":{"work":["read"]}});
    let pool = sqlx::any::AnyPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("legacy fixture database");
    sqlx::query("INSERT INTO engage_records(kind,id,channel,parent,position,body) VALUES('grant','legacy','','',0,$1)").bind(record.to_string()).execute(&pool).await.expect("legacy grant fixture");
    let updated = board
        .request(
            &Principal::default(),
            "PATCH",
            "/grants/legacy",
            &[],
            json!({"remove":[legacy]}),
        )
        .await
        .expect("legacy subject removal");
    assert_eq!(
        updated["grant"]["principals"]
            .as_array()
            .expect("principals")
            .len(),
        1
    );
    assert_eq!(updated["grant"]["principals"][0]["kind"], "user");
}
