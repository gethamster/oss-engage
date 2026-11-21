use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use engage_core::{BoardError, Principal};
use engage_store::Board;
use serde_json::{Value, json};
use tower_http::trace::TraceLayer;

use crate::{ServerConfig, auth::Authenticator, blobs::Blobs, error, feed, inspector};

pub(crate) struct App {
    pub board: Board,
    pub config: ServerConfig,
    pub auth: Authenticator,
    pub blobs: Blobs,
    pub limits: crate::limits::Limits,
    pub sync_ready: std::sync::atomic::AtomicBool,
    pub wake: tokio::sync::Notify,
}

pub(crate) fn router(app: Arc<App>) -> Router {
    Router::new()
        .fallback(handle)
        .with_state(app)
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request| {
                // Signed upload URLs and authorization headers never enter request spans.
                tracing::info_span!("http", method = %request.method(), path = request.uri().path())
            }),
        )
}

async fn handle(State(app): State<Arc<App>>, request: Request) -> Response {
    let result = dispatch(app, request).await;
    match result {
        Ok(response) => response,
        Err(cause) => failure(cause),
    }
}

async fn dispatch(app: Arc<App>, request: Request) -> Result<Response, BoardError> {
    let (parts, body) = request.into_parts();
    let peer = parts
        .extensions
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0);
    app.limits.request(
        &peer
            .map(|peer| peer.ip().to_string())
            .unwrap_or_else(|| "embedded".into()),
    )?;
    if matches!(app.config.auth, crate::AuthConfig::Local)
        && peer.is_some_and(|peer| !peer.ip().is_loopback())
    {
        return Err(error(
            403,
            "invalid_peer",
            "local board requires a loopback peer",
        ));
    }
    guard_browser(&app.config, &parts.headers, &parts.method)?;
    let decoded = decode_path(parts.uri.path())?;
    let path = decoded.trim_end_matches('/');
    let path = if path.is_empty() { "/" } else { path };
    let method = parts.method.as_str();
    let query: Vec<(String, String)> =
        url::form_urlencoded::parse(parts.uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    if method == "POST" && path == "/wake" {
        // Untrusted notifications carry no event or identity; workers pull their own leases.
        app.wake.notify_one();
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    if method == "GET" && path == "/healthz" {
        return Ok(json_response(
            StatusCode::OK,
            json!({"status":"ok","version":env!("CARGO_PKG_VERSION")}),
        ));
    }
    if method == "GET" && path == "/readyz" {
        if !app.sync_ready.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(error(
                503,
                "not_ready",
                "sync service principals have not registered",
            ));
        }
        app.board
            .request(&Principal::default(), "GET", "/", &[], Value::Null)
            .await
            .map_err(|_| error(503, "not_ready", "board storage is unavailable"))?;
        return Ok(json_response(StatusCode::OK, json!({"status":"ready"})));
    }
    if method == "GET" && (path == "/inspect" || path.starts_with("/inspect/")) {
        return inspector::serve(&app, parts.uri.path()).await;
    }
    if method == "GET" && path == "/" {
        let base_url = base_url(&app.config, &parts.headers, &parts.uri)?;
        return Ok(json_response(
            StatusCode::OK,
            json!({
                "version":"0.53.0","url":base_url,"audience":base_url,
                "team_id":null,"project_id":null,
                "sync":app.config.sync.as_ref().map(|r|r.discovery()).unwrap_or_default()
            }),
        ));
    }
    if method == "PUT" && path.starts_with("/attachments/uploads/") && path.ends_with("/data") {
        let id = path
            .strip_prefix("/attachments/uploads/")
            .and_then(|s| s.strip_suffix("/data"))
            .ok_or_else(|| error(404, "not_found", "no upload"))?;
        let permit = query
            .iter()
            .find(|(key, _)| key == "ticket")
            .map(|(_, value)| value.as_str())
            .ok_or_else(|| error(403, "invalid_upload", "upload ticket is required"))?;
        let allowed_size = app
            .blobs
            .preflight_ticket(id, permit, header(&parts.headers, "content-type"))
            .await?;
        let bytes = read_body(body, &parts.headers, allowed_size).await?;
        app.blobs
            .put_ticket(id, permit, header(&parts.headers, "content-type"), bytes)
            .await?;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    if method == "POST" && path.starts_with("/webhooks/") {
        let bytes = read_body(body, &parts.headers, app.config.max_body_bytes).await?;
        return crate::platform::webhook(
            &app,
            path.trim_start_matches("/webhooks/"),
            &parts.headers,
            bytes,
        )
        .await;
    }
    if path == "/auth/session" && method == "DELETE" {
        return Ok((
            StatusCode::NO_CONTENT,
            [("set-cookie", crate::auth::clear_cookie())],
        )
            .into_response());
    }
    let identity = app.auth.identify(&parts.headers, peer).await?;
    if path == "/auth/session" && method == "POST" {
        if !parts.headers.contains_key("authorization")
            && !matches!(
                app.config.auth,
                crate::AuthConfig::Local | crate::AuthConfig::Tailscale { .. }
            )
        {
            return Err(error(
                401,
                "unauthorized",
                "sign in with a bearer credential",
            ));
        }
        let secure = app
            .config
            .public_url
            .as_deref()
            .is_some_and(|address| address.starts_with("https://"));
        let cookie = app.auth.session_cookie(identity, secure)?;
        let mut response = StatusCode::NO_CONTENT.into_response();
        response.headers_mut().insert(
            "set-cookie",
            HeaderValue::from_str(&cookie)
                .map_err(|_| error(500, "auth_failed", "cannot issue browser session"))?,
        );
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-store"));
        return Ok(response);
    }
    if method == "GET"
        && path == "/events"
        && header(&parts.headers, "accept").is_some_and(|value| value.contains("text/event-stream"))
    {
        return feed::stream(app, parts.headers, query, peer).await;
    }
    if method == "POST" && path == "/attachments" {
        app.board
            .request(&identity.principal, "GET", "/me", &[], Value::Null)
            .await?;
        let bytes = read_body(
            body,
            &parts.headers,
            crate::blobs::UPLOAD_MAX.min(app.config.max_body_bytes),
        )
        .await?;
        let name = header(&parts.headers, "x-board-filename")
            .map(|value| {
                if malformed_percent(value) {
                    return Err(error(
                        400,
                        "bad_request",
                        "x-board-filename is not URI-encoded",
                    ));
                }
                percent_encoding::percent_decode_str(value)
                    .decode_utf8()
                    .map(|s| s.into_owned())
                    .map_err(|_| error(400, "bad_request", "x-board-filename is not URI-encoded"))
            })
            .transpose()?;
        let result = app
            .blobs
            .upload(
                &app.board,
                &identity.principal,
                bytes,
                name.as_deref(),
                header(&parts.headers, "content-type"),
            )
            .await?;
        return Ok(json_response(
            StatusCode::CREATED,
            json!({"attachment":result}),
        ));
    }
    if method == "GET"
        && path.starts_with("/attachments/")
        && !path.starts_with("/attachments/uploads/")
    {
        let id = path.trim_start_matches("/attachments/");
        let (metadata, bytes) = app
            .blobs
            .download(&app.board, &identity.principal, id)
            .await?;
        return attachment_response(&metadata, bytes);
    }
    let bytes = read_body(body, &parts.headers, app.config.max_body_bytes).await?;
    let input = if bytes.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|_| error(400, "bad_request", "request body is not valid JSON"))?
    };
    if method == "POST" && path == "/attachments/uploads" {
        app.board
            .request(&identity.principal, "GET", "/me", &[], Value::Null)
            .await?;
        let url = base_url(&app.config, &parts.headers, &parts.uri)?;
        return Ok(json_response(
            StatusCode::OK,
            app.blobs.start(&identity.principal, &input, &url).await?,
        ));
    }
    if method == "POST" && path.starts_with("/attachments/uploads/") {
        let id = path.trim_start_matches("/attachments/uploads/");
        return Ok(json_response(
            StatusCode::CREATED,
            json!({"attachment":app.blobs.finish(&app.board, &identity.principal, id).await?}),
        ));
    }
    if path.starts_with("/sources/") {
        return crate::platform::source(&app, &identity.principal, method, path, &query, &input)
            .await;
    }
    if method == "GET" && path == "/links" {
        return crate::platform::linked(&app, &identity.principal, &query).await;
    }
    if method == "POST" && path.starts_with("/messages/") && path.ends_with("/typing") {
        return crate::platform::typing(&app, &identity.principal, path, &input).await;
    }
    let result = feed::poll(
        &app,
        &identity.principal,
        method,
        path,
        &query,
        &input,
        identity.expires_at,
    )
    .await?;
    let created =
        method == "POST" && matches!(path, "/messages" | "/tasks" | "/tasks/batch" | "/schedules");
    Ok(json_response(
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        result,
    ))
}

