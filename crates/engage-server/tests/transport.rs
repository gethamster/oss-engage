use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use engage_core::Principal;
use engage_server::{AuthConfig, BlobConfig, ServerConfig, router};
use engage_store::Board;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn local() -> Router {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("in-memory board opens");
    router(
        board,
        ServerConfig {
            blobs: BlobConfig::Memory,
            ..Default::default()
        },
    )
    .await
    .expect("local router initializes")
}

async fn json_request(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:4100")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("test request builds");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("request dispatches");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body reads")
        .to_bytes();
    let value = serde_json::from_slice(&bytes).expect("JSON response parses");
    (status, value)
}

#[tokio::test]
async fn local_transport_refuses_dns_rebinding_and_cross_origin_writes() {
    let app = local().await;
    let request = Request::builder()
        .uri("/me")
        .header("host", "attacker.example")
        .body(Body::empty())
        .expect("request builds");
    assert_eq!(
        app.clone()
            .oneshot(request)
            .await
            .expect("request dispatches")
            .status(),
        StatusCode::FORBIDDEN
    );
    let request = Request::builder()
        .method("POST")
        .uri("/messages")
        .header("host", "127.0.0.1:4100")
        .header("origin", "https://attacker.example")
        .body(Body::from(
            json!({"channel":"work","text":"forged"}).to_string(),
        ))
        .expect("request builds");
    assert_eq!(
        app.oneshot(request)
            .await
            .expect("request dispatches")
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn local_authentication_never_binds_a_public_interface() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("board opens");
    let result = router(
        board,
        ServerConfig {
            bind: "0.0.0.0:4100".parse().expect("address parses"),
            ..Default::default()
        },
    )
    .await;
    assert!(
        result.is_err(),
        "public listener must require configured credentials"
    );
}

#[tokio::test]
async fn bearer_credentials_require_verification_and_issue_an_httponly_session() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("board opens");
    let token = uuid::Uuid::new_v4().to_string();
    let app = router(
        board,
        ServerConfig {
            auth: AuthConfig::Bearer {
                token: token.clone(),
                principal: Principal::default(),
            },
            session_key: Some(uuid::Uuid::new_v4().to_string()),
            blobs: BlobConfig::Memory,
            ..Default::default()
        },
    )
    .await
    .expect("router initializes");
    let (status, _) = json_request(&app, "GET", "/me", Value::Null).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let request = Request::builder()
        .method("POST")
        .uri("/auth/session")
        .header("host", "127.0.0.1:4100")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request builds");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("request dispatches");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cookie = response
        .headers()
        .get("set-cookie")
        .expect("session cookie exists")
        .to_str()
        .expect("cookie is text");
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    assert!(
        !cookie.contains(&token),
        "source bearer must not be stored in browser cookie"
    );
    let request = Request::builder()
        .uri("/me")
        .header("host", "127.0.0.1:4100")
        .header(
            "cookie",
            cookie.split(';').next().expect("cookie name exists"),
        )
        .body(Body::empty())
        .expect("request builds");
    assert_eq!(
        app.oneshot(request)
            .await
            .expect("request dispatches")
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn raw_uploads_are_immutable_and_active_content_downloads_are_sandboxed() {
    let app = local().await;
    let data = "<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>";
    let request = Request::builder()
        .method("POST")
        .uri("/attachments")
        .header("host", "127.0.0.1:4100")
        .header("x-board-filename", "proof.svg")
        .header("content-type", "image/svg+xml")
        .body(Body::from(data))
        .expect("request builds");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("upload dispatches");
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("upload response reads")
        .to_bytes();
    let envelope: Value = serde_json::from_slice(&bytes).expect("metadata parses");
    let attachment = &envelope["attachment"];
    let id = attachment["id"].as_str().expect("attachment has ID");
    let request = Request::builder()
        .uri(format!("/attachments/{id}"))
        .header("host", "127.0.0.1:4100")
        .body(Body::empty())
        .expect("request builds");
    let response = app.oneshot(request).await.expect("download dispatches");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["content-disposition"]
            .to_str()
            .expect("disposition text")
            .starts_with("attachment;")
    );
    assert_eq!(
        response.headers()["content-security-policy"],
        "sandbox; default-src 'none'"
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .expect("download reads")
            .to_bytes(),
        data
    );
}

#[tokio::test]
async fn direct_upload_ticket_binds_content_and_cannot_overwrite_bytes() {
    let app = local().await;
    let (status, ticket) = json_request(
        &app,
        "POST",
        "/attachments/uploads",
        json!({"name":"proof.txt","type":"text/plain","size":5}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let url =
        url::Url::parse(ticket["url"].as_str().expect("ticket URL exists")).expect("URL parses");
    let target = format!(
        "{}?{}",
        url.path(),
        url.query().expect("permit query exists")
    );
    for (bytes, status) in [
        ("bad", StatusCode::BAD_REQUEST),
        ("proof", StatusCode::NO_CONTENT),
        ("proof", StatusCode::CONFLICT),
    ] {
        let request = Request::builder()
            .method("PUT")
            .uri(&target)
            .header("host", "127.0.0.1:4100")
            .header("content-type", "text/plain")
            .body(Body::from(bytes))
            .expect("request builds");
        assert_eq!(
            app.clone()
                .oneshot(request)
                .await
                .expect("PUT dispatches")
                .status(),
            status
        );
    }
    let id = ticket["id"].as_str().expect("ID exists");
    let (status, attachment) = json_request(
        &app,
        "POST",
        &format!("/attachments/uploads/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(attachment["attachment"]["id"], id);
    assert_eq!(attachment["attachment"]["size"], 5);
}

#[tokio::test]
async fn invalid_json_and_oversized_bodies_are_protocol_errors() {
    let app = local().await;
    let request = Request::builder()
        .method("POST")
        .uri("/messages")
        .header("host", "127.0.0.1:4100")
        .body(Body::from("{"))
        .expect("request builds");
    assert_eq!(
        app.clone()
            .oneshot(request)
            .await
            .expect("request dispatches")
            .status(),
        StatusCode::BAD_REQUEST
    );
    let request = Request::builder()
        .method("POST")
        .uri("/attachments")
        .header("host", "127.0.0.1:4100")
        .header("content-length", "4194305")
        .body(Body::empty())
        .expect("request builds");
    assert_eq!(
        app.oneshot(request)
            .await
            .expect("request dispatches")
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn events_stream_uses_board_event_frames_and_snapshot_cursor() {
    use futures_util::StreamExt;
    let app = local().await;
    let (status, _) = json_request(
        &app,
        "POST",
        "/messages",
        json!({"channel":"work","text":"ready"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let request = Request::builder()
        .uri("/events?last=10")
        .header("host", "127.0.0.1:4100")
        .header("accept", "text/event-stream")
        .body(Body::empty())
        .expect("request builds");
    let response = app.oneshot(request).await.expect("SSE dispatches");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut chunks = response.into_body().into_data_stream();
    let mut output = String::new();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(chunk) = chunks.next().await {
            output.push_str(&String::from_utf8_lossy(&chunk.expect("SSE chunk reads")));
            if output.contains("event: board") {
                break;
            }
        }
    })
    .await
    .expect("a posted message appears in the stream");
    assert!(output.contains("id:"));
    assert!(output.contains("ready"));
}

#[tokio::test]
async fn public_routes_have_bounded_request_limits() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("board opens");
    let app = router(
        board,
        ServerConfig {
            requests_per_minute: 1,
            blobs: BlobConfig::Memory,
            ..Default::default()
        },
    )
    .await
    .expect("router initializes");
    assert_eq!(
        json_request(&app, "GET", "/healthz", Value::Null).await.0,
        StatusCode::OK
    );
    let (status, body) = json_request(&app, "GET", "/healthz", Value::Null).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "rate_limited");
}

#[tokio::test]
async fn stream_limits_release_capacity_when_the_reader_disconnects() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("board opens");
    let app = router(
        board,
        ServerConfig {
            max_streams_per_principal: 1,
            blobs: BlobConfig::Memory,
            ..Default::default()
        },
    )
    .await
    .expect("router initializes");
    let request = || {
        Request::builder()
            .uri("/events")
            .header("host", "127.0.0.1:4100")
            .header("accept", "text/event-stream")
            .body(Body::empty())
            .expect("request builds")
    };
    let first = app.clone().oneshot(request()).await.expect("stream opens");
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        app.clone()
            .oneshot(request())
            .await
            .expect("request dispatches")
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(first);
    assert_eq!(
        app.oneshot(request())
            .await
            .expect("stream reopens")
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn inspector_trailing_slash_serves_html_with_a_fresh_boot_script_nonce() {
    let directory = tempfile::tempdir().expect("test asset directory creates");
    std::fs::write(
        directory.path().join("index.html"),
        "<!doctype html><script type=\"module\">console.log('boot')</script>",
    )
    .expect("test shell writes");
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("board opens");
    let app = router(
        board,
        ServerConfig {
            inspector_dir: Some(directory.path().to_path_buf()),
            blobs: BlobConfig::Memory,
            ..Default::default()
        },
    )
    .await
    .expect("router initializes");
    let request = |path: &str| {
        Request::builder()
            .uri(path)
            .header("host", "127.0.0.1:4100")
            .body(Body::empty())
            .expect("request builds")
    };
    let redirect = app
        .clone()
        .oneshot(request("/inspect"))
        .await
        .expect("request dispatches");
    assert_eq!(redirect.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(redirect.headers()["location"], "/inspect/");
    let response = app
        .clone()
        .oneshot(request("/inspect/"))
        .await
        .expect("inspector serves");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "trailing slash must not redirect to itself"
    );
    let policy = response.headers()["content-security-policy"]
        .to_str()
        .expect("policy text")
        .to_owned();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("shell reads")
        .to_bytes();
    let html = String::from_utf8(bytes.to_vec()).expect("shell is UTF-8");
    let nonce = html
        .split("nonce=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("boot script has a nonce");
    assert!(policy.contains(&format!("'nonce-{nonce}'")));
    let second = app
        .oneshot(request("/inspect/"))
        .await
        .expect("next shell serves");
    assert_ne!(
        second.headers()["content-security-policy"],
        policy,
        "each page gets a fresh nonce"
    );
}

#[tokio::test]
async fn percent_encoded_schema_references_are_decoded_once() {
    let app = local().await;
    let (status,_)=json_request(&app,"POST","/schemas",json!({"name":"transport-schema","version":1,"schema":{"type":"object","properties":{"ok":{"type":"boolean"}}}})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, value) =
        json_request(&app, "GET", "/schemas/transport-schema%401", Value::Null).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "encoded @ must address the same schema version"
    );
    assert_eq!(value["schema"]["name"], "transport-schema");
}

#[tokio::test]
async fn polling_and_streaming_preserve_event_query_validation() {
    let app = local().await;
    for accept in ["application/json", "text/event-stream"] {
        for path in [
            "/events?wait=invalid",
            "/events?wait=0&wait=1",
            "/events?last=10&wait=0",
            "/events?last=10&wait=1",
        ] {
            let request = Request::builder()
                .uri(path)
                .header("host", "127.0.0.1:4100")
                .header("accept", accept)
                .body(Body::empty())
                .expect("request builds");
            let response = app
                .clone()
                .oneshot(request)
                .await
                .expect("request dispatches");
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{accept} rejects {path}"
            );
        }
    }
}

#[tokio::test]
async fn ready_task_claim_rejects_a_nonnumeric_wait_before_claiming() {
    let app = local().await;
    let (status, _) = json_request(
        &app,
        "POST",
        "/tasks",
        json!({"channel":"work","text":"remain ready"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = json_request(
        &app,
        "POST",
        "/tasks/claim",
        json!({"channel":"work","wait":"invalid"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "invalid wait cannot consume the ready task"
    );
    let (status, claimed) = json_request(
        &app,
        "POST",
        "/tasks/claim",
        json!({"channel":"work","wait":0}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(claimed["task"]["text"], "remain ready");
}
