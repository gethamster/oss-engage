use std::time::Duration;

use engage_server::{BlobConfig, ServerConfig, serve};
use engage_store::Board;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn standalone_shutdown_drains_event_streams_long_polls_and_partial_uploads() {
    let reserved = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test port reserves");
    let address = reserved.local_addr().expect("test address exists");
    drop(reserved);
    let board = Board::connect("sqlite::memory:")
        .await
        .expect("board opens");
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let mut server = tokio::spawn(serve(
        board,
        ServerConfig {
            bind: address,
            blobs: BlobConfig::Memory,
            ..Default::default()
        },
        async move {
            let _ = stopped.await;
        },
    ));
    let mut stream = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match tokio::net::TcpStream::connect(address).await {
                Ok(connection) => break connection,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("listener starts");
    stream
        .write_all(
            format!("GET /events HTTP/1.1\r\nHost: {address}\r\nAccept: text/event-stream\r\n\r\n")
                .as_bytes(),
        )
        .await
        .expect("stream request writes");
    let mut bytes = [0; 2048];
    let count = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut bytes))
        .await
        .expect("SSE starts")
        .expect("SSE response reads");
    assert!(String::from_utf8_lossy(&bytes[..count]).contains("200 OK"));
    let mut poll = tokio::net::TcpStream::connect(address)
        .await
        .expect("poll connects");
    poll.write_all(format!("GET /events?wait=300 HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes())
        .await
        .expect("long poll writes");
    let mut upload = tokio::net::TcpStream::connect(address)
        .await
        .expect("upload connects");
    upload.write_all(format!("POST /attachments HTTP/1.1\r\nHost: {address}\r\nContent-Length: 100\r\n\r\npending").as_bytes())
        .await
        .expect("partial upload writes");
    tokio::time::sleep(Duration::from_millis(50)).await;
    stop.send(()).expect("shutdown signals");
    let result = tokio::time::timeout(Duration::from_secs(2), &mut server).await;
    server.abort();
    assert!(
        result.is_ok(),
        "active HTTP work must not hold shutdown for its full wait or body"
    );
    result
        .expect("shutdown finishes")
        .expect("server task joins")
        .expect("server shuts down cleanly");
}