pub(crate) fn failure(cause: BoardError) -> Response {
    let status = StatusCode::from_u16(cause.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if status.is_server_error() {
        tracing::error!(code = cause.code, "HTTP operation failed");
    }
    let mut response = json_response(
        status,
        json!({"error":{"code":cause.code,"message":cause.message}}),
    );
    if status == StatusCode::UNAUTHORIZED {
        response
            .headers_mut()
            .insert("www-authenticate", HeaderValue::from_static("Bearer"));
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from_static("60"));
    }
    response
}

pub(crate) fn json_response(status: StatusCode, value: Value) -> Response {
    let mut response = (status, axum::Json(value)).into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn read_body(body: Body, headers: &HeaderMap, limit: usize) -> Result<Bytes, BoardError> {
    if let Some(length) = header(headers, "content-length") {
        let length = length
            .parse::<usize>()
            .map_err(|_| error(400, "bad_request", "invalid content-length"))?;
        if length > limit {
            return Err(error(413, "too_large", "request body is too large"));
        }
    }
    to_bytes(body, limit)
        .await
        .map_err(|_| error(413, "too_large", "request body is too large or unreadable"))
}

fn attachment_response(metadata: &Value, bytes: Bytes) -> Result<Response, BoardError> {
    let media_type = metadata
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("application/octet-stream");
    let name = metadata
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("file");
    let inline = matches!(
        media_type,
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/avif"
            | "video/mp4"
            | "video/webm"
            | "text/plain"
    );
    let disposition = format!(
        "{}; filename*=UTF-8''{}",
        if inline { "inline" } else { "attachment" },
        percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC)
    );
    let length = bytes.len();
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(
        "content-type",
        HeaderValue::from_str(media_type)
            .map_err(|_| error(500, "invalid_attachment", "attachment metadata is invalid"))?,
    );
    headers.insert(
        "content-length",
        HeaderValue::from_str(&length.to_string())
            .map_err(|_| error(500, "invalid_attachment", "attachment size is invalid"))?,
    );
    headers.insert(
        "content-disposition",
        HeaderValue::from_str(&disposition)
            .map_err(|_| error(500, "invalid_attachment", "attachment name is invalid"))?,
    );
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "cache-control",
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    Ok(response)
}

