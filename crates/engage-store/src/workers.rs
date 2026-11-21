mod sync;
mod tail;
use crate::{db::Tx, messages, n, need, now, q, read_message, text};
use chrono::Duration;
use engage_core::{BoardError, Principal};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

async fn full(tx: &mut Tx<'_>, p: &Principal) -> Result<(), BoardError> {
    let narrowed = Principal {
        owner: true,
        ..p.clone()
    };
    for permission in ["read", "post", "react", "label", "sync", "manage"] {
        if !crate::access::allowed(tx, &narrowed, "*", permission).await? {
            return Err(BoardError::new(
                403,
                "forbidden",
                "operation requires an unnarrowed credential",
            ));
        }
    }
    Ok(())
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn registration_public(mut record: Value) -> Value {
    record
        .as_object_mut()
        .expect("registration object")
        .remove("_principal");
    record
}

pub(crate) async fn match_event(
    tx: &mut Tx<'_>,
    event: &Value,
    writer: &Principal,
    before: Option<&Value>,
) -> Result<(), BoardError> {
    let message = &event["message"];
    if !message.is_object() {
        return Ok(());
    }
    for registration in tx.list("registration").await? {
        let owner = text(&registration, "owner")?;
        let principal: Principal = serde_json::from_value(registration["_principal"].clone())
            .map_err(|_| BoardError::new(500, "storage_error", "invalid watcher identity"))?;
        if !crate::access::allowed(tx, &principal, text(message, "channel")?, "read").await? {
            continue;
        }
        let mut watchers: Vec<_> = registration["watch"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(name, watch)| (name.to_string(), watch.clone()))
            .collect();
        if registration["sync"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
            && crate::access::allowed(tx, &principal, text(message, "channel")?, "sync").await?
        {
            watchers.push(("@sync".into(), json!({"tail":"*"})));
        }
        for (watcher, watch) in watchers {
            if writer.id == owner
                && writer.profile.as_deref() == Some(&watcher)
                && !message["schedule"].is_object()
            {
                continue;
            }
            let filters = tail::parse(watch["tail"].as_str().unwrap_or("*"))?;
            if !messages::matches(message, &filters)
                && !before.is_some_and(|m| messages::matches(m, &filters))
            {
                continue;
            }
            let id = digest(&format!("{}:{owner}:{watcher}", event["cursor"]));
            let record = json!({"id":id,"owner":owner,"watcher":watcher,"thread":message["thread"],"event":event,"attempt":0,"lease":null,"until":null,"hash":registration["hash"],"error":null,"position":tx.position});
            tx.put("match", &id, &record).await?;
        }
    }
    Ok(())
}

async fn pull(
    tx: &mut Tx<'_>,
    kind: &str,
    p: &Principal,
    key: Option<&str>,
    limit: usize,
) -> Result<Vec<Value>, BoardError> {
    let mut result = vec![];
    let mut threads = BTreeSet::new();
    for mut item in tx.list(kind).await? {
        if item["owner"] != p.id
            || item["done"] == true
            || key.is_some_and(|key| item["key"] != key)
        {
            continue;
        }
        if kind == "match" {
            let channel = item["event"]["message"]["channel"].as_str().unwrap_or("");
            let permission = if item["watcher"] == "@sync" {
                "sync"
            } else {
                "read"
            };
            if !crate::access::allowed(tx, p, channel, permission).await? {
                continue;
            }
        }
        let thread = format!("{}:{}", item["watcher"], item["thread"]);
        if !threads.insert(thread) {
            continue;
        }
        if item["until"].as_str().is_some_and(|until| {
            until
                > tx.at
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .as_str()
        }) {
            continue;
        }
        if item["retryAt"].as_str().is_some_and(|retry| {
            retry
                > tx.at
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .as_str()
        }) {
            continue;
        }
        if item["attempt"].as_u64().unwrap_or(0) >= 10 {
            continue;
        }
        let id = text(&item, "id")?.to_string();
        item["lease"] = uuid::Uuid::new_v4().simple().to_string().into();
        item["until"] = (tx.at + Duration::seconds(60))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            .into();
        tx.put(kind, &id, &item).await?;
        let mut visible = item;
        let map = visible.as_object_mut().expect("queue item object");
        for field in [
            "owner",
            "until",
            "hash",
            "error",
            "retryAt",
            "done",
            "platformId",
            "position",
        ] {
            map.remove(field);
        }
        result.push(visible);
        if result.len() >= limit {
            break;
        }
    }
    Ok(result)
}
async fn ack(
    tx: &mut Tx<'_>,
    kind: &str,
    p: &Principal,
    id: &str,
    body: &Value,
) -> Result<Value, BoardError> {
    let code = if kind == "match" {
        "no_match"
    } else {
        "no_inbox_event"
    };
    let mut item = tx
        .get(kind, id)
        .await?
        .filter(|item| item["owner"] == p.id && item["done"] != true)
        .ok_or_else(|| BoardError::missing(code))?;
    if kind == "match" {
        let permission = if item["watcher"] == "@sync" {
            "sync"
        } else {
            "read"
        };
        if !crate::access::allowed(
            tx,
            p,
            item["event"]["message"]["channel"].as_str().unwrap_or(""),
            permission,
        )
        .await?
        {
            return Err(BoardError::missing(code));
        }
    }
    if item["lease"] != body["lease"]
        || body["lease"].is_null()
        || item["until"].as_str().is_none_or(|until| {
            until
                <= tx
                    .at
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .as_str()
        })
    {
        return Err(BoardError::conflict("lease_lost", "delivery lease expired"));
    }
    if let Some(error) = body["error"].as_str() {
        let attempt = item["attempt"].as_i64().unwrap_or(0) + 1;
        item["attempt"] = attempt.into();
        item["error"] = error.into();
        item["lease"] = Value::Null;
        item["until"] = Value::Null;
        item["retryAt"] = (tx.at + Duration::seconds(1i64 << attempt.min(9)))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            .into();
        tx.put(kind, id, &item).await?;
    } else {
        item["done"] = true.into();
        item["lease"] = Value::Null;
        item["until"] = Value::Null;
        tx.put(kind, id, &item).await?;
    }
    Ok(json!({"ok":true}))
}

pub(crate) async fn route(
    tx: &mut Tx<'_>,
    p: &Principal,
    method: &str,
    path: &str,
    body: &Value,
    query: &[(String, String)],
) -> Result<Option<Value>, BoardError> {
    if path.starts_with("/sync/") {
        let summary = crate::access::summary(tx, p).await?;
        if !summary.as_object().into_iter().flatten().any(|(_, can)| {
            can.as_array()
                .is_some_and(|can| can.contains(&json!("sync")))
        }) {
            return Err(BoardError::new(403, "forbidden", "sync access is required"));
        }
    }
    let result = match (method, path) {
        ("GET", "/watchers") => {
            let all = crate::access::allowed(tx, p, "*", "manage").await?;
            json!({"watchers":tx.list("registration").await?.into_iter().filter(|r|all||r["owner"]==p.id).map(registration_public).collect::<Vec<_>>()})
        }
        ("POST", "/watchers") => {
            full(tx, p).await?;
            if body["path"].is_string() {
                return Err(BoardError::new(
                    501,
                    "no_wake_by_path",
                    "deployment path wakeups require a platform adapter",
                ));
            }
            let watch = body["watch"]
                .as_object()
                .ok_or_else(|| BoardError::invalid("watch is required"))?;
            if watch.len() > 64 {
                return Err(BoardError::invalid("at most 64 watchers"));
            }
            for (id, value) in watch {
                tail::parse(value["tail"].as_str().unwrap_or(""))?;
                if id.is_empty()
                    || id.len() > 100
                    || id.starts_with('@')
                    || value["tail"].as_str().is_none_or(|tail| tail.len() > 1000)
                {
                    return Err(BoardError::invalid("invalid watcher"));
                }
            }
            if let Some(url) = body["url"].as_str() {
                let parsed =
                    url::Url::parse(url).map_err(|_| BoardError::invalid("invalid wake URL"))?;
                let local = parsed.host_str().is_some_and(|host| {
                    host == "localhost"
                        || host
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback())
                });
                let local_allowed =
                    parsed.scheme() == "http" && local && p.facts["allow_local_wake"] == true;
                if (parsed.scheme() != "https" && !local_allowed)
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                    || parsed.query().is_some()
                    || parsed.fragment().is_some()
                {
                    return Err(BoardError::invalid("wake URL must use https"));
                }
            }
            let registration = json!({"owner":p.id,"url":body.get("url").cloned().unwrap_or(Value::Null),"watch":body["watch"],"sync":body.get("sync").cloned().unwrap_or(json!([])),"hash":digest(&body.to_string()),"_principal":p});
            tx.put("registration", &p.id, &registration).await?;
            // Replacement invalidates queued work from the old registration.
            for mut item in tx.list("match").await? {
                if item["owner"] == p.id && item["hash"] != registration["hash"] {
                    item["done"] = true.into();
                    let id = text(&item, "id")?.to_string();
                    tx.put("match", &id, &item).await?;
                }
            }
            json!({"registration":registration_public(registration)})
        }
        ("GET", "/watchers/matches") => {
            full(tx, p).await?;
            json!({"matches":pull(tx,"match",p,None,n(query,"limit",10)).await?})
        }
        _ if method == "POST"
            && path.starts_with("/watchers/matches/")
            && path.ends_with("/ack") =>
        {
            full(tx, p).await?;
            ack(tx, "match", p, &path[18..path.len() - 4], body).await?
        }
        ("POST", "/wake") => json!({"ok":true}),
        _ if path.starts_with("/inbox/") => {
            full(tx, p).await?;
            inbox(tx, p, method, &path[7..], body, query).await?
        }
        ("POST", "/sync/imports") => {
            full(tx, p).await?;
            sync::import(tx, p, body).await?
        }
        ("POST", "/sync/receipts") => {
            full(tx, p).await?;
            sync::receipt(tx, p, body).await?
        }
        ("GET", "/sync/find") => {
            let mut found_thread = Value::Null;
            let mut found_message = Value::Null;
            if let Some(post) = q(query, "post") {
                let imported = tx.get("sync-post", &format!("{}:{post}", p.id)).await?;
                let record = if imported.is_some() {
                    imported
                } else {
                    tx.get("post", &format!("{}:{post}", p.actor())).await?
                };
                if let Some(record) = record
                    && let Ok(message) = read_message(tx, p, text(&record, "message")?).await
                    && crate::access::allowed(tx, p, text(&message, "channel")?, "sync").await?
                {
                    found_message = message["id"].clone();
                    found_thread = message["thread"].clone();
                }
            }
            if let Some(thread) = q(query, "thread") {
                for message in tx.list("message").await? {
                    if !crate::access::allowed(tx, p, text(&message, "channel")?, "sync").await? {
                        continue;
                    }
                    let id = text(&message, "id")?;
                    let copies = tx.get("copies", id).await?.unwrap_or(json!({}));
                    let owners = tx.get("copy-owner", id).await?.unwrap_or(json!({}));
                    for (target, copy) in copies.as_object().into_iter().flatten() {
                        if owners[target] != p.id || copy["thread"] != thread {
                            continue;
                        }
                        found_thread = message["thread"].clone();
                        if q(query, "message").is_some_and(|id| copy["message"] == id) {
                            found_message = message["id"].clone();
                        }
                    }
                }
            }
            json!({"thread":found_thread,"message":found_message})
        }
        ("GET", "/sync/copies") => {
            let mut copies = json!({});
            for (_, id) in query.iter().filter(|(key, _)| key == "message") {
                let message = read_message(tx, p, id).await?;
                if !crate::access::allowed(tx, p, text(&message, "channel")?, "sync").await? {
                    continue;
                }
                let record = tx.get("copies", id).await?.unwrap_or(json!({}));
                let owners = tx.get("copy-owner", id).await?.unwrap_or(json!({}));
                let mut mine = json!({});
                for (target, copy) in record.as_object().into_iter().flatten() {
                    if owners[target] == p.id {
                        mine[target] = copy.clone();
                    }
                }
                copies[id] = mine;
            }
            json!({"copies":copies})
        }
        _ if method == "GET" && path.starts_with("/sync/threads/") => {
            let id = &path[14..];
            let message = read_message(tx, p, id).await?;
            need(tx, p, text(&message, "channel")?, "sync").await?;
            tx.get("sync-thread", id)
                .await?
                .ok_or_else(|| BoardError::missing("thread_not_found"))?
        }
        _ if path.starts_with("/messages/")
            && (path.ends_with("/sync") || path.ends_with("/typing")) =>
        {
            let (id, action) = path[10..]
                .split_once('/')
                .ok_or_else(|| BoardError::invalid("invalid message path"))?;
            let message = read_message(tx, p, id).await?;
            let thread = text(&message, "thread")?;
            if action == "typing" {
                need(tx, p, text(&message, "channel")?, "post").await?;
                json!({"targets":[]})
            } else if method == "GET" {
                let record = tx
                    .get("sync-thread", thread)
                    .await?
                    .unwrap_or(json!({"targets":[]}));
                let copies = tx.get("copies", id).await?.unwrap_or(json!({}));
                let targets:Vec<_>=record["targets"].as_array().into_iter().flatten().map(|target|json!({"target":target,"key":target.as_str().unwrap_or("").split(':').next(),"thread":copies[target.as_str().unwrap_or("")]["thread"],"message":copies[target.as_str().unwrap_or("")]["message"],"url":null,"undelivered":null})).collect();
                json!({"targets":targets})
            } else {
                need(tx, p, text(&message, "channel")?, "sync").await?;
                let mut record=tx.get("sync-thread",thread).await?.unwrap_or(json!({"thread":thread,"root":thread,"channel":message["channel"],"targets":[],"external":{}}));
                let targets = body["targets"]
                    .as_array()
                    .cloned()
                    .unwrap_or_else(|| body.get("target").cloned().into_iter().collect());
                for target in &targets {
                    if !record["targets"]
                        .as_array()
                        .expect("targets array")
                        .contains(target)
                    {
                        record["targets"]
                            .as_array_mut()
                            .expect("targets array")
                            .push(target.clone());
                    }
                }
                tx.put("sync-thread", thread, &record).await?;
                record
            }
        }
        ("GET", "/links") => return Err(BoardError::missing("message_not_found")),
        _ => return Ok(None),
    };
    Ok(Some(result))
}

async fn inbox(
    tx: &mut Tx<'_>,
    p: &Principal,
    method: &str,
    tail: &str,
    body: &Value,
    query: &[(String, String)],
) -> Result<Value, BoardError> {
    let parts: Vec<_> = tail.split('/').collect();
    let key = parts[0];
    let access = crate::access::summary(tx, p).await?;
    if !access
        .as_object()
        .into_iter()
        .flatten()
        .any(|(_, permissions)| {
            permissions
                .as_array()
                .is_some_and(|p| p.contains(&json!("sync")))
        })
    {
        return Err(BoardError::new(
            403,
            "forbidden",
            "inbox access requires current sync permission",
        ));
    }
    let registration = tx.get("registration", &p.id).await?;
    if !registration.as_ref().is_some_and(|r| {
        r["sync"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| entry["key"] == key))
    }) {
        return Err(BoardError::new(
            403,
            "forbidden",
            "sync key is not registered",
        ));
    }
    match (method, parts.as_slice()) {
        ("POST", [_]) => {
            let events = body["events"]
                .as_array()
                .ok_or_else(|| BoardError::invalid("events must be a list"))?;
            if events.is_empty() || events.len() > 50 {
                return Err(BoardError::invalid("inbox accepts 1 to 50 events"));
            }
            let mut count = 0;
            for item in events {
                let platform_id = text(item, "id")?;
                if platform_id.is_empty() || platform_id.len() > 300 {
                    return Err(BoardError::invalid("inbox event ID exceeds 300 characters"));
                }
                let dedupe = digest(&format!("{}:{key}:{platform_id}", p.id));
                if tx.get("inbox-id", &dedupe).await?.is_some() {
                    continue;
                }
                let id = uuid::Uuid::new_v4().simple().to_string();
                let mut event = json!({"type":"inbox.received","createdAt":now(),"payload":{}});
                tx.event(&mut event, None).await?;
                let queued = json!({"id":id,"owner":p.id,"key":key,"thread":item["event"]["thread"],"channel":item["event"]["channel"],"createdAt":now(),"sync":key,"event":item["event"],"raw":item["raw"],"attempt":0,"lease":null,"until":null,"platformId":platform_id,"position":tx.position});
                tx.put("inbox", &id, &queued).await?;
                tx.put("inbox-id", &dedupe, &json!({"id":id})).await?;
                count += 1;
            }
            Ok(json!({"accepted":count,"ok":true}))
        }
        ("GET", [_, "next"]) => {
            Ok(json!({"items":pull(tx,"inbox",p,Some(key),n(query,"limit",10)).await?}))
        }
        ("POST", [_, id, "ack"]) => ack(tx, "inbox", p, id, body).await,
        ("GET", [_]) => {
            let mut entries: Vec<_> = tx
                .list("inbox")
                .await?
                .into_iter()
                .filter(|r| r["owner"] == p.id && r["key"] == key)
                .collect();
            entries.reverse();
            entries.truncate(n(query, "limit", 50));
            Ok(json!({"events":entries,"cursor":tx.position.to_string()}))
        }
        _ => Err(BoardError::missing("route_not_found")),
    }
}
