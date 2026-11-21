use crate::{by, changed, db::Tx, messages, need, now, read_message, text};
use engage_core::{BoardError, Principal};
use serde_json::{Value, json};

async fn imported_actor(
    tx: &mut Tx<'_>,
    p: &Principal,
    author: &Value,
    from: Option<&str>,
) -> Result<(Principal, Value), BoardError> {
    let profile = text(author, "id")?;
    let registration = tx
        .get("registration", &p.id)
        .await?
        .ok_or_else(|| BoardError::new(403, "forbidden", "sync entries must be registered"))?;
    let entry = registration["sync"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| {
            entry["key"].as_str().is_some_and(|key| {
                profile.starts_with(&format!("{key}:")) && from.is_none_or(|from| from == key)
            })
        })
        .ok_or_else(|| {
            BoardError::new(
                403,
                "forbidden",
                "platform author does not belong to a registered sync key",
            )
        })?;
    let platform = entry["platform"].as_str().unwrap_or("").to_string();
    if tx
        .get("actor", profile)
        .await?
        .is_some_and(|actor| actor["kind"] != "external")
    {
        return Err(BoardError::new(
            403,
            "forbidden",
            "sync cannot impersonate a signed-in board actor",
        ));
    }
    let mut imported = p.clone();
    imported.profile = Some(profile.to_string());
    let avatar = author["avatar"]
        .as_str()
        .filter(|url| url.starts_with("https://"));
    let view = json!({"id":imported.actor(),"kind":"external","handle":format!("{}/{}",p.handle,author["handle"].as_str().unwrap_or(profile)),"name":author["name"],"avatar":avatar,"platform":platform,"via":{"name":p.name,"environment":null}});
    tx.put("actor", &imported.actor(), &view).await?;
    Ok((imported, view))
}
pub(super) async fn import(
    tx: &mut Tx<'_>,
    p: &Principal,
    body: &Value,
) -> Result<Value, BoardError> {
    let op = text(body, "op")?;
    let author = &body["author"];
    let from = body["from"].as_str();
    let (imported, view) = imported_actor(tx, p, author, from).await?;
    if let Some(origin) = body.get("origin").filter(|origin| origin.is_object()) {
        let key = from.ok_or_else(|| BoardError::invalid("an origin requires its sync key"))?;
        for field in ["channel", "thread", "subthread"] {
            if origin[field]
                .as_str()
                .is_some_and(|value| !value.starts_with(&format!("{key}:")))
            {
                return Err(BoardError::invalid(
                    "origin location belongs to another sync key",
                ));
            }
        }
    }
    let target = if let Some(id) = body["message"].as_str() {
        Some(read_message(tx, p, id).await?)
    } else if let Some(thread) = body["origin"]["thread"].as_str() {
        let mut found = None;
        for message in tx.list("message").await? {
            if message["origin"]["thread"] == thread
                && (body["origin"]["message"].is_null()
                    || message["origin"]["message"] == body["origin"]["message"])
            {
                found = Some(message);
                break;
            }
        }
        found
    } else {
        None
    };
    if op == "post" {
        let channel = if let Some(re) = body["re"].as_str() {
            text(&read_message(tx, p, re).await?, "channel")?.to_string()
        } else {
            text(body, "channel")?.to_string()
        };
        need(tx, p, &channel, "sync").await?;
        if let Some(client_id) = body["id"].as_str()
            && let Some(landed) = tx
                .get("sync-post", &format!("{}:{client_id}", p.id))
                .await?
        {
            let message = read_message(tx, p, text(&landed, "message")?).await?;
            return Ok(json!({"message":message["id"]}));
        }
        if let Some(client_id) = body["id"].as_str()
            && let Some(landed) = tx
                .get("post", &format!("{}:{client_id}", imported.actor()))
                .await?
        {
            let message = read_message(tx, p, text(&landed, "message")?).await?;
            return Ok(json!({"message":message["id"]}));
        }
        let origin_thread = body["origin"]["thread"].as_str();
        let mut occupied = target.is_some();
        if let Some(origin_thread) = origin_thread {
            for record in tx.list("sync-thread").await? {
                if record["targets"]
                    .as_array()
                    .is_some_and(|targets| targets.iter().any(|t| t == origin_thread))
                {
                    occupied = true;
                    break;
                }
            }
        }
        if occupied && body["re"].is_null() {
            return Err(BoardError::conflict(
                "already_synced",
                "platform thread belongs to another board thread",
            ));
        }
        let mut post = body.clone();
        let mut sent = post["text"].as_str().unwrap_or("").to_string();
        for mentioned in body["mentioned"].as_array().into_iter().flatten() {
            let (id, _) = imported_actor(tx, p, mentioned, from).await?;
            let raw = text(mentioned, "id")?;
            sent = sent.replace(&format!("@[{raw}]"), &format!("@[{}]", id.actor()));
        }
        post["text"] = sent.into();
        // Imported platform text is not schema-checked, even if it resembles a data fence.
        let mut message = messages::post_import(tx, &imported, &post, &view).await?;
        let id = text(&message, "id")?.to_string();
        message["author"] = view;
        message["from"] = body.get("from").cloned().unwrap_or(Value::Null);
        message["origin"] = body.get("origin").cloned().unwrap_or(Value::Null);
        message["blocks"] = json!([]);
        message["links"] = body.get("links").cloned().unwrap_or(json!([]));
        tx.put("message", &id, &message).await?;
        let thread = text(&message, "thread")?.to_string();
        if body["origin"]["thread"].is_string() {
            let target = body["origin"]["thread"].clone();
            let mut record = tx.get("sync-thread", &thread).await?.unwrap_or(
                json!({"thread":thread,"root":thread,"channel":channel,"targets":[],"external":{}}),
            );
            if !record["targets"]
                .as_array()
                .expect("target array")
                .contains(&target)
            {
                record["targets"]
                    .as_array_mut()
                    .expect("target array")
                    .push(target.clone());
            }
            record["external"][target.as_str().unwrap_or("")] = target.clone();
            tx.put("sync-thread", &thread, &record).await?;
            let origin = &body["origin"];
            let mut copies = tx.get("copies", &id).await?.unwrap_or(json!({}));
            copies[target.as_str().unwrap_or("")] = json!({"thread":origin["subthread"].as_str().unwrap_or(target.as_str().unwrap_or("")),"message":origin["message"]});
            tx.put("copies", &id, &copies).await?;
            let mut owners = tx.get("copy-owner", &id).await?.unwrap_or(json!({}));
            owners[target.as_str().unwrap_or("")] = p.id.clone().into();
            tx.put("copy-owner", &id, &owners).await?;
        }
        if let Some(client_id) = body["id"].as_str() {
            tx.put(
                "sync-post",
                &format!("{}:{client_id}", p.id),
                &json!({"message":id}),
            )
            .await?;
        }
        return Ok(json!({"message":id}));
    }
    let mut message = target.ok_or_else(|| BoardError::missing("message_not_found"))?;
    need(tx, p, text(&message, "channel")?, "sync").await?;
    let id = text(&message, "id")?.to_string();
    let before = message.clone();
    if body["revision"]
        .as_i64()
        .is_some_and(|r| message["revision"] != r)
    {
        return Err(BoardError::conflict(
            "revision_conflict",
            "message revision changed",
        ));
    }
    let kind = match op {
        "edit" => {
            let source = engage_core::WriteSource::Sync {
                key: body["from"].as_str().unwrap_or("sync").into(),
                platform: body["from"].as_str().unwrap_or("sync").into(),
                channel: message["origin"]["channel"].as_str().unwrap_or("").into(),
                thread: message["origin"]["thread"].as_str().unwrap_or("").into(),
            };
            let write = engage_core::Write {
                kind: engage_core::WriteKind::Edit,
                id: Some(id.clone()),
                channel: text(&message, "channel")?.into(),
                re: message["re"].as_str().map(str::to_string),
                text: text(body, "text")?.into(),
                labels: Default::default(),
                attachments: vec![],
            };
            let screened = crate::middleware::run(tx, &imported, write, source).await?;
            if screened.text.contains('\0') || screened.text.len() > 65536 {
                return Err(BoardError::invalid("invalid imported message text"));
            }
            message["text"] = screened.text.into();
            message["editedAt"] = now().into();
            message["blocks"] = json!([]);
            "message.edited"
        }
        "delete" => {
            message["deleted"] = true.into();
            message["text"] = "".into();
            message["labels"] = json!({});
            message["attachments"] = json!([]);
            message["reactions"] = json!({});
            message["blocks"] = json!([]);
            message["mentions"] = json!([]);
            message["links"] = json!([]);
            if message["task"].is_object() {
                message["task"]["state"] = "canceled".into();
                message["task"]["claim"] = Value::Null;
                message
                    .as_object_mut()
                    .expect("message object")
                    .remove("_claim_token");
            }
            "message.deleted"
        }
        "react" => {
            let emoji = text(body, "emoji")?;
            let mut reactions = message["reactions"][emoji]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if body["remove"] == true {
                reactions.retain(|r| r != &imported.actor());
            } else if !reactions.contains(&json!(imported.actor())) {
                reactions.push(imported.actor().into());
            }
            message["reactions"][emoji] = reactions.into();
            if body["remove"] == true {
                "reaction.removed"
            } else {
                "reaction.added"
            }
        }
        "labels" => {
            messages::labels(&body["set"])?;
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
            "labels.changed"
        }
        "links" => {
            message["links"] = body["links"].clone();
            "message.linked"
        }
        _ => return Err(BoardError::invalid("invalid sync operation")),
    };
    let mut payload = body.clone();
    payload["id"] = id.clone().into();
    payload["by"] = by(&imported);
    changed(tx, &imported, &mut message, Some(&before), kind, payload).await?;
    let owns_task = message["task"]["owner"] == imported.id
        && (imported.profile.is_none() || message["task"]["by"] == imported.actor());
    if op == "edit" && message["task"].is_object() && !owns_task {
        crate::tasks::edited(tx, &imported, &mut message).await?;
    }
    Ok(json!({"message":id}))
}

