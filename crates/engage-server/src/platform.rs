use crate::{
    error,
    http::{App, json_response},
};
use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use engage_core::{BoardError, Principal};
use serde_json::{Value, json};

pub(crate) async fn linked(
    app: &App,
    principal: &Principal,
    query: &[(String, String)],
) -> Result<Response, BoardError> {
    let address = query
        .iter()
        .find(|(key, _)| key == "url")
        .map(|(_, value)| value.as_str())
        .ok_or_else(|| error(400, "bad_request", "a platform URL is required"))?;
    let url = url::Url::parse(address)
        .map_err(|_| error(400, "bad_request", "platform URL must be absolute"))?;
    if !matches!(url.scheme(), "http" | "https") || address.len() > 4000 {
        return Err(error(
            400,
            "bad_request",
            "platform URL must be HTTP or HTTPS",
        ));
    }
    let registry = app
        .config
        .sync
        .as_ref()
        .ok_or_else(|| error(404, "message_not_found", "no message for this platform URL"))?;
    for entry in registry.discovery() {
        let Some(key) = entry["key"].as_str() else {
            continue;
        };
        let provider = registry.provider(key).map_err(sync_error)?;
        let Some((thread, message)) = provider.locate(address) else {
            continue;
        };
        let mut find = vec![("thread".into(), format!("{key}:{thread}"))];
        if let Some(message) = message {
            find.push(("message".into(), message));
        }
        let found = app
            .board
            .request(principal, "GET", "/sync/find", &find, Value::Null)
            .await?;
        let id = found["message"]
            .as_str()
            .or_else(|| found["thread"].as_str());
        if let Some(id) = id {
            let path = format!("/messages/{id}");
            match app
                .board
                .request(principal, "GET", &path, &[], Value::Null)
                .await
            {
                Ok(view) => return Ok(json_response(StatusCode::OK, view)),
                Err(cause) if cause.status == 404 => {}
                Err(cause) => return Err(cause),
            }
        }
    }
    Err(error(
        404,
        "message_not_found",
        "no message for this platform URL",
    ))
}

pub(crate) async fn typing(
    app: &App,
    principal: &Principal,
    path: &str,
    input: &Value,
) -> Result<Response, BoardError> {
    // The engine checks thread visibility and posting rights before any external request.
    app.board
        .request(principal, "POST", path, &[], input.clone())
        .await?;
    let status = match input.get("status") {
        None => None,
        Some(Value::String(status))
            if !status.trim().is_empty() && status.trim().chars().count() <= 50 =>
        {
            Some(status.trim())
        }
        _ => {
            return Err(error(
                400,
                "bad_request",
                "typing status must contain between 1 and 50 characters",
            ));
        }
    };
    let Some(registry) = &app.config.sync else {
        return Ok(json_response(StatusCode::OK, json!({"targets":[]})));
    };
    let message_path = path.trim_end_matches("/typing");
    let synced = app
        .board
        .request(
            principal,
            "GET",
            &format!("{message_path}/sync"),
            &[],
            Value::Null,
        )
        .await?;
    let mut targets = Vec::new();
    for target in synced["targets"].as_array().into_iter().flatten() {
        let Some(key) = target["key"].as_str() else {
            continue;
        };
        let Ok(provider) = registry.provider(key) else {
            continue;
        };
        if !provider.capabilities().typing {
            continue;
        }
        let Some(thread) = target["thread"].as_str() else {
            continue;
        };
        let thread = thread.strip_prefix(&format!("{key}:")).unwrap_or(thread);
        if provider.typing(thread, status).await.is_ok() {
            targets.push(target["target"].clone());
        }
    }
    Ok(json_response(StatusCode::OK, json!({"targets":targets})))
}

