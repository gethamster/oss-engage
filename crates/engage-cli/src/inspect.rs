use std::sync::Arc;

use anyhow::Result;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use engage_client::{Client, ProxyHeaders};

#[derive(Clone)]
struct Proxy {
    client: Client,
    authority: String,
    origin: String,
    key: String,
    cookie: String,
}

pub async fn serve(client: Client, open: bool) -> Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let key = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let cookie = format!("engage_proxy_{}", uuid::Uuid::new_v4().simple());
    let origin = format!("http://{address}");
    let launch = format!("{origin}/inspect/?key={key}");
    let state = Proxy {
        client,
        authority: address.to_string(),
        origin,
        key,
        cookie,
    };
    let app = axum::Router::new()
        .fallback(any(forward))
        .with_state(Arc::new(state));
    if open {
        crate::run::open_browser(&launch)?;
    } else {
        println!("{launch}");
    }
    eprintln!("Inspector proxy listening at http://{address}; press Ctrl-C to close");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

async fn forward(State(state): State<Arc<Proxy>>, request: Request<Body>) -> Response {
    let headers = request.headers();
    if header(headers, "host") != Some(state.authority.as_str()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let query: Vec<(String, String)> =
        url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
            .into_owned()
            .collect();
    if request.method() == Method::GET
        && request.uri().path() == "/inspect/"
        && query
            .iter()
            .any(|(name, value)| name == "key" && value == &state.key)
    {
        let cookie = format!(
            "{}={}; Path=/; HttpOnly; SameSite=Strict",
            state.cookie, state.key
        );
        return (
            StatusCode::SEE_OTHER,
            [
                ("location", "/inspect/"),
                ("set-cookie", cookie.as_str()),
                ("cache-control", "no-store"),
                ("referrer-policy", "no-referrer"),
            ],
        )
            .into_response();
    }
    let cookie = format!("{}={}", state.cookie, state.key);
    if !header(headers, "cookie")
        .is_some_and(|value| value.split(';').any(|part| part.trim() == cookie))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if let Some(origin) = header(headers, "origin") {
        if origin != state.origin {
            return StatusCode::FORBIDDEN.into_response();
        }
    } else if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if header(headers, "sec-fetch-site").is_some_and(|site| site != "same-origin" && site != "none")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if request.method() == Method::DELETE && request.uri().path() == "/auth/session" {
        let cookie = format!(
            "{}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0",
            state.cookie
        );
        return (
            [("set-cookie", cookie)],
            axum::Json(serde_json::json!({"ok":true})),
        )
            .into_response();
    }
    // The local browser never establishes a second credential session remotely.
    if request.uri().path().starts_with("/auth/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let content_type = header(headers, "content-type").map(str::to_owned);
    let filename = header(headers, "x-board-filename").map(str::to_owned);
    let accept = header(headers, "accept").map(str::to_owned);
    let profile = header(headers, "x-board-profile").map(str::to_owned);
    let client = if let Some(profile) = profile {
        if profile.len() > 4096 {
            return StatusCode::BAD_REQUEST.into_response();
        }
        match state.client.clone().with_profile(&profile) {
            Ok(client) => client,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        }
    } else {
        state.client.clone()
    };
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let body = match to_bytes(request.into_body(), 100 * 1024 * 1024).await {
        Ok(bytes) => bytes.to_vec(),
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let mut response =
        match client
            .proxy_request(
                &method,
                &path,
                &query,
                ProxyHeaders {
                    content_type: content_type.as_deref(),
                    filename: filename.as_deref(),
                    accept: accept.as_deref(),
                },
                body,
            )
            .await
        {
            Ok(response) => response,
            Err(error) => return (
                StatusCode::BAD_GATEWAY,
                axum::Json(
                    serde_json::json!({"error":{"code":"proxy_error","message":error.to_string()}}),
                ),
            )
                .into_response(),
        };
    let mut builder = Response::builder().status(response.status().as_u16());
    for name in [
        "content-type",
        "content-disposition",
        "cache-control",
        "etag",
        "last-modified",
        "content-security-policy",
        "x-content-type-options",
        "referrer-policy",
    ] {
        if let Some(value) = response.headers().get(name) {
            builder = builder.header(name, value);
        }
    }
    if !path.starts_with("/inspect/") {
        builder = builder.header("cache-control", "no-store");
    }
    if method == "POST" && path == "/attachments/uploads" && response.status().is_success() {
        let mut bytes = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) if bytes.len() + chunk.len() <= 64 * 1024 => {
                    bytes.extend_from_slice(&chunk)
                }
                Ok(None) => break,
                _ => return StatusCode::BAD_GATEWAY.into_response(),
            }
        }
        let Ok(mut ticket) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
        if let Some(address) = ticket["url"].as_str() {
            match upload_url(state.client.base_url(), &state.origin, address) {
                Ok(Some(address)) => ticket["url"] = address.into(),
                Ok(None) => {}
                Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
            }
        }
        return builder
            .body(Body::from(ticket.to_string()))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    }
    if path.starts_with("/inspect/")
        && response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|value| value.starts_with("text/html"))
    {
        let html = match response.text().await {
            Ok(html) => html,
            Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
        };
        let html = html.replacen(
            "<head>",
            "<head><meta name=\"engage-proxy\" content=\"true\">",
            1,
        );
        return builder
            .body(Body::from(html))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    }
    // Omit upstream cookies, redirects, CORS, and hop-by-hop headers.
    builder
        .header("x-content-type-options", "nosniff")
        .body(Body::from_stream(response.bytes_stream()))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

