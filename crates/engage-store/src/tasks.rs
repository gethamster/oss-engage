mod batch;
use crate::{by, changed, db::Tx, messages, n, need, public, q, read_message, text};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Duration;
use engage_core::{BoardError, Principal};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

fn finished(task: &Value) -> bool {
    matches!(
        task["state"].as_str(),
        Some("completed" | "failed" | "canceled")
    )
}
fn hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
fn lease(body: &Value) -> Result<i64, BoardError> {
    let n = body["lease"].as_f64().unwrap_or(900.0);
    if !n.is_finite() || n < 30.0 {
        return Err(BoardError::invalid("lease must be at least 30 seconds"));
    }
    Ok((n as i64).min(3600))
}
async fn owned(tx: &mut Tx<'_>, p: &Principal, message: &Value) -> Result<(), BoardError> {
    if message["task"]["owner"] == p.id
        && (p.profile.is_none() || message["task"]["by"] == p.actor())
        || crate::access::allowed(tx, p, text(message, "channel")?, "manage").await?
    {
        Ok(())
    } else {
        Err(BoardError::new(
            403,
            "task_not_owned",
            "task belongs to another actor",
        ))
    }
}
async fn get(tx: &mut Tx<'_>, p: &Principal, id: &str) -> Result<Value, BoardError> {
    let message = read_message(tx, p, id).await?;
    if !message["task"].is_object() || message["re"].is_string() {
        return Err(BoardError::missing("not_a_task"));
    }
    Ok(message)
}
async fn emit(
    tx: &mut Tx<'_>,
    p: &Principal,
    message: &mut Value,
    before: &Value,
    kind: &str,
    extra: Value,
) -> Result<(), BoardError> {
    if message["deleted"] != true {
        message["labels"]["board.task"] = message["task"]["state"].clone();
    }
    let mut payload = json!({"id":message["id"],"by":by(p)});
    if let Some(map) = extra.as_object() {
        for (k, v) in map {
            if matches!(
                k.as_str(),
                "for" | "needs" | "epoch" | "until" | "kind" | "reason" | "result"
            ) {
                payload[k] = v.clone();
            }
        }
    }
    changed(tx, p, message, Some(before), kind, payload).await
}
async fn validate_needs(
    tx: &mut Tx<'_>,
    p: &Principal,
    id: &str,
    needs: &Value,
) -> Result<Vec<String>, BoardError> {
    if !needs.is_array() {
        return Err(BoardError::new(
            400,
            "invalid_needs",
            "needs must be a list",
        ));
    }
    if needs.as_array().is_some_and(|needs| needs.len() > 20) {
        return Err(BoardError::new(
            400,
            "invalid_needs",
            "at most 20 prerequisites",
        ));
    }
    let mut unique = BTreeSet::new();
    for needed in needs.as_array().expect("checked array") {
        let needed = needed
            .as_str()
            .ok_or_else(|| BoardError::new(400, "invalid_needs", "needs names task IDs"))?;
        let mut stack = vec![needed.to_string()];
        let mut visited = BTreeSet::new();
        while let Some(next) = stack.pop() {
            if next == id {
                return Err(BoardError::new(
                    400,
                    "invalid_needs",
                    "task dependencies form a cycle",
                ));
            }
            if !visited.insert(next.clone()) {
                continue;
            }
            let other = get(tx, p, &next).await.map_err(|_| {
                BoardError::new(400, "invalid_needs", "prerequisite is not a readable task")
            })?;
            if other["deleted"] == true || other["task"]["state"] == "canceled" {
                return Err(BoardError::new(
                    400,
                    "invalid_needs",
                    "prerequisite was canceled",
                ));
            }
            for child in other["task"]["needs"].as_array().into_iter().flatten() {
                if let Some(s) = child.as_str() {
                    stack.push(s.to_string());
                }
            }
        }
        unique.insert(needed.to_string());
    }
    Ok(unique.into_iter().collect())
}
async fn ready(tx: &mut Tx<'_>, message: &Value) -> Result<bool, BoardError> {
    for id in message["task"]["needs"].as_array().into_iter().flatten() {
        let Some(other) = tx.get("message", id.as_str().unwrap_or("")).await? else {
            return Ok(false);
        };
        if other["task"]["state"] != "completed" {
            return Ok(false);
        }
    }
    Ok(true)
}
async fn make_ready(tx: &mut Tx<'_>, p: &Principal, message: &mut Value) -> Result<(), BoardError> {
    if message["task"]["state"] == "waiting" && ready(tx, message).await? {
        let before = message.clone();
        message["task"]["state"] = "ready".into();
        emit(tx, p, message, &before, "task.ready", json!({})).await?;
    }
    Ok(())
}
pub(crate) async fn settle(tx: &mut Tx<'_>) -> Result<(), BoardError> {
    let p = Principal::default();
    let mut pending = tx.list("task-state:claimed").await?;
    pending.extend(tx.list("task-state:waiting").await?);
    for mut message in pending {
        if message["task"]["state"] == "claimed"
            && message["task"]["claim"]["until"]
                .as_str()
                .is_some_and(|until| {
                    until
                        <= tx
                            .at
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                            .as_str()
                })
        {
            let before = message.clone();
            message["task"]["state"] = "ready".into();
            message["task"]["claim"] = Value::Null;
            message
                .as_object_mut()
                .expect("message object")
                .remove("_claim_token");
            emit(tx, &p, &mut message, &before, "task.expired", json!({})).await?;
        }
        make_ready(tx, &p, &mut message).await?;
    }
    Ok(())
}
pub(crate) async fn edited(
    tx: &mut Tx<'_>,
    p: &Principal,
    message: &mut Value,
) -> Result<(), BoardError> {
    if finished(&message["task"]) {
        return Ok(());
    }
    let before = message.clone();
    message["task"]["state"] = "blocked".into();
    message["task"]["claim"] = Value::Null;
    message["task"]["blocked"] =
        json!({"kind":"edited","reason":"task root edited by another actor"});
    message
        .as_object_mut()
        .expect("message object")
        .remove("_claim_token");
    emit(
        tx,
        p,
        message,
        &before,
        "task.blocked",
        json!({"kind":"edited","reason":"task root edited by another actor"}),
    )
    .await
}

