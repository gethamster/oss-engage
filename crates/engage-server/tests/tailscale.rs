#![cfg(unix)]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use engage_server::{AuthConfig, BlobConfig, ServerConfig, router};
use engage_store::Board;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

#[tokio::test]
async fn tailnet_identity_uses_daemon_and_actual_peer_instead_of_forwarded_claims() {
    let temporary = tempfile::tempdir().expect("test directory creates");
    let path = temporary.path().join("tailscaled.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("fake daemon binds");
    let verifier = tokio::spawn(async move {
        let (mut connection, _) = listener.accept().await.expect("WhoIs arrives");
        let mut bytes = vec![0; 4096];
        let read = connection.read(&mut bytes).await.expect("WhoIs reads");
        let request = String::from_utf8_lossy(&bytes[..read]).into_owned();
        let payload=json!({"UserProfile":{"ID":123,"LoginName":"alice@example.test","DisplayName":"Alice Verified"},"Node":{"ID":456,"Tags":["tag:agent"]}}).to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
            payload.len()
        );
        connection
            .write_all(response.as_bytes())
            .await
            .expect("daemon responds");
        request
    });
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("board opens");
    let app = router(
        board,
        ServerConfig {
            bind: "100.64.0.1:4100".parse().expect("tailnet address parses"),
            auth: AuthConfig::Tailscale {
                socket: path,
                owner_logins: vec![],
            },
            blobs: BlobConfig::Memory,
            ..Default::default()
        },
    )
    .await
    .expect("tailnet router initializes");
    let actual_peer = "100.64.0.2:12345"
        .parse::<std::net::SocketAddr>()
        .expect("peer address parses");
    let request = Request::builder()
        .uri("/me")
        .header("host", "100.64.0.1:4100")
        .header("x-forwarded-for", "100.64.0.99")
        .header("x-board-name", "Forged")
        .extension(ConnectInfo(actual_peer))
        .body(Body::empty())
        .expect("request builds");
    let response = app.oneshot(request).await.expect("request dispatches");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("identity reads")
        .to_bytes();
    let identity: Value = serde_json::from_slice(&bytes).expect("identity parses");
    assert_eq!(identity["principal"], "tailscale:user:123");
    assert_eq!(identity["handle"], "alice@example.test");
    assert_eq!(identity["name"], "Alice Verified");
    assert_eq!(
        identity["can"],
        json!({}),
        "tailnet admission alone does not grant owner access"
    );
    let request = verifier.await.expect("WhoIs task completes");
    assert!(
        request.contains("addr=100.64.0.2%3A12345"),
        "WhoIs receives actual socket peer: {request}"
    );
    assert!(
        !request.contains("100.64.0.99"),
        "forwarded identity is not trusted"
    );
}