fn guard_browser(
    config: &ServerConfig,
    headers: &HeaderMap,
    method: &Method,
) -> Result<(), BoardError> {
    // Forwarded headers are deliberately ignored: they cannot establish origin or identity.
    let host = header(headers, "host");
    if let Some(host) = host {
        let expected = config
            .public_url
            .as_deref()
            .map(url::Url::parse)
            .transpose()
            .map_err(|_| error(500, "invalid_config", "invalid public URL"))?;
        let scheme = expected.as_ref().map_or("http", url::Url::scheme);
        let parsed = url::Url::parse(&format!("{scheme}://{host}"))
            .map_err(|_| error(400, "bad_request", "invalid host header"))?;
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(error(400, "bad_request", "invalid host header"));
        }
        if matches!(config.auth, crate::AuthConfig::Local)
            && !matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
        {
            return Err(error(
                403,
                "invalid_host",
                "local board requires a loopback host",
            ));
        }
        if let Some(expected) = expected
            && (parsed.host_str() != expected.host_str() || parsed.port() != expected.port())
        {
            return Err(error(
                403,
                "invalid_host",
                "request host does not match the public URL",
            ));
        }
    }
    if !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
        && let Some(origin) = header(headers, "origin")
    {
        let origin = url::Url::parse(origin)
            .map_err(|_| error(403, "invalid_origin", "cross-origin writes are refused"))?;
        let expected = if let Some(public_url) = &config.public_url {
            url::Url::parse(public_url).ok()
        } else {
            host.and_then(|host| url::Url::parse(&format!("http://{host}")).ok())
        };
        if expected.is_none_or(|expected| origin.origin() != expected.origin()) {
            return Err(error(
                403,
                "invalid_origin",
                "cross-origin writes are refused",
            ));
        }
        if header(headers, "sec-fetch-site") == Some("cross-site") {
            return Err(error(
                403,
                "invalid_origin",
                "cross-site writes are refused",
            ));
        }
    }
    Ok(())
}

fn base_url(config: &ServerConfig, headers: &HeaderMap, _uri: &Uri) -> Result<String, BoardError> {
    if let Some(url) = &config.public_url {
        return Ok(url.trim_end_matches('/').to_owned());
    }
    let host = header(headers, "host")
        .map(str::to_owned)
        .unwrap_or_else(|| config.bind.to_string());
    Ok(format!("http://{host}"))
}

pub(crate) fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn malformed_percent(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.iter().enumerate().any(|(at, byte)| {
        *byte == b'%'
            && (at + 2 >= bytes.len()
                || !bytes[at + 1].is_ascii_hexdigit()
                || !bytes[at + 2].is_ascii_hexdigit())
    })
}

fn decode_path(path: &str) -> Result<String, BoardError> {
    path.split('/')
        .map(|segment| {
            if malformed_percent(segment) {
                return Err(error(400, "bad_request", "path is not URI-encoded"));
            }
            let segment = percent_encoding::percent_decode_str(segment)
                .decode_utf8()
                .map_err(|_| error(400, "bad_request", "path is not valid UTF-8"))?;
            if segment.contains(['/', '\\', '\0']) {
                return Err(error(
                    400,
                    "bad_request",
                    "path segment contains an invalid separator",
                ));
            }
            Ok(segment.into_owned())
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|segments| segments.join("/"))
}