async fn create(tx: &mut Tx<'_>, p: &Principal, body: &Value) -> Result<Value, BoardError> {
    if body["lease"].is_number() && body["claim"] != true {
        return Err(BoardError::invalid("lease goes with claim"));
    }
    if body["claim"] == true {
        lease(body)?;
    }
    if let Some(client_id) = body["id"].as_str()
        && let Some(record) = tx
            .get("post", &format!("{}:{client_id}", p.actor()))
            .await?
    {
        let old = read_message(tx, p, text(&record, "message")?).await?;
        if !old["task"].is_object() || old["re"].is_string() {
            return Err(BoardError::conflict(
                "duplicate_id",
                "ID names another write",
            ));
        }
        return Ok(json!({"task":public(old)}));
    }
    let mut message = if let Some(re) = body["re"].as_str() {
        if ["text", "labels", "noSync", "attachments", "id"]
            .iter()
            .any(|k| body.get(k).is_some())
        {
            return Err(BoardError::invalid(
                "adoption uses the existing thread unchanged",
            ));
        }
        let parent = read_message(tx, p, re).await?;
        if parent["re"].is_string() {
            return Err(BoardError::new(
                400,
                "not_a_root",
                "a task is a thread root",
            ));
        }
        parent
    } else {
        messages::post(tx, p, body).await?
    };
    if message["task"].is_object() {
        return Err(BoardError::conflict(
            "already_a_task",
            "thread is already a task",
        ));
    }
    need(tx, p, text(&message, "channel")?, "label").await?;
    let id = text(&message, "id")?.to_string();
    let needs =
        validate_needs(tx, p, &id, &body.get("needs").cloned().unwrap_or(json!([]))).await?;
    let assignee = body.get("for").cloned().unwrap_or(Value::Null);
    validate_assignment(p, &assignee)?;
    let before = message.clone();
    message["task"] = json!({"state":if needs.is_empty(){"ready"}else{"waiting"},"owner":p.id,"by":p.actor(),"for":assignee,"needs":needs,"epoch":0,"claim":null,"blocked":null,"result":null,"reason":null});
    emit(
        tx,
        p,
        &mut message,
        &before,
        "task.created",
        json!({"needs":needs,"for":assignee}),
    )
    .await?;
    make_ready(tx, p, &mut message).await?;
    if body["claim"] == true {
        return claim(tx, p, message, body).await;
    }
    Ok(json!({"task":public(message)}))
}
fn validate_assignment(p: &Principal, value: &Value) -> Result<(), BoardError> {
    if let Some(actor) = value.as_str() {
        if actor != p.id && !actor.starts_with(&format!("{}+", p.id)) {
            return Err(BoardError::invalid(
                "assignee must be an actor of its owner",
            ));
        }
    } else if !value.is_null() {
        return Err(BoardError::invalid("for is an actor ID or null"));
    }
    Ok(())
}