pub(crate) async fn source(
    app: &App,
    principal: &Principal,
    method: &str,
    path: &str,
    query: &[(String, String)],
    input: &Value,
) -> Result<Response, BoardError> {
    let me = app
        .board
        .request(principal, "GET", "/me", &[], Value::Null)
        .await?;
    let full = principal.scope.is_none()
        && (principal.owner
            || me
                .get("can")
                .and_then(|can| can.get("*"))
                .and_then(Value::as_array)
                .is_some_and(|can| {
                    ["read", "post", "react", "label", "sync", "manage"]
                        .iter()
                        .all(|name| can.iter().any(|value| value.as_str() == Some(name)))
                }));
    if !full {
        return Err(error(
            403,
            "forbidden",
            "reading platform sources requires full board access",
        ));
    }
    let registry = app
        .config
        .sync
        .as_ref()
        .ok_or_else(|| error(404, "source_not_found", "no sync provider"))?;
    let (key, action) = path
        .trim_start_matches("/sources/")
        .split_once('/')
        .ok_or_else(|| error(404, "not_found", "no source route"))?;
    let provider = registry.provider(key).map_err(sync_error)?;
    let value = match (method, action) {
        ("GET", "read") => {
            let target = query
                .iter()
                .find(|(key, _)| key == "target")
                .map(|(_, value)| value.as_str())
                .ok_or_else(|| error(400, "bad_request", "source target is required"))?;
            let limit = query
                .iter()
                .find(|(key, _)| key == "limit")
                .and_then(|(_, value)| value.parse::<usize>().ok())
                .unwrap_or(50);
            if !(1..=200).contains(&limit) {
                return Err(error(
                    400,
                    "bad_request",
                    "source limit must be between 1 and 200",
                ));
            }
            let after = query
                .iter()
                .find(|(key, _)| key == "after")
                .map(|(_, value)| value.as_str());
            serde_json::to_value(
                provider
                    .read(target, limit, after)
                    .await
                    .map_err(sync_error)?,
            )
            .map_err(|_| error(500, "source_failed", "cannot serialize source"))?
        }
        ("POST", "join") => {
            let target = input
                .get("target")
                .and_then(Value::as_str)
                .ok_or_else(|| error(400, "bad_request", "source target is required"))?;
            serde_json::to_value(provider.join(target).await.map_err(sync_error)?)
                .map_err(|_| error(500, "source_failed", "cannot serialize source"))?
        }
        _ => return Err(error(404, "not_found", "no source route")),
    };
    Ok(json_response(StatusCode::OK, value))
}

pub(crate) async fn webhook(
    app: &App,
    key: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<Response, BoardError> {
    let registry = app
        .config
        .sync
        .as_ref()
        .ok_or_else(|| error(404, "sync_not_found", "no sync provider"))?;
    let request = engage_sync::WebhookRequest {
        headers: headers
            .iter()
            .filter_map(|(key, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (key.as_str().to_owned(), value.to_owned()))
            })
            .collect(),
        body: body.to_vec(),
        now_unix: chrono::Utc::now().timestamp(),
    };
    let response = registry.webhook(key, &request).await.map_err(sync_error)?;
    if !response.incoming.is_empty() {
        let service_id = registry
            .principal(key)
            .ok_or_else(|| error(404, "sync_not_found", "no sync provider"))?;
        let principal = Principal {
            id: service_id.to_owned(),
            handle: service_id.to_owned(),
            name: service_id.to_owned(),
            ..app.config.sync_principal.clone()
        };
        for events in response.incoming.chunks(50) {
            let input = json!({"events":events.iter().map(|event|json!({"id":event.id,"event":event.event,"raw":event.raw})).collect::<Vec<_>>()});
            app.board
                .request(&principal, "POST", &format!("/inbox/{key}"), &[], input)
                .await?;
        }
    }
    Ok(json_response(
        StatusCode::from_u16(response.status).unwrap_or(StatusCode::OK),
        response.body,
    ))
}

fn sync_error(cause: engage_sync::SyncError) -> BoardError {
    let (status, code, message) = match cause {
        engage_sync::SyncError::Signature | engage_sync::SyncError::Stale => (
            401,
            "invalid_signature",
            "platform signature is invalid or expired",
        ),
        engage_sync::SyncError::Payload(_) => {
            (400, "invalid_payload", "platform payload is invalid")
        }
        engage_sync::SyncError::UnknownProvider(_) => (404, "sync_not_found", "no sync provider"),
        engage_sync::SyncError::Unsupported(_) => (
            400,
            "unsupported_operation",
            "this provider does not support the operation",
        ),
        _ => (502, "platform_unavailable", "platform operation failed"),
    };
    error(status, code, message)
}
