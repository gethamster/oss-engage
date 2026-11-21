use crate::{actor, by, changed, db::Tx, n, need, now, public, q, read_message, registry, text};
use engage_core::{BoardError, Principal, Write, WriteKind, WriteSource};
use regex::Regex;
use serde_json::{Value, json};

pub(crate) fn channel_matches(pattern: &str, channel: &str) -> bool {
    pattern == "*"
        || pattern == channel
        || (pattern.ends_with('/')
            && (channel == pattern.trim_end_matches('/') || channel.starts_with(pattern)))
}
pub(crate) fn matches(message: &Value, query: &[(String, String)]) -> bool {
    for (key, value) in query {
        match key.as_str() {
            "channel" if !channel_matches(value, message["channel"].as_str().unwrap_or("")) => {
                return false;
            }
            "re" if message["thread"].as_str() != Some(value) => return false,
            "by" if message["by"]["actor"].as_str() != Some(value)
                && message["author"]["handle"].as_str() != Some(value) =>
            {
                return false;
            }
            "from"
                if if value == "board" {
                    !message["from"].is_null()
                } else {
                    message["from"].as_str() != Some(value)
                } =>
            {
                return false;
            }
            "text"
                if !value.to_lowercase().split_whitespace().all(|word| {
                    message["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(word)
                }) =>
            {
                return false;
            }
            "mentions"
                if !message["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains(&format!("@[{value}]")) =>
            {
                return false;
            }
            "label" => {
                let (key, values) = value
                    .split_once('=')
                    .map_or((value.as_str(), None), |(k, v)| (k, Some(v)));
                let actual = &message["labels"][key];
                if actual.is_null()
                    || values.is_some_and(|v| !v.split('|').any(|one| actual.as_str() == Some(one)))
                {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}
pub(crate) fn valid_channel(channel: &str) -> bool {
    !channel.is_empty()
        && channel.len() <= 2048
        && channel.split('/').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        })
}
pub(crate) fn labels(labels: &Value) -> Result<(), BoardError> {
    if labels.is_null() {
        return Ok(());
    }
    let map = labels
        .as_object()
        .ok_or_else(|| BoardError::invalid("labels must be an object"))?;
    if map.len() > 64 {
        return Err(BoardError::invalid("too many labels"));
    }
    for (key, value) in map {
        if key.starts_with("board.") {
            return Err(BoardError::new(
                400,
                "reserved_label",
                "board labels are reserved",
            ));
        }
        if key.is_empty()
            || key.len() > 64
            || !key
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.-".contains(c))
            || value.as_str().is_none_or(|v| v.len() > 500)
        {
            return Err(BoardError::invalid("invalid label"));
        }
    }
    Ok(())
}

pub(crate) async fn post(
    tx: &mut Tx<'_>,
    p: &Principal,
    body: &Value,
) -> Result<Value, BoardError> {
    post_inner(tx, p, body, None).await
}

pub(crate) async fn post_import(
    tx: &mut Tx<'_>,
    p: &Principal,
    body: &Value,
    author: &Value,
) -> Result<Value, BoardError> {
    post_inner(tx, p, body, Some(author)).await
}

async fn post_inner(
    tx: &mut Tx<'_>,
    p: &Principal,
    body: &Value,
    imported: Option<&Value>,
) -> Result<Value, BoardError> {
    if let Some(id) = body["id"].as_str() {
        if id.is_empty() || id.len() > 200 || id.contains('\0') {
            return Err(BoardError::invalid("invalid client ID"));
        }
        if let Some(record) = tx.get("post", &format!("{}:{id}", p.actor())).await? {
            return read_message(tx, p, text(&record, "message")?)
                .await
                .map(public);
        }
    }
    if body.get("channel").is_some() == body.get("re").is_some() {
        return Err(BoardError::invalid("a post needs channel or re"));
    }
    let (channel, re, reply_to, mut thread) = if let Some(re) = body["re"].as_str() {
        let parent = read_message(tx, p, re).await?;
        let root = text(&parent, "thread")?.to_string();
        let all = tx.select("message", None, Some(&root)).await?;
        let newest = all
            .iter()
            .max_by_key(|m| {
                m["id"]
                    .as_str()
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(0)
            })
            .and_then(|m| m["id"].as_str())
            .unwrap_or(&root);
        if body["after"].as_str().is_some_and(|a| a != newest) {
            return Err(BoardError::conflict(
                "newer_message",
                "another message came after the precondition",
            ));
        }
        (
            text(&parent, "channel")?.to_string(),
            json!(root),
            if re == root { Value::Null } else { json!(re) },
            root,
        )
    } else {
        let channel = text(body, "channel")?;
        if !valid_channel(channel) {
            return Err(BoardError::invalid("invalid channel path"));
        }
        (
            channel.to_string(),
            Value::Null,
            Value::Null,
            (tx.position + 1).to_string(),
        )
    };
    need(
        tx,
        p,
        &channel,
        if imported.is_some() { "sync" } else { "post" },
    )
    .await?;
    if body.get("after").is_some() && re.is_null() {
        return Err(BoardError::invalid("after is for replies"));
    }
    labels(&body["labels"])?;
    if imported.is_none() && body["labels"].as_object().is_some_and(|m| !m.is_empty()) {
        need(tx, p, &channel, "label").await?;
    }
    let sent = body["text"].as_str().unwrap_or("");
    if sent.contains('\0')
        || sent.len() > 65536
        || sent.trim().is_empty() && body["attachments"].as_array().is_none_or(Vec::is_empty)
    {
        return Err(BoardError::invalid("message text is empty or too long"));
    }
    if body["noSync"] == true && body["sync"].as_array().is_some_and(|v| !v.is_empty()) {
        return Err(BoardError::invalid("noSync and sync contradict"));
    }
    let mut attachments = vec![];
    if body["attachments"]
        .as_array()
        .is_some_and(|files| files.len() > 10)
    {
        return Err(BoardError::invalid("at most 10 attachments"));
    }
    for attachment_id in body["attachments"].as_array().into_iter().flatten() {
        let attachment = tx
            .get(
                "attachment",
                attachment_id
                    .as_str()
                    .ok_or_else(|| BoardError::invalid("invalid attachment ID"))?,
            )
            .await?
            .ok_or_else(|| BoardError::missing("attachment_not_found"))?;
        if attachment["owner"] != p.id {
            return Err(BoardError::missing("attachment_not_found"));
        }
        let mut visible = attachment;
        if let Some(o) = visible.as_object_mut() {
            o.remove("owner");
            o.remove("object");
        }
        attachments.push(visible);
    }
    let source = if imported.is_some() {
        let key = body["source"]["key"]
            .as_str()
            .or_else(|| body["from"].as_str())
            .unwrap_or("sync");
        WriteSource::Sync {
            key: key.into(),
            platform: key.into(),
            channel: body["source"]["channel"]
                .as_str()
                .or_else(|| body["origin"]["channel"].as_str())
                .unwrap_or(&channel)
                .into(),
            thread: body["source"]["thread"]
                .as_str()
                .or_else(|| body["origin"]["thread"].as_str())
                .unwrap_or(&thread)
                .into(),
        }
    } else {
        tx.source.clone().unwrap_or_default()
    };
    let write = Write {
        kind: WriteKind::Post,
        id: None,
        channel: channel.clone(),
        re: re.as_str().map(str::to_string),
        text: sent.to_string(),
        labels: serde_json::from_value(body.get("labels").cloned().unwrap_or(json!({})))
            .map_err(|_| BoardError::invalid("invalid labels"))?,
        attachments: attachments.clone(),
    };
    let screened = crate::middleware::run(tx, p, write, source).await?;
    if screened.text.contains('\0') || screened.text.len() > 65536 {
        return Err(BoardError::invalid("invalid message text"));
    }
    let screened_labels = serde_json::to_value(&screened.labels)
        .map_err(|_| BoardError::invalid("invalid labels"))?;
    labels(&screened_labels)?;
    let sent = screened.text.as_str();
    let blocks = if imported.is_some() {
        vec![]
    } else {
        registry::blocks(tx, sent).await?
    };
    if re.is_null() {
        thread = (tx.position + 1).to_string();
    }
    let actors = tx.list("actor").await?;
    let (stored, mentions) = mentions(sent, &actors);
    let id = (tx.position + 1).to_string();
    let mut message = json!({"id":id,"channel":channel,"text":stored,"re":re,"replyTo":reply_to,"thread":thread,"labels":screened_labels,"reactions":{},"by":by(p),"author":actor(p),"noSync":body["noSync"].as_bool().unwrap_or(false),"origin":null,"from":null,"createdAt":now(),"editedAt":null,"deleted":false,"undelivered":{},"attachments":attachments,"links":[],"blocks":blocks,"mentions":mentions});
    if let Some(author) = imported {
        message["author"] = author.clone();
        message["from"] = body.get("from").cloned().unwrap_or(Value::Null);
        message["origin"] = body.get("origin").cloned().unwrap_or(Value::Null);
        message["links"] = body.get("links").cloned().unwrap_or(json!([]));
    }
    if let Some(WriteSource::Schedule { id }) = &tx.source {
        message["schedule"] = json!({"id":id,"n":body["_schedule_n"].as_u64().unwrap_or(0)});
    }
    let mut payload = message.clone();
    payload
        .as_object_mut()
        .expect("message is object")
        .remove("author");
    changed(tx, p, &mut message, None, "message.posted", payload).await?;
    let old_channel = tx.get("channel", &channel).await?;
    let mut record =
        old_channel.unwrap_or(json!({"path":channel,"threads":0,"messages":0,"lastIndex":0}));
    record["messages"] = (record["messages"].as_u64().unwrap_or(0) + 1).into();
    if re.is_null() {
        record["threads"] = (record["threads"].as_u64().unwrap_or(0) + 1).into();
    }
    record["lastIndex"] = tx.position.into();
    tx.put("channel", &channel, &record).await?;
    if let Some(client_id) = body["id"].as_str() {
        tx.put(
            "post",
            &format!("{}:{client_id}", p.actor()),
            &json!({"message":id}),
        )
        .await?;
    }
    Ok(public(message))
}

fn mentions(sent: &str, actors: &[Value]) -> (String, Vec<Value>) {
    let pattern = Regex::new(r"(?m)(`[^`]*`|```[\s\S]*?```)|@([A-Za-z0-9][A-Za-z0-9._/-]*)")
        .expect("static mention pattern");
    let mut mentions = vec![];
    let stored = pattern
        .replace_all(sent, |capture: &regex::Captures<'_>| {
            let Some(handle) = capture.get(2).map(|m| m.as_str()) else {
                return capture[0].to_string();
            };
            let candidates: Vec<_> = actors
                .iter()
                .filter(|a| {
                    a["handle"]
                        .as_str()
                        .is_some_and(|h| h.eq_ignore_ascii_case(handle))
                })
                .cloned()
                .collect();
            let found = if candidates.len() == 1 {
                candidates.first().cloned()
            } else {
                None
            };
            mentions.push(
                json!({"written":format!("@{handle}"),"actor":found,"candidates":candidates}),
            );
            found
                .as_ref()
                .and_then(|a| a["id"].as_str())
                .map_or_else(|| capture[0].to_string(), |id| format!("@[{id}]"))
        })
        .to_string();
    (stored, mentions)
}

pub(crate) async fn summary(tx: &mut Tx<'_>, message: Value) -> Result<Value, BoardError> {
    let id = text(&message, "id")?;
    let replies: Vec<_> = tx
        .select("message", None, Some(id))
        .await?
        .into_iter()
        .filter(|m| m["re"].is_string() && m["deleted"] != true)
        .collect();
    let last = replies.last().cloned().map(public);
    Ok(json!({"message":public(message),"replies":replies.len(),"last":last}))
}

pub(crate) async fn route(
    tx: &mut Tx<'_>,
    p: &Principal,
    method: &str,
    path: &str,
    body: &Value,
    query: &[(String, String)],
) -> Result<Option<Value>, BoardError> {
    let result = match (method, path) {
        ("POST", "/messages") => {
            let message = post(tx, p, body).await?;
            json!({"mentions":message["mentions"],"message":message})
        }
        ("GET", "/posts") => return Err(BoardError::missing("post_not_found")),
        ("GET", "/actors") => {
            let ids: Vec<_> = query
                .iter()
                .filter(|(k, _)| k == "id")
                .map(|(_, v)| v)
                .collect();
            json!({"actors":tx.list("actor").await?.into_iter().filter(|a|ids.is_empty()||ids.iter().any(|id|a["id"]==id.as_str())).collect::<Vec<_>>()})
        }
        ("POST", "/channels") => {
            if body.as_object().is_none_or(|o| o.len() != 1) || !valid_channel(text(body, "path")?)
            {
                return Err(BoardError::invalid("a channel takes only a valid path"));
            }
            let channel = text(body, "path")?;
            need(tx, p, channel, "read").await?;
            need(tx, p, channel, "post").await?;
            let existing = tx.get("channel", channel).await?;
            let created = existing.is_none();
            let value =
                existing.unwrap_or(json!({"path":channel,"threads":0,"messages":0,"lastIndex":0}));
            tx.put("channel", channel, &value).await?;
            json!({"channel":value,"created":created})
        }
        ("GET", "/channels") => {
            let prefix = q(query, "prefix").unwrap_or("");
            let after = q(query, "after").unwrap_or("");
            let mut channels = vec![];
            for channel in tx.list("channel").await? {
                let path = text(&channel, "path")?;
                if path.starts_with(prefix)
                    && path > after
                    && crate::access::allowed(tx, p, path, "read").await?
                {
                    channels.push(channel);
                }
            }
            channels.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
            let limit = n(query, "limit", 100);
            let more = channels.len() > limit;
            channels.truncate(limit);
            json!({"next":if more{channels.last().map(|c|c["path"].clone()).unwrap_or(Value::Null)}else{Value::Null},"channels":channels,"cursor":tx.position.to_string()})
        }
        ("GET", "/messages") => {
            let channel =
                q(query, "channel").ok_or_else(|| BoardError::invalid("channel is required"))?;
            if !valid_channel(channel) {
                return Err(BoardError::invalid("messages reads one channel"));
            }
            need(tx, p, channel, "read").await?;
            let after = q(query, "after").and_then(|v| v.parse::<i64>().ok());
            let limit = n(query, "limit", 100);
            let mut messages = tx.select("message", Some(channel), None).await?;
            messages.sort_by_key(|m| {
                m["id"]
                    .as_str()
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(0)
            });
            if let Some(after) = after {
                messages.retain(|m| {
                    m["id"]
                        .as_str()
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(0)
                        > after
                });
            } else if messages.len() > limit {
                messages.drain(..messages.len() - limit);
            }
            let more = messages.len() > limit;
            messages.truncate(limit);
            let cursor = if more {
                messages
                    .last()
                    .map(|m| m["id"].clone())
                    .unwrap_or(json!(tx.position.to_string()))
            } else {
                json!(tx.position.to_string())
            };
            json!({"messages":messages.into_iter().map(public).collect::<Vec<_>>(),"cursor":cursor})
        }
        ("GET", "/threads") => threads(tx, p, query).await?,
        ("GET", "/events") => {
            if q(query, "last").is_some()
                && (q(query, "after").is_some() || q(query, "wait").is_some())
            {
                return Err(BoardError::invalid("last takes no after or wait"));
            }
            let after = q(query, "after").and_then(|s| s.parse().ok()).unwrap_or(
                if q(query, "last").is_some() {
                    0
                } else {
                    tx.position
                },
            );
            let mut events = vec![];
            for (event, before) in tx.events(after).await? {
                let message = &event["message"];
                if message.is_object()
                    && crate::access::allowed(
                        tx,
                        p,
                        message["channel"].as_str().unwrap_or(""),
                        "read",
                    )
                    .await?
                    && (matches(message, query)
                        || before.as_ref().is_some_and(|b| matches(b, query)))
                {
                    events.push(event);
                }
            }
            if let Some(last) = q(query, "last").and_then(|v| v.parse::<usize>().ok())
                && events.len() > last
            {
                events.drain(..events.len() - last);
            }
            json!({"events":events,"cursor":tx.position.to_string()})
        }
        _ if path.starts_with("/posts/") && method == "GET" => {
            let id = &path[7..];
            let record = tx
                .get("post", &format!("{}:{id}", p.actor()))
                .await?
                .ok_or_else(|| BoardError::missing("post_not_found"))?;
            json!({"message":public(read_message(tx,p,text(&record,"message")?).await?)})
        }
        _ if path.starts_with("/messages/") => {
            return message_route(tx, p, method, &path[10..], body, query).await;
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}

async fn threads(
    tx: &mut Tx<'_>,
    p: &Principal,
    query: &[(String, String)],
) -> Result<Value, BoardError> {
    if q(query, "before").is_some() && q(query, "after").is_some() {
        return Err(BoardError::invalid("before and after cannot be combined"));
    }
    let messages =
        if let Some(channel) = q(query, "channel").filter(|s| !s.ends_with('/') && *s != "*") {
            tx.select("message", Some(channel), None).await?
        } else {
            tx.list("message").await?
        };
    let mut roots = vec![];
    for root in messages.iter().filter(|m| m["re"].is_null()) {
        if !crate::access::allowed(tx, p, text(root, "channel")?, "read").await? {
            continue;
        }
        let id = text(root, "id")?;
        let thread: Vec<_> = messages.iter().filter(|m| m["thread"] == id).collect();
        let labels: Vec<_> = query
            .iter()
            .filter(|(key, _)| key == "label")
            .cloned()
            .collect();
        let content: Vec<_> = query
            .iter()
            .filter(|(key, _)| key != "label")
            .cloned()
            .collect();
        if !matches(root, &labels) || !thread.iter().any(|m| matches(m, &content)) {
            continue;
        }
        let activity = thread
            .iter()
            .filter_map(|m| m["version"].as_i64())
            .max()
            .unwrap_or(0);
        if q(query, "before")
            .and_then(|v| v.parse::<i64>().ok())
            .is_some_and(|before| activity >= before)
        {
            continue;
        }
        if q(query, "after")
            .and_then(|v| v.parse::<i64>().ok())
            .is_some_and(|after| activity <= after)
        {
            continue;
        }
        roots.push((activity, root.clone()));
    }
    roots.sort_by_key(|(position, _)| *position);
    let limit = n(query, "limit", 50);
    let more = roots.len() > limit;
    if more {
        roots.drain(..roots.len() - limit);
    }
    let older = if more {
        roots
            .first()
            .map(|(position, _)| json!(position.to_string()))
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let mut summaries = vec![];
    for (_, root) in roots {
        summaries.push(summary(tx, root).await?);
    }
    Ok(json!({"threads":summaries,"cursor":tx.position.to_string(),"older":older}))
}

async fn message_route(
    tx: &mut Tx<'_>,
    p: &Principal,
    method: &str,
    tail: &str,
    body: &Value,
    query: &[(String, String)],
) -> Result<Option<Value>, BoardError> {
    let (id, action) = tail.split_once('/').unwrap_or((tail, ""));
    if action == "sync" || action == "typing" {
        return Ok(None);
    }
    let mut message = read_message(tx, p, id).await?;
    if action.is_empty() && method == "GET" {
        let root = text(&message, "thread")?.to_string();
        let root_message = read_message(tx, p, &root).await?;
        let mut replies: Vec<_> = tx
            .select("message", None, Some(&root))
            .await?
            .into_iter()
            .filter(|m| m["re"].is_string())
            .collect();
        replies.sort_by_key(|m| {
            m["id"]
                .as_str()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0)
        });
        let limit = n(query, "limit", 500);
        if let Some(after) = q(query, "after") {
            let after = after
                .parse::<i64>()
                .map_err(|_| BoardError::invalid("invalid reply cursor"))?;
            replies.retain(|m| {
                m["id"]
                    .as_str()
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(0)
                    > after
            });
        } else if replies.len() > limit {
            replies.drain(..replies.len() - limit);
        }
        replies.truncate(limit);
        return Ok(Some(
            json!({"message":public(root_message),"replies":replies.into_iter().map(public).collect::<Vec<_>>(),"cursor":tx.position.to_string(),"schedules":[]}),
        ));
    }
    if method != "POST" {
        return Ok(None);
    }
    let channel = text(&message, "channel")?.to_string();
    let before = message.clone();
    let own = message["by"]["principal"] == p.id
        && (p.profile.is_none() || message["by"]["actor"] == p.actor());
    if matches!(action, "edit" | "delete") {
        if !own {
            return Err(BoardError::new(
                403,
                "forbidden",
                "message belongs to another actor",
            ));
        } else {
            need(tx, p, &channel, "post").await?;
        }
        if body["revision"]
            .as_i64()
            .is_some_and(|r| Some(r) != message["revision"].as_i64())
        {
            return Err(BoardError::conflict(
                "revision_conflict",
                "message revision changed",
            ));
        }
        if message["deleted"] == true {
            return Err(BoardError::missing("message_not_found"));
        }
    }
    let (kind, payload) = match action {
        "edit" => {
            let sent = text(body, "text")?;
            if sent.trim().is_empty() || sent.len() > 65536 || sent.contains('\0') {
                return Err(BoardError::invalid("invalid message text"));
            }
            let write = Write {
                kind: WriteKind::Edit,
                id: Some(id.to_string()),
                channel: channel.clone(),
                re: message["re"].as_str().map(str::to_string),
                text: sent.to_string(),
                labels: Default::default(),
                attachments: vec![],
            };
            let screened =
                crate::middleware::run(tx, p, write, tx.source.clone().unwrap_or_default()).await?;
            let sent = screened.text.as_str();
            if sent.contains('\0') || sent.len() > 65536 {
                return Err(BoardError::invalid("invalid message text"));
            }
            message["blocks"] = registry::blocks(tx, sent).await?.into();
            message["text"] = sent.into();
            message["editedAt"] = now().into();
            (
                "message.edited",
                json!({"id":id,"text":sent,"by":by(p),"from":null}),
            )
        }
        "delete" => {
            message["deleted"] = true.into();
            message["text"] = "".into();
            message["labels"] = json!({});
            message["reactions"] = json!({});
            message["attachments"] = json!([]);
            message["links"] = json!([]);
            message["blocks"] = json!([]);
            message["mentions"] = json!([]);
            if message["task"].is_object() {
                message["task"]["state"] = "canceled".into();
                message["task"]["claim"] = Value::Null;
                message
                    .as_object_mut()
                    .expect("message object")
                    .remove("_claim_token");
            }
            ("message.deleted", json!({"id":id,"by":by(p),"from":null}))
        }
        "reactions" => {
            need(tx, p, &channel, "react").await?;
            let emoji = text(body, "emoji")?.trim_matches(':').to_ascii_lowercase();
            let emoji = match emoji.as_str() {
                "+1" | "👍" => "thumbs_up",
                "white_check_mark" | "✅" => "check",
                other => other,
            };
            let mut actors = message["reactions"][emoji]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let remove = body["remove"] == true;
            if remove {
                actors.retain(|a| a != &p.actor());
            } else if !actors.iter().any(|a| a == &p.actor()) {
                actors.push(p.actor().into());
            }
            if actors.is_empty() {
                message["reactions"]
                    .as_object_mut()
                    .expect("reaction map")
                    .remove(emoji);
            } else {
                message["reactions"][emoji] = actors.into();
            }
            (
                if remove {
                    "reaction.removed"
                } else {
                    "reaction.added"
                },
                json!({"id":id,"emoji":emoji,"by":by(p),"from":null}),
            )
        }
        "labels" => {
            need(tx, p, &channel, "label").await?;
            labels(&body["set"])?;
            if body.get("set").is_none() && body.get("unset").is_none() {
                return Err(BoardError::invalid("nothing to change"));
            }
            for (key, expected) in body["if"].as_object().into_iter().flatten() {
                if &message["labels"][key] != expected {
                    return Err(BoardError::conflict("label_conflict", "labels changed"));
                }
            }
            for (key, value) in body["set"].as_object().into_iter().flatten() {
                message["labels"][key] = value.clone();
            }
            for key in body["unset"].as_array().into_iter().flatten() {
                if let Some(key) = key.as_str() {
                    if key.starts_with("board.") {
                        return Err(BoardError::new(
                            400,
                            "reserved_label",
                            "board labels are reserved",
                        ));
                    }
                    message["labels"]
                        .as_object_mut()
                        .expect("label map")
                        .remove(key);
                }
            }
            (
                "labels.changed",
                json!({"id":id,"set":body.get("set").cloned().unwrap_or(json!({})),"unset":body.get("unset").cloned().unwrap_or(json!([])),"by":by(p),"from":null}),
            )
        }
        _ => return Ok(None),
    };
    changed(tx, p, &mut message, Some(&before), kind, payload).await?;
    let owns_task = message["task"]["owner"] == p.id
        && (p.profile.is_none() || message["task"]["by"] == p.actor());
    if action == "edit" && message["task"].is_object() && !owns_task {
        crate::tasks::edited(tx, p, &mut message).await?;
    }
    Ok(Some(json!({"message":public(message)})))
}