async fn claim(
    tx: &mut Tx<'_>,
    p: &Principal,
    mut message: Value,
    body: &Value,
) -> Result<Value, BoardError> {
    let seconds = lease(body)?;
    need(tx, p, text(&message, "channel")?, "label").await?;
    let state = message["task"]["state"].as_str().unwrap_or("");
    if state == "claimed" {
        if body["token"]
            .as_str()
            .is_some_and(|t| message["_claim_token"] == hash(t))
            && message["task"]["claim"]["actor"] == p.actor()
        {
            return Ok(
                json!({"task":public(message.clone()),"claim":{"epoch":message["task"]["epoch"],"token":body["token"],"until":message["task"]["claim"]["until"]}}),
            );
        }
        return Err(BoardError::conflict(
            "task_claimed",
            "task already has an active claim",
        ));
    }
    if finished(&message["task"]) {
        return Err(BoardError::conflict("task_finished", "task is finished"));
    }
    if state == "blocked" {
        return Err(BoardError::conflict("task_blocked", "task is blocked"));
    }
    if state == "waiting" {
        return Err(BoardError::conflict(
            "task_waiting",
            "task has incomplete prerequisites",
        ));
    }
    if message["task"]["for"]
        .as_str()
        .is_some_and(|a| a != p.actor())
    {
        return Err(BoardError::conflict(
            "task_assigned",
            "task is assigned to another actor",
        ));
    }
    let token = body["token"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| {
            let mut bytes = Vec::from(uuid::Uuid::new_v4().as_bytes());
            bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
            URL_SAFE_NO_PAD.encode(bytes)
        });
    if token.len() != 43
        || !token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(BoardError::invalid(
            "claim token must be 32 base64url bytes",
        ));
    }
    let until =
        (tx.at + Duration::seconds(seconds)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let before = message.clone();
    let epoch = message["task"]["epoch"].as_i64().unwrap_or(0) + 1;
    message["task"]["state"] = "claimed".into();
    message["task"]["epoch"] = epoch.into();
    message["task"]["claim"] = json!({"actor":p.actor(),"until":until});
    message["_claim_token"] = hash(&token).into();
    emit(
        tx,
        p,
        &mut message,
        &before,
        "task.claimed",
        json!({"epoch":epoch,"until":until}),
    )
    .await?;
    Ok(json!({"task":public(message),"claim":{"epoch":epoch,"token":token,"until":until}}))
}
fn held(tx: &Tx<'_>, p: &Principal, message: &Value, body: &Value) -> Result<(), BoardError> {
    if message["task"]["state"] != "claimed"
        || message["task"]["epoch"] != body["epoch"]
        || message["task"]["claim"]["actor"] != p.actor()
        || body["token"]
            .as_str()
            .is_none_or(|token| message["_claim_token"] != hash(token))
        || message["task"]["claim"]["until"]
            .as_str()
            .is_none_or(|until| {
                until
                    <= tx
                        .at
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                        .as_str()
            })
    {
        return Err(BoardError::conflict(
            "claim_lost",
            "claim no longer owns this task",
        ));
    }
    Ok(())
}

