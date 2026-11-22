use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn fixture(body: Value) -> (Slack, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("connection");
        let mut request = Vec::new();
        loop {
            let mut buffer = [0; 8192];
            let count = stream.read(&mut buffer).await.expect("request bytes");
            assert!(count > 0, "request ends after the declared body");
            request.extend_from_slice(&buffer[..count]);
            let text = String::from_utf8_lossy(&request);
            if let Some((headers, body)) = text.split_once("\r\n\r\n") {
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if body.len() >= length {
                    break;
                }
            }
        }
        let body = body.to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("response");
        String::from_utf8(request).expect("HTTP request")
    });
    let credentials = crate::StaticCredentials(crate::Credentials {
        access_token: crate::Secret::new("fixture"),
        webhook_secret: crate::Secret::new("fixture"),
        bot_user_id: None,
    });
    let mut provider = Slack::new(credentials);
    provider.api.base = format!("http://{address}");
    (provider, task)
}

#[tokio::test]
async fn slack_api_form_encodes_read_and_structured_arguments() {
    let (provider, request) = fixture(json!({"ok":true})).await;
    provider
        .call(
            "conversations.info",
            json!({"channel":"C1","include_locale":true,"metadata":{"event_type":"proof"}}),
        )
        .await
        .expect("call");
    let request = request.await.expect("server");
    assert!(
        request
            .to_ascii_lowercase()
            .contains("content-type: application/x-www-form-urlencoded"),
        "Slack read methods require form arguments"
    );
    let (_, body) = request.split_once("\r\n\r\n").expect("body");
    let arguments = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(arguments["channel"], "C1");
    assert_eq!(arguments["include_locale"], "true");
    assert_eq!(arguments["metadata"], "{\"event_type\":\"proof\"}");
}

#[tokio::test]
async fn reaction_to_reply_uses_its_parent_thread() {
    let (provider, request) =
        fixture(json!({"ok":true,"type":"message","message":{"ts":"1.2","thread_ts":"1.1"}})).await;
    let incoming = provider.normalized("slack", &json!({"event_id":"reaction-1","event":{"type":"reaction_added","user":"U2","reaction":"heart","item":{"type":"message","channel":"C1","ts":"1.2"}}})).await.expect("reaction normalized");
    assert_eq!(
        incoming[0].thread, "slack:C1:1.1",
        "reactions must address the imported reply's original conversation"
    );
    assert_eq!(incoming[0].event["message"], "1.2");
    let request = request.await.expect("server");
    assert!(request.starts_with("POST /reactions.get "));
}
