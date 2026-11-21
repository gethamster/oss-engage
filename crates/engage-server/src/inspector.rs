use std::path::Component;

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use engage_core::BoardError;

use crate::{error, http::App};

pub(crate) async fn serve(app: &App, path: &str) -> Result<Response, BoardError> {
    if path == "/inspect" {
        return Ok((StatusCode::TEMPORARY_REDIRECT, [("location", "/inspect/")]).into_response());
    }
    let directory = app.config.inspector_dir.as_ref().ok_or_else(|| {
        error(
            404,
            "inspector_not_built",
            "inspector assets are not configured",
        )
    })?;
    let suffix = path.strip_prefix("/inspect/").unwrap_or("");
    let decoded = percent_encoding::percent_decode_str(suffix)
        .decode_utf8()
        .map_err(|_| error(400, "bad_request", "invalid asset path"))?;
    if !decoded.is_empty()
        && (decoded.contains('\\')
            || std::path::Path::new(decoded.as_ref())
                .components()
                .any(|component| !matches!(component, Component::Normal(_))))
    {
        return Err(error(404, "not_found", "no asset"));
    }
    let mut file = directory.join(decoded.as_ref());
    if decoded.is_empty() || !decoded.rsplit('/').next().unwrap_or("").contains('.') {
        file = directory.join("index.html");
    }
    let bytes = tokio::fs::read(&file).await.map_err(|cause| {
        if cause.kind() == std::io::ErrorKind::NotFound {
            error(404,"inspector_not_built","build the inspector with: cd crates/engage-inspector && trunk build --release --public-url /inspect/")
        } else {
            tracing::error!(%cause,"cannot read inspector asset");
            error(503,"inspector_unavailable","inspector assets are unavailable")
        }
    })?;
    let extension = file.extension().and_then(|s| s.to_str()).unwrap_or("");
    let media_type = match extension {
        "html" => "text/html; charset=utf-8",
        "wasm" => "application/wasm",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "json" => "application/json",
        _ => "application/octet-stream",
    };
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let bytes = if extension == "html" {
        String::from_utf8(bytes)
            .map_err(|_| error(500, "invalid_assets", "inspector HTML is not valid UTF-8"))?
            .replace("<script", &format!("<script nonce=\"{nonce}\""))
            .into_bytes()
    } else {
        bytes
    };
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static(media_type));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static(if extension == "html" {
            "no-cache"
        } else {
            "public, max-age=31536000, immutable"
        }),
    );
    let policy = format!(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' 'nonce-{nonce}'; style-src 'self' 'unsafe-inline'; img-src 'self' https: data: blob:; connect-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self'"
    );
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_str(&policy)
            .map_err(|_| error(500, "invalid_assets", "cannot construct inspector policy"))?,
    );
    Ok(response)
}