fn upload_url(board: &str, proxy: &str, ticket: &str) -> Result<Option<String>> {
    let board = url::Url::parse(board)?;
    let ticket = url::Url::parse(ticket)?;
    anyhow::ensure!(
        matches!(ticket.scheme(), "http" | "https")
            && ticket.username().is_empty()
            && ticket.password().is_none()
            && ticket.fragment().is_none(),
        "invalid upload ticket URL"
    );
    if ticket.origin() != board.origin() {
        return Ok(None);
    }
    let path = ticket
        .path()
        .strip_prefix(board.path())
        .ok_or_else(|| anyhow::anyhow!("upload ticket escaped board mount"))?;
    let mut result = url::Url::parse(proxy)?;
    result.set_path(&format!("/{path}"));
    result.set_query(ticket.query());
    Ok(Some(result.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> Arc<Proxy> {
        Arc::new(Proxy {
            client: Client::new("https://board.example").expect("valid URL"),
            authority: "127.0.0.1:1234".into(),
            origin: "http://127.0.0.1:1234".into(),
            key: "private".into(),
            cookie: "test".into(),
        })
    }
    #[tokio::test]
    async fn proxy_requires_host_cookie_and_same_origin() {
        let request = Request::builder()
            .uri("/messages")
            .header("host", "evil.example")
            .body(Body::empty())
            .expect("request");
        assert_eq!(
            forward(State(state()), request).await.status(),
            StatusCode::FORBIDDEN
        );
        let request = Request::builder()
            .uri("/messages")
            .header("host", "127.0.0.1:1234")
            .body(Body::empty())
            .expect("request");
        assert_eq!(
            forward(State(state()), request).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let request = Request::builder()
            .method("POST")
            .uri("/messages")
            .header("host", "127.0.0.1:1234")
            .header("cookie", "test=private")
            .header("origin", "https://evil.example")
            .body(Body::empty())
            .expect("request");
        assert_eq!(
            forward(State(state()), request).await.status(),
            StatusCode::FORBIDDEN
        );
    }
    #[tokio::test]
    async fn launch_token_becomes_httponly_cookie_and_leaves_url() {
        let request = Request::builder()
            .uri("/inspect/?key=private")
            .header("host", "127.0.0.1:1234")
            .body(Body::empty())
            .expect("request");
        let response = forward(State(state()), request).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()["location"], "/inspect/");
        assert!(
            response.headers()["set-cookie"]
                .to_str()
                .expect("cookie")
                .contains("HttpOnly; SameSite=Strict")
        );
    }

    #[test]
    fn upload_tickets_only_rewrite_the_configured_board_origin_and_mount() {
        assert_eq!(
            upload_url(
                "https://board.example/api/board/",
                "http://127.0.0.1:1234",
                "https://board.example/api/board/attachments/uploads/att_id/data?ticket=secret"
            )
            .expect("ticket rewrites")
            .as_deref(),
            Some("http://127.0.0.1:1234/attachments/uploads/att_id/data?ticket=secret")
        );
        assert!(
            upload_url(
                "https://board.example/api/board/",
                "http://127.0.0.1:1234",
                "https://board.example/admin"
            )
            .is_err()
        );
        assert!(
            upload_url(
                "https://board.example/",
                "http://127.0.0.1:1234",
                "https://storage.example/upload?signature=store-secret"
            )
            .expect("external store ticket is untouched")
            .is_none()
        );
        assert!(
            upload_url(
                "https://board.example/",
                "http://127.0.0.1:1234",
                "https://board.example.evil/upload"
            )
            .expect("different origin is not proxied")
            .is_none()
        );
    }

    #[tokio::test]
    async fn inspector_profile_and_native_uploads_stay_behind_the_proxy() {
        use http_body_util::BodyExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream binds");
        let remote = format!(
            "http://{}/api/board/",
            listener.local_addr().expect("upstream address exists")
        );
        let seen = Arc::new(std::sync::Mutex::new(Vec::<HeaderMap>::new()));
        let received = seen.clone();
        let upstream_base = remote.clone();
        let app=axum::Router::new().fallback(any(move |request:Request<Body>| {
            let seen=received.clone();let base=upstream_base.clone();async move {
                seen.lock().expect("test capture lock").push(request.headers().clone());
                axum::Json(serde_json::json!({"id":"att_test","url":format!("{base}attachments/uploads/att_test/data?ticket=one-use"),"headers":{"content-type":"text/plain"}}))
            }
        }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("fake upstream serves");
        });
        let token = uuid::Uuid::new_v4().to_string();
        let state = Arc::new(Proxy {
            client: Client::new(&remote)
                .expect("URL parses")
                .with_token(&token)
                .expect("token header"),
            authority: "127.0.0.1:1234".into(),
            origin: "http://127.0.0.1:1234".into(),
            key: "private".into(),
            cookie: "test".into(),
        });
        let request = Request::builder()
            .method("POST")
            .uri("/attachments/uploads")
            .header("host", &state.authority)
            .header("origin", &state.origin)
            .header("cookie", "test=private")
            .header("x-board-profile", "codex")
            .header("content-type", "application/json")
            .body(Body::from("{\"size\":5}"))
            .expect("request builds");
        let response = forward(State(state), request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("ticket response reads")
            .to_bytes();
        let ticket: serde_json::Value = serde_json::from_slice(&bytes).expect("ticket parses");
        assert_eq!(
            ticket["url"],
            "http://127.0.0.1:1234/attachments/uploads/att_test/data?ticket=one-use"
        );
        let headers = seen.lock().expect("test capture lock");
        assert_eq!(headers[0]["x-board-profile"], "codex");
        assert_eq!(headers[0]["authorization"], format!("Bearer {token}"));
        assert!(
            !headers[0].contains_key("cookie"),
            "proxy session stays local"
        );
        assert!(
            !String::from_utf8_lossy(&bytes).contains(&token),
            "source token never enters browser response"
        );
        server.abort();
    }
}
