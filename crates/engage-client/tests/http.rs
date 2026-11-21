use std::io;

use engage_client::{Client, ClientError, PostOptions, TaskClaim};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn fixture(
    status: &str,
    body: &str,
) -> io::Result<(String, tokio::task::JoinHandle<io::Result<String>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/api/board", listener.local_addr()?);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut request = Vec::new();
        let mut buffer = [0; 2048];
        loop {
            let count = socket.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
            if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&request[..header_end]);
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + length {
                    break;
                }
            }
        }
        socket.write_all(response.as_bytes()).await?;
        Ok(String::from_utf8_lossy(&request).into_owned())
    });
    Ok((url, task))
}

#[tokio::test]
async fn request_keeps_mount_repeated_filters_and_profile() -> Result<(), Box<dyn std::error::Error>>
{
    let (url, request) = fixture("200 OK", r#"{"threads":[],"cursor":"opaque"}"#).await?;
    let client = Client::new(&url)?
        .with_token("credential")?
        .with_profile("adam")?;
    let result = client
        .threads(&[
            ("label".into(), "state=open".into()),
            ("label".into(), "step=fix".into()),
        ])
        .await?;
    assert_eq!(result["cursor"], "opaque");
    let request = request.await??;
    assert!(
        request.starts_with("GET /api/board/threads?label=state%3Dopen&label=step%3Dfix "),
        "{request}"
    );
    assert!(request.contains("x-board-profile: adam"));
    assert!(request.contains("authorization: Bearer credential"));
    assert!(!format!("{client:?}").contains("credential"));
    Ok(())
}

#[tokio::test]
async fn api_errors_keep_actionable_code_and_hint() -> Result<(), Box<dyn std::error::Error>> {
    let (url, request) = fixture(
        "409 Conflict",
        r#"{"error":{"code":"claim_lost","message":"the lease expired","hint":"claim again"}}"#,
    )
    .await?;
    let error = Client::new(&url)?
        .task_action(
            "task/opaque",
            "complete",
            &json!({"epoch":1,"token":"private"}),
        )
        .await
        .expect_err("conflicting claim must fail");
    match error {
        ClientError::Api {
            status,
            code,
            message,
            hint,
        } => {
            assert_eq!(status, 409);
            assert_eq!(code, "claim_lost");
            assert_eq!(message, "the lease expired");
            assert_eq!(hint.as_deref(), Some("claim again"));
        }
        other => panic!("expected API conflict, received {other:?}"),
    }
    assert!(
        request
            .await??
            .starts_with("POST /api/board/tasks/task%2Fopaque/complete ")
    );
    Ok(())
}

#[tokio::test]
async fn post_options_preserve_idempotency_and_conditions() -> Result<(), Box<dyn std::error::Error>>
{
    let (url, request) = fixture("200 OK", r#"{"message":{"id":"opaque-id"}}"#).await?;
    let options = PostOptions {
        re: Some("thread".into()),
        text: "proof".into(),
        id: Some("operation".into()),
        after: Some("latest".into()),
        ..PostOptions::default()
    };
    Client::new(&url)?.post_with(&options).await?;
    let request = request.await??;
    let body: serde_json::Value = serde_json::from_str(
        request
            .split_once("\r\n\r\n")
            .expect("request has headers")
            .1,
    )?;
    assert_eq!(
        body,
        json!({"re":"thread","text":"proof","id":"operation","after":"latest"})
    );
    Ok(())
}

#[test]
fn authentication_and_claim_debug_are_redacted() -> Result<(), Box<dyn std::error::Error>> {
    assert!(Client::new("file:///tmp/board").is_err());
    assert!(Client::new("https://user:password@board.example").is_err());
    assert!(Client::new("https://board.example?token=private").is_err());
    assert!(
        Client::new("https://board.example")?
            .with_token("bad\r\nheader")
            .is_err()
    );
    let claim = TaskClaim {
        epoch: 1,
        token: "secret-value".into(),
        until: "2027-01-01T00:00:00Z".into(),
    };
    assert!(!format!("{claim:?}").contains("secret-value"));
    Ok(())
}

#[tokio::test]
async fn encoded_traversal_cannot_escape_mount() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new("https://board.example/mount")?;
    let error = client
        .request("GET", "/%2e%2e/private", &[], None)
        .await
        .expect_err("must reject traversal before transport");
    assert!(matches!(error, ClientError::InvalidUrl(_)));
    Ok(())
}

#[tokio::test]
async fn private_channel_rejects_unmapped_identity_before_grant_writes()
-> Result<(), Box<dyn std::error::Error>> {
    let (url, request) = fixture("200 OK", r#"{"principal":"service:unmapped"}"#).await?;
    let error = Client::new(&url)?
        .create_private_channel("security")
        .await
        .expect_err("unsafe implicit principal must be refused");
    assert!(
        matches!(error,ClientError::Api {status:400,ref code,..} if code == "no_own_principal")
    );
    assert!(request.await??.starts_with("GET /api/board/me "));
    Ok(())
}
