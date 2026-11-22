use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use engage_core::{BoardError, Principal};
use serde_json::Value;

use crate::error;
use crate::http::App;

/// Polls durable storage without holding a transaction between attempts.
pub(crate) async fn poll(
    app: &App,
    principal: &Principal,
    method: &str,
    path: &str,
    query: &[(String, String)],
    body: &Value,
    expires_at: Option<u64>,
) -> Result<Value, BoardError> {
    let is_events = method == "GET" && path == "/events";
    let is_matches = method == "GET" && path == "/watchers/matches";
    let is_claim = method == "POST" && path == "/tasks/claim";
    let seconds = if is_claim {
        body.get("wait")
            .map(|value| {
                value
                    .as_f64()
                    .ok_or_else(|| error(400, "bad_request", "wait must be a number"))
            })
            .transpose()?
            .unwrap_or(0.0)
    } else {
        query
            .iter()
            .find(|(name, _)| name == "wait")
            .map(|(_, value)| {
                value
                    .parse::<f64>()
                    .map_err(|_| error(400, "bad_request", "wait must be a number"))
            })
            .transpose()?
            .unwrap_or(0.0)
    };
    let maximum = if is_matches { 25.0 } else { 300.0 };
    if !seconds.is_finite() || !(0.0..=maximum).contains(&seconds) {
        return Err(error(
            400,
            "bad_request",
            "wait is outside its allowed range",
        ));
    }
    let should_wait = (is_events || is_matches || is_claim) && seconds > 0.0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(seconds);
    // Keep transport fields visible to the shared boundary validator. Removing
    // `wait` here would bypass duplicate-field and `last` interaction checks.
    let mut query = query.to_vec();
    loop {
        if expires_at.is_some_and(|expiry| expiry <= chrono::Utc::now().timestamp().max(0) as u64) {
            return Err(error(401, "unauthorized", "board credential has expired"));
        }
        let page = app
            .board
            .request(principal, method, path, &query, body.clone())
            .await?;
        let empty = if is_claim {
            page.get("task").is_none_or(Value::is_null)
        } else if is_events {
            page.get("events")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
        } else if is_matches {
            page.get("matches")
                .and_then(Value::as_array)
                .or_else(|| page.as_array())
                .is_none_or(Vec::is_empty)
        } else {
            false
        };
        if !should_wait || !empty || tokio::time::Instant::now() >= deadline {
            return Ok(page);
        }
        if is_events {
            if let Some(cursor) = page.get("cursor").and_then(Value::as_str) {
                query.retain(|(key, _)| key != "after");
                query.push(("after".into(), cursor.into()));
            }
            if query.iter().any(|(key, _)| key == "last") {
                return Ok(page);
            }
        }
        tokio::time::sleep_until(
            (tokio::time::Instant::now() + Duration::from_millis(100)).min(deadline),
        )
        .await;
    }
}

pub(crate) async fn stream(
    app: Arc<App>,
    headers: HeaderMap,
    mut query: Vec<(String, String)>,
    peer: Option<std::net::SocketAddr>,
) -> Result<Response, BoardError> {
    let identity = app.auth.identify(&headers, peer).await?;
    let permit = app.limits.stream(&identity.principal.id)?;
    if !query.iter().any(|(key, _)| key == "after")
        && let Some(cursor) = headers
            .get("last-event-id")
            .and_then(|header| header.to_str().ok())
    {
        if cursor.is_empty() || cursor.len() > 128 {
            return Err(error(400, "bad_request", "invalid event cursor"));
        }
        query.push(("after".into(), cursor.to_owned()));
    }
    // Validate the first read before committing the HTTP response to a stream.
    let first_query = query.clone();
    let first = app
        .board
        .request(
            &identity.principal,
            "GET",
            "/events",
            &first_query,
            Value::Null,
        )
        .await?;
    query.retain(|(key, _)| key != "wait" && key != "last" && key != "after");
    let events = async_stream::stream! {
        let _permit=permit;
        yield Ok::<Event, Infallible>(Event::default().comment("connected"));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
        let mut page = first;
        let mut after: Option<String> = None;
        loop {
            if app.drain.is_cancelled() {break;}
            let mut last_sent: Option<String> = None;
            if let Some(events) = page.get("events").and_then(Value::as_array) {
                for event in events {
                    let cursor = event.get("cursor").and_then(Value::as_str).unwrap_or_default();
                    if cursor.contains(['\r','\n']) { break; }
                    let data = Event::default().id(cursor).event("board").json_data(event);
                    if let Ok(data) = data { yield Ok(data); }
                    last_sent = Some(cursor.to_owned());
                }
            }
            if let Some(cursor) = page.get("cursor").and_then(Value::as_str) {
                if cursor.contains(['\r','\n']) {break;}
                if Some(cursor) != last_sent.as_deref().or(after.as_deref()) {
                    yield Ok(Event::default().id(cursor));
                }
                after = Some(cursor.to_owned());
            }
            if tokio::time::Instant::now() >= deadline {break;}
            let identified = tokio::select! {
                biased;
                () = app.drain.cancelled() => break,
                identity = app.auth.identify(&headers,peer) => identity,
            };
            let identity = match identified {
                Ok(identity) => identity,
                Err(_) => break,
            };
            let mut next_query = query.clone();
            if let Some(cursor) = &after {next_query.push(("after".into(),cursor.clone()));}
            next_query.push(("wait".into(),"15".into()));
            let next = tokio::select! {
                biased;
                () = app.drain.cancelled() => break,
                next = poll(&app,&identity.principal,"GET","/events",&next_query,&Value::Null,identity.expires_at) => next,
            };
            match next {
                Ok(next) => page = next,
                Err(cause) => {
                    tracing::warn!(code = cause.code, "event stream ended");
                    break;
                }
            }
        }
    };
    Ok(Sse::new(events)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("ping"),
        )
        .into_response())
}