pub(super) async fn receipt(
    tx: &mut Tx<'_>,
    p: &Principal,
    body: &Value,
) -> Result<Value, BoardError> {
    let id = text(body, "message")?;
    let mut message = read_message(tx, p, id).await?;
    need(tx, p, text(&message, "channel")?, "sync").await?;
    let target = text(body, "target")?;
    let thread = text(&message, "thread")?.to_string();
    let mut record=tx.get("sync-thread",&thread).await?.unwrap_or(json!({"thread":thread,"root":thread,"channel":message["channel"],"targets":[],"external":{}}));
    match text(body, "kind")? {
        "thread" => {
            if !record["targets"]
                .as_array()
                .expect("targets array")
                .contains(&json!(target))
            {
                record["targets"]
                    .as_array_mut()
                    .expect("targets array")
                    .push(target.into());
            }
            record["external"][target] = target.into();
            tx.put("sync-thread", &thread, &record).await?;
        }
        "message" => {
            if !record["targets"]
                .as_array()
                .expect("targets array")
                .contains(&json!(target))
            {
                return Err(BoardError::invalid("receipt target is not on this thread"));
            }
            let mut copies = tx.get("copies", id).await?.unwrap_or(json!({}));
            copies[target] = body["external"].clone();
            tx.put("copies", id, &copies).await?;
            let mut owners = tx.get("copy-owner", id).await?.unwrap_or(json!({}));
            owners[target] = p.id.clone().into();
            tx.put("copy-owner", id, &owners).await?;
            record["external"][target] = body["external"]["thread"].clone();
            tx.put("sync-thread", &thread, &record).await?;
            message["undelivered"]
                .as_object_mut()
                .expect("undelivered map")
                .remove(target);
            tx.put("message", id, &message).await?;
        }
        "files" => {
            let mut copies = tx.get("copies", id).await?.unwrap_or(json!({}));
            copies[target]["files"] = body["files"].clone();
            tx.put("copies", id, &copies).await?;
        }
        "failed" => {
            message["undelivered"][target] = body["error"].clone();
            tx.put("message", id, &message).await?;
        }
        _ => return Err(BoardError::invalid("invalid receipt kind")),
    }
    Ok(json!({"ok":true}))
}