pub(crate) async fn route(
    tx: &mut Tx<'_>,
    p: &Principal,
    method: &str,
    path: &str,
    body: &Value,
    query: &[(String, String)],
) -> Result<Option<Value>, BoardError> {
    if !path.starts_with("/tasks") {
        return Ok(None);
    }
    let result = match (method, path) {
        ("POST", "/tasks") => create(tx, p, body).await?,
        ("POST", "/tasks/batch") => batch::create(tx, p, body).await?,
        ("GET", "/tasks") => list(tx, p, query).await?,
        ("POST", "/tasks/claim") => {
            let channel = text(body, "channel")?;
            need(tx, p, channel.trim_end_matches('/'), "label").await?;
            let mut found = None;
            let mut all = tx.list("task-state:ready").await?;
            all.sort_by_key(|m| {
                (
                    m["task"]["for"] != p.actor(),
                    m["id"]
                        .as_str()
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or(0),
                )
            });
            for message in all {
                if message["task"]["state"] != "ready"
                    || !messages::channel_matches(
                        channel,
                        message["channel"].as_str().unwrap_or(""),
                    )
                {
                    continue;
                }
                if !body["label"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .all(|(key, value)| &message["labels"][key] == value)
                {
                    continue;
                }
                if message["task"]["for"]
                    .as_str()
                    .is_some_and(|id| id != p.actor())
                {
                    continue;
                }
                if message["task"]["owner"] != p.id
                    && !body["from"]
                        .as_array()
                        .is_some_and(|owners| owners.contains(&message["task"]["owner"]))
                {
                    continue;
                }
                if crate::access::allowed(tx, p, text(&message, "channel")?, "label").await? {
                    found = Some(message);
                    break;
                }
            }
            match found {
                Some(message) => claim(tx, p, message, body).await?,
                None => json!({"task":null}),
            }
        }
        _ if method == "GET" && path.starts_with("/tasks/batch/") => {
            let id = &path[13..];
            let record = tx
                .get("batch", &format!("{}:{id}", p.actor()))
                .await?
                .ok_or_else(|| BoardError::missing("batch_not_found"))?;
            if q(query, "request").is_some_and(|request| record["request"] != request) {
                return Err(BoardError::conflict(
                    "duplicate_id",
                    "batch request differs",
                ));
            }
            let mut tasks = vec![];
            for id in record["tasks"].as_array().into_iter().flatten() {
                tasks.push(public(get(tx, p, id.as_str().unwrap_or("")).await?));
            }
            json!({"tasks":tasks})
        }
        _ if method == "POST" && path.starts_with("/tasks/") => {
            let Some((id, action)) = path[7..].split_once('/') else {
                return Ok(None);
            };
            let mut message = get(tx, p, id).await?;
            if action == "claim" {
                return Ok(Some(claim(tx, p, message, body).await?));
            }
            act(tx, p, &mut message, action, body).await?
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}

async fn act(
    tx: &mut Tx<'_>,
    p: &Principal,
    message: &mut Value,
    action: &str,
    body: &Value,
) -> Result<Value, BoardError> {
    need(tx, p, text(message, "channel")?, "label").await?;
    let before = message.clone();
    let task = &message["task"];
    if matches!(action, "renew" | "release" | "complete" | "fail") {
        held(tx, p, message, body)?;
    } else if action == "block" && task["state"] == "claimed" {
        if body["epoch"].is_null() {
            return Err(BoardError::conflict(
                "task_claimed",
                "holding a claimed task requires its claim",
            ));
        }
        held(tx, p, message, body)?;
    } else {
        owned(tx, p, message).await?;
    }
    if matches!(action, "needs" | "assign" | "cancel" | "block") && finished(task) {
        return Err(BoardError::conflict("task_finished", "task is finished"));
    }
    if matches!(action, "needs" | "assign") && task["state"] == "claimed" {
        return Err(BoardError::conflict(
            "task_claimed",
            "task has an active claim",
        ));
    }
    let kind = match action {
        "renew" => {
            let until = (tx.at + Duration::seconds(lease(body)?))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            message["task"]["claim"]["until"] = until.clone().into();
            emit(
                tx,
                p,
                message,
                &before,
                "task.renewed",
                json!({"epoch":message["task"]["epoch"],"until":until}),
            )
            .await?;
            return Ok(
                json!({"task":public(message.clone()),"until":until,"claim":{"epoch":message["task"]["epoch"],"token":body["token"],"until":until}}),
            );
        }
        "release" => {
            message["task"]["state"] = "ready".into();
            "task.released"
        }
        "complete" => {
            message["task"]["state"] = "completed".into();
            message["task"]["result"] = body.get("result").cloned().unwrap_or(Value::Null);
            "task.completed"
        }
        "fail" => {
            text(body, "reason")?;
            message["task"]["state"] = "failed".into();
            message["task"]["reason"] = body["reason"].clone();
            "task.failed"
        }
        "block" => {
            if body["kind"] == "edited" {
                return Err(BoardError::invalid("edited is a reserved hold kind"));
            }
            text(body, "reason")?;
            message["task"]["state"] = "blocked".into();
            message["task"]["blocked"] = json!({"kind":body.get("kind").cloned().unwrap_or(Value::Null),"reason":body["reason"]});
            "task.blocked"
        }
        "unblock" => {
            if message["task"]["state"] != "blocked" {
                return Err(BoardError::conflict("task_not_blocked", "task has no hold"));
            }
            message["task"]["state"] = if message["task"]["needs"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
            {
                json!("waiting")
            } else {
                json!("ready")
            };
            message["task"]["blocked"] = Value::Null;
            "task.unblocked"
        }
        "reopen" => {
            if !matches!(
                message["task"]["state"].as_str(),
                Some("failed" | "completed")
            ) {
                return Err(BoardError::conflict(
                    "task_not_failed",
                    "task cannot be reopened",
                ));
            }
            message["task"]["state"] = if message["task"]["needs"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
            {
                json!("waiting")
            } else {
                json!("ready")
            };
            message["task"]["result"] = Value::Null;
            message["task"]["reason"] = Value::Null;
            "task.reopened"
        }
        "cancel" => {
            text(body, "reason")?;
            message["task"]["state"] = "canceled".into();
            message["task"]["reason"] = body["reason"].clone();
            "task.canceled"
        }
        "assign" => {
            let for_actor = body
                .get("for")
                .ok_or_else(|| BoardError::invalid("for is required"))?;
            let mut owner = p.clone();
            owner.id = text(&message["task"], "owner")?.into();
            validate_assignment(&owner, for_actor)?;
            message["task"]["for"] = for_actor.clone();
            "task.assigned"
        }
        "needs" => {
            let needs = validate_needs(tx, p, text(message, "id")?, &body["needs"]).await?;
            if message["task"]["needs"] == json!(needs) {
                return Ok(json!({"task":public(message.clone())}));
            }
            message["task"]["needs"] = json!(needs);
            if message["task"]["state"] != "blocked" {
                message["task"]["state"] = if needs.is_empty() {
                    json!("ready")
                } else {
                    json!("waiting")
                };
            }
            "task.needs_changed"
        }
        _ => return Err(BoardError::missing("route_not_found")),
    };
    if action != "assign" && action != "needs" {
        message["task"]["claim"] = Value::Null;
        message
            .as_object_mut()
            .expect("message object")
            .remove("_claim_token");
    }
    emit(tx, p, message, &before, kind, body.clone()).await?;
    make_ready(tx, p, message).await?;
    if action == "complete" {
        settle(tx).await?;
    }
    Ok(json!({"task":public(message.clone())}))
}

async fn list(
    tx: &mut Tx<'_>,
    p: &Principal,
    query: &[(String, String)],
) -> Result<Value, BoardError> {
    let latest = q(query, "latest").is_some_and(|v| v == "true" || v == "1");
    if latest && q(query, "after").is_some() {
        return Err(BoardError::invalid("latest takes no after"));
    }
    let mut all = vec![];
    for message in tx.list("task").await? {
        if !message["task"].is_object()
            || message["deleted"] == true
            || !messages::matches(&message, query)
            || !crate::access::allowed(tx, p, text(&message, "channel")?, "read").await?
        {
            continue;
        }
        let task = &message["task"];
        let state = q(query, "state").unwrap_or("open");
        if state == "open" && finished(task)
            || state != "open" && state != "all" && task["state"] != state
        {
            continue;
        }
        if q(query, "needs").is_some_and(|id| {
            !task["needs"]
                .as_array()
                .is_some_and(|needs| needs.contains(&json!(id)))
        }) || q(query, "owner").is_some_and(|id| task["owner"] != id)
            || q(query, "for").is_some_and(|id| task["for"] != id)
        {
            continue;
        }
        let id = text(&message, "id")?.parse::<i64>().unwrap_or(0);
        if q(query, "after")
            .and_then(|s| s.parse::<i64>().ok())
            .is_some_and(|after| id <= after)
        {
            continue;
        }
        let summary = messages::summary(tx, message).await?;
        let position = if latest {
            summary["last"]["id"]
                .as_str()
                .and_then(|id| id.parse::<i64>().ok())
                .unwrap_or(id)
        } else {
            id
        };
        all.push((position, summary));
    }
    all.sort_by_key(|(position, _)| *position);
    let limit = n(query, "limit", 100);
    let more = all.len() > limit;
    if latest && more {
        all.drain(..all.len() - limit);
    } else {
        all.truncate(limit);
    }
    let cursor = if more && !latest {
        all.last()
            .map(|(_, s)| s["message"]["id"].clone())
            .unwrap_or(json!(tx.position.to_string()))
    } else {
        json!(tx.position.to_string())
    };
    Ok(json!({"tasks":all.into_iter().map(|(_,s)|s).collect::<Vec<_>>(),"cursor":cursor}))
}
