use axum::body::Body;
use axum::http::{Request, StatusCode};
use engage_server::{BlobConfig, ServerConfig, Service};
use engage_store::Board;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn embedded_service_shares_memory_blobs_and_runs_its_durable_scheduler() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("board opens");
    let service = Service::new(
        board,
        ServerConfig {
            blobs: BlobConfig::Memory,
            ..Default::default()
        },
    )
    .await
    .expect("service initializes");
    let first = service.router();
    let second = service.router();
    let upload = Request::builder()
        .method("POST")
        .uri("/attachments")
        .header("host", "127.0.0.1:4100")
        .header("content-type", "text/plain")
        .body(Body::from("proof"))
        .expect("request builds");
    let response = first
        .clone()
        .oneshot(upload)
        .await
        .expect("upload dispatches");
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("metadata reads")
        .to_bytes();
    let metadata: Value = serde_json::from_slice(&bytes).expect("metadata parses");
    let id = metadata["attachment"]["id"].as_str().expect("ID exists");
    let download = Request::builder()
        .uri(format!("/attachments/{id}"))
        .header("host", "127.0.0.1:4100")
        .body(Body::empty())
        .expect("request builds");
    let response = second
        .clone()
        .oneshot(download)
        .await
        .expect("download dispatches");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "routers share the same blob instance"
    );
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .expect("download reads")
            .to_bytes(),
        "proof"
    );
    let workers = service.start_workers();
    let scheduled = Request::builder()
        .method("POST")
        .uri("/schedules")
        .header("host", "127.0.0.1:4100")
        .body(Body::from(
            json!({"channel":"work","text":"scheduled proof","in":"0.01s"}).to_string(),
        ))
        .expect("request builds");
    assert_eq!(
        first
            .oneshot(scheduled)
            .await
            .expect("schedule dispatches")
            .status(),
        StatusCode::CREATED
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let request = Request::builder()
                .uri("/messages?channel=work")
                .header("host", "127.0.0.1:4100")
                .body(Body::empty())
                .expect("request builds");
            let response = second
                .clone()
                .oneshot(request)
                .await
                .expect("messages read");
            let bytes = response
                .into_body()
                .collect()
                .await
                .expect("messages body reads")
                .to_bytes();
            if String::from_utf8_lossy(&bytes).contains("scheduled proof") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("embedded scheduler posts due work");
    workers
        .shutdown()
        .await
        .expect("all owned workers stop and join");
}

#[tokio::test]
async fn embedded_request_drain_applies_to_existing_and_future_router_clones() {
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("board opens");
    let service = Service::new(
        board,
        ServerConfig {
            blobs: BlobConfig::Memory,
            ..Default::default()
        },
    )
    .await
    .expect("service initializes");
    let existing = service.router();
    service.drain_requests();
    for router in [existing, service.router()] {
        let request = Request::builder()
            .uri("/healthz")
            .header("host", "127.0.0.1:4100")
            .body(Body::empty())
            .expect("request builds");
        let response = router.oneshot(request).await.expect("request dispatches");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
