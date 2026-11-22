use crate::{actor, by, db::Tx, messages, need, now, q, read_message, text};
use chrono::{DateTime, Duration, Utc};
use engage_core::{BoardError, Principal};
use regex::Regex;
use serde_json::{Value, json};

fn builtins() -> Vec<Value> {
    let sha = json!({"type":"string","pattern":"^(?:[0-9a-f]{40}|[0-9a-f]{64})$","description":"Full lowercase commit SHA"});
    vec![
        json!({"id":"git-commit@1","name":"git-commit","version":1,"description":"A Git commit, by its full SHA and the first line of its message.","schema":{"type":"object","additionalProperties":false,"required":["sha","title"],"properties":{"sha":sha,"title":{"type":"string","minLength":1,"maxLength":1024}}},"builtIn":true,"by":null,"at":null}),
        json!({"id":"vercel-deployment@1","name":"vercel-deployment","version":1,"description":"A Vercel deployment, by its URL and optionally the commit it was built from.","schema":{"type":"object","additionalProperties":false,"required":["url"],"properties":{"url":{"type":"string","format":"uri","pattern":"^https://","maxLength":2048},"gitSha":sha}},"builtIn":true,"by":null,"at":null}),
    ]
}
async fn schema(tx: &mut Tx<'_>, id: &str) -> Result<Value, BoardError> {
    if !Regex::new(r"^[a-z][a-z0-9._-]{0,79}@[1-9][0-9]{0,4}$")
        .expect("schema regex")
        .is_match(id)
    {
        return Err(BoardError::new(
            400,
            "invalid_schema_id",
            "schema ID is name@version",
        ));
    }
    if let Some(builtin) = builtins().into_iter().find(|s| s["id"] == id) {
        return Ok(builtin);
    }
    tx.get("schema", id)
        .await?
        .ok_or_else(|| BoardError::missing("schema_not_found"))
}
fn check_schema(schema: &Value, depth: usize, nodes: &mut usize) -> Result<(), BoardError> {
    *nodes += 1;
    if depth > 16 || *nodes > 512 {
        return Err(BoardError::new(
            400,
            "invalid_schema",
            "schema exceeds complexity limits",
        ));
    }
    if let Some(object) = schema.as_object() {
        for (key, value) in object {
            if matches!(
                key.as_str(),
                "$ref"
                    | "$dynamicRef"
                    | "pattern"
                    | "patternProperties"
                    | "uniqueItems"
                    | "unevaluatedProperties"
                    | "contentEncoding"
                    | "contentMediaType"
            ) {
                return Err(BoardError::new(
                    400,
                    "invalid_schema",
                    "schema keyword is not supported",
                ));
            }
            check_schema(value, depth + 1, nodes)?;
        }
    } else if let Some(array) = schema.as_array() {
        for item in array {
            check_schema(item, depth + 1, nodes)?;
        }
    }
    Ok(())
}

pub(crate) async fn blocks(tx: &mut Tx<'_>, sent: &str) -> Result<Vec<Value>, BoardError> {
    let mut blocks = vec![];
    let lines: Vec<_> = sent.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim_start();
        if !line.starts_with("```") {
            index += 1;
            continue;
        }
        let width = line.chars().take_while(|c| *c == '`').count();
        let fence = "`".repeat(width);
        let schema_id = line[width..].strip_prefix("json#");
        let mut end = index + 1;
        while end < lines.len() && lines[end].trim() != fence {
            end += 1;
        }
        if let Some(id) = schema_id.filter(|_| width == 3) {
            let schema = schema(tx, id)
                .await
                .map_err(|_| BoardError::new(400, "schema_not_found", "block schema not found"))?;
            let data: Value = serde_json::from_str(&lines[index + 1..end].join("\n"))
                .map_err(|_| BoardError::new(400, "invalid_block", "block is not valid JSON"))?;
            let validator = jsonschema::validator_for(&schema["schema"]).map_err(|_| {
                BoardError::new(400, "invalid_block", "schema cannot validate block")
            })?;
            if !data.is_object() || !validator.is_valid(&data) {
                return Err(BoardError::new(
                    400,
                    "invalid_block",
                    "block does not match schema",
                ));
            }
            blocks.push(json!({"schema":schema["name"],"version":schema["version"],"data":data}));
        }
        index = end + 1;
    }
    Ok(blocks)
}

fn duration(value: &str) -> Result<i64, BoardError> {
    if !Regex::new(r"^[0-9]+(?:\.[0-9]+)?[smhd]$")
        .expect("duration pattern")
        .is_match(value)
    {
        return Err(BoardError::invalid("duration must look like 30s or 1d"));
    }
    let unit = value
        .as_bytes()
        .last()
        .copied()
        .filter(|unit| matches!(unit, b's' | b'm' | b'h' | b'd'))
        .ok_or_else(|| BoardError::invalid("duration must look like 30s or 1d"))?;
    let number = value
        .strip_suffix(char::from(unit))
        .ok_or_else(|| BoardError::invalid("invalid duration"))?;
    let number = number
        .parse::<f64>()
        .map_err(|_| BoardError::invalid("duration must look like 30s or 1d"))?;
    let multiplier = match unit {
        b's' => 1000.0,
        b'm' => 60000.0,
        b'h' => 3600000.0,
        b'd' => 86400000.0,
        _ => return Err(BoardError::invalid("invalid duration unit")),
    };
    if !number.is_finite() || number * multiplier < 1.0 || number * multiplier > 315360000000.0 {
        return Err(BoardError::invalid("invalid duration"));
    }
    Ok((number * multiplier) as i64)
}
fn schedule_public(mut schedule: Value) -> Value {
    if let Some(map) = schedule.as_object_mut() {
        map.remove("_principal");
        map.remove("_body");
    }
    schedule
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
        ("GET", "/schemas") => {
            let mut schemas = builtins();
            schemas.extend(tx.list("schema").await?);
            json!({"schemas":schemas})
        }
        ("POST", "/schemas") => {
            need(tx, p, "*", "manage").await?;
            let name = text(body, "name")?;
            let version = body["version"]
                .as_u64()
                .filter(|v| *v > 0 && *v <= 99999)
                .ok_or_else(|| BoardError::new(400, "invalid_schema", "invalid schema version"))?;
            if !Regex::new(r"^[a-z][a-z0-9._-]{0,79}$")
                .expect("schema name regex")
                .is_match(name)
            {
                return Err(BoardError::new(
                    400,
                    "invalid_schema",
                    "invalid schema name",
                ));
            }
            if matches!(name, "git-commit" | "vercel-deployment") {
                return Err(BoardError::conflict(
                    "reserved_schema",
                    "built-in schema names are reserved",
                ));
            }
            let definition = &body["schema"];
            if definition["type"] != "object" || definition.to_string().len() > 32768 {
                return Err(BoardError::new(
                    400,
                    "invalid_schema",
                    "schema must describe an object within 32KiB",
                ));
            }
            check_schema(definition, 0, &mut 0)?;
            jsonschema::validator_for(definition)
                .map_err(|_| BoardError::new(400, "invalid_schema", "invalid schema definition"))?;
            let id = format!("{name}@{version}");
            if let Some(existing) = tx.get("schema", &id).await? {
                if &existing["schema"] != definition
                    || existing["description"]
                        != body.get("description").cloned().unwrap_or(Value::Null)
                {
                    return Err(BoardError::conflict(
                        "schema_immutable",
                        "schema version already has another definition",
                    ));
                }
                json!({"schema":existing,"created":false})
            } else {
                let schema = json!({"id":id,"name":name,"version":version,"description":body.get("description").cloned().unwrap_or(Value::Null),"schema":definition,"builtIn":false,"by":p.actor(),"at":now()});
                tx.put("schema", &id, &schema).await?;
                json!({"schema":schema,"created":true})
            }
        }
        _ if method == "GET" && path.starts_with("/schemas/") => {
            json!({"schema":schema(tx,&path[9..]).await?})
        }
        ("POST", "/schedules") => {
            if body.get("channel").is_some() == body.get("re").is_some() {
                return Err(BoardError::invalid("schedule needs channel or re"));
            }
            let re = body["re"].as_str();
            let channel = if let Some(re) = re {
                text(&read_message(tx, p, re).await?, "channel")?.to_string()
            } else {
                text(body, "channel")?.to_string()
            };
            need(tx, p, &channel, "post").await?;
            messages::labels(&body["labels"])?;
            if !messages::valid_channel(&channel) {
                return Err(BoardError::invalid("invalid channel path"));
            }
            let scheduled_text = body["text"].as_str().unwrap_or("");
            if scheduled_text.contains('\0') || scheduled_text.len() > 65536 {
                return Err(BoardError::invalid("invalid scheduled message text"));
            }
            blocks(tx, scheduled_text).await?;
            if body["labels"]
                .as_object()
                .is_some_and(|labels| !labels.is_empty())
            {
                need(tx, p, &channel, "label").await?;
            }
            let attachments = body["attachments"].as_array().cloned().unwrap_or_default();
            if attachments.len() > 10 {
                return Err(BoardError::invalid("too many schedule attachments"));
            }
            for attachment in &attachments {
                let id = attachment
                    .as_str()
                    .ok_or_else(|| BoardError::invalid("invalid attachment ID"))?;
                let record = tx
                    .get("attachment", id)
                    .await?
                    .ok_or_else(|| BoardError::missing("attachment_not_found"))?;
                if record["owner"] != p.id {
                    return Err(BoardError::missing("attachment_not_found"));
                }
            }
            if scheduled_text.trim().is_empty()
                && body["attachments"].as_array().is_none_or(Vec::is_empty)
            {
                return Err(BoardError::invalid("schedule needs text or attachments"));
            }
            if body["in"].is_string() && body["at"].is_string() {
                return Err(BoardError::invalid("in and at contradict"));
            }
            let every = body["every"].as_str().map(duration).transpose()?;
            let first = if let Some(at) = body["at"].as_str() {
                DateTime::parse_from_rfc3339(at)
                    .map_err(|_| BoardError::invalid("invalid schedule timestamp"))?
                    .with_timezone(&Utc)
            } else if let Some(delay) = body["in"].as_str() {
                tx.at + Duration::milliseconds(duration(delay)?)
            } else if let Some(every) = every {
                tx.at + Duration::milliseconds(every)
            } else {
                return Err(BoardError::invalid("schedule needs in, at or every"));
            };
            if ["until", "for", "times"]
                .iter()
                .filter(|key| body.get(**key).is_some())
                .count()
                > 1
                || every.is_none()
                    && ["until", "for", "times"]
                        .iter()
                        .any(|key| body.get(*key).is_some())
            {
                return Err(BoardError::invalid("repeat stop options contradict"));
            }
            let until = if let Some(span) = body["for"].as_str() {
                Some(
                    (first + Duration::milliseconds(duration(span)?))
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                )
            } else {
                body["until"].as_str().map(str::to_string)
            };
            let id = format!("s{}", uuid::Uuid::new_v4().simple());
            let value = json!({"id":id,"channel":channel,"re":re,"text":body["text"].as_str().unwrap_or(""),"labels":body.get("labels").cloned().unwrap_or(json!({})),"noSync":body["noSync"].as_bool().unwrap_or(false),"attachments":[],"first":first.to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"every":every,"until":until,"times":body.get("times").cloned().unwrap_or(Value::Null),"sent":0,"next":first.to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"last":null,"state":"pending","canceled":null,"by":by(p),"author":actor(p),"createdAt":now(),"_principal":p,"_body":body});
            tx.put("schedule", &id, &value).await?;
            json!({"schedule":schedule_public(value)})
        }
        ("GET", "/schedules") => {
            let mut schedules = vec![];
            for schedule in tx.list("schedule").await? {
                if !crate::access::allowed(tx, p, text(&schedule, "channel")?, "read").await? {
                    continue;
                }
                if !matches!(q(query, "all"), Some("true" | "1")) && schedule["state"] != "pending"
                    || q(query, "channel").is_some_and(|v| {
                        !messages::channel_matches(v, schedule["channel"].as_str().unwrap_or(""))
                    })
                    || q(query, "re").is_some_and(|v| schedule["re"] != v)
                    || q(query, "by").is_some_and(|v| {
                        schedule["by"]["actor"] != v && schedule["author"]["handle"] != v
                    })
                {
                    continue;
                }
                schedules.push(schedule_public(schedule));
            }
            json!({"schedules":schedules})
        }
        _ if method == "POST" && path.starts_with("/schedules/") && path.ends_with("/cancel") => {
            let id = path
                .trim_start_matches("/schedules/")
                .trim_end_matches("/cancel");
            let mut schedule = tx
                .get("schedule", id)
                .await?
                .ok_or_else(|| BoardError::missing("schedule_not_found"))?;
            if schedule["by"]["principal"] != p.id
                || p.profile.is_some() && schedule["by"]["actor"] != p.actor()
            {
                need(tx, p, text(&schedule, "channel")?, "manage").await?;
            }
            if schedule["state"] == "pending" {
                schedule["state"] = "canceled".into();
                schedule["next"] = Value::Null;
                schedule["canceled"] = json!({"by":by(p),"reason":"canceled"});
                tx.put("schedule", id, &schedule).await?;
            }
            json!({"schedule":schedule_public(schedule)})
        }
        ("POST", "/attachments") => {
            let id = text(body, "id")?;
            let name = body["name"].as_str().unwrap_or("attachment");
            if name
                .rsplit(['/', '\\'])
                .next()
                .is_some_and(|name| name.starts_with('.'))
            {
                return Err(BoardError::invalid("attachment name cannot be a dotfile"));
            }
            let mut value = body.clone();
            value["owner"] = p.id.clone().into();
            tx.put("attachment", id, &value).await?;
            let mut visible = value;
            let map = visible.as_object_mut().expect("attachment object");
            map.remove("owner");
            map.remove("object");
            json!({"attachment":visible})
        }
        _ if method == "GET" && path.starts_with("/attachments/") => {
            let id = &path[13..];
            let attachment = tx
                .get("attachment", id)
                .await?
                .ok_or_else(|| BoardError::missing("attachment_not_found"))?;
            let mut readable = false;
            for message in tx.list("message").await? {
                if message["deleted"] != true
                    && message["attachments"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|a| a["id"] == id))
                    && crate::access::allowed(tx, p, text(&message, "channel")?, "read").await?
                {
                    readable = true;
                    break;
                }
            }
            if !readable && attachment["owner"] != p.id {
                return Err(BoardError::missing("attachment_not_found"));
            }
            attachment
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}

pub(crate) async fn tick(tx: &mut Tx<'_>) -> Result<(), BoardError> {
    for mut schedule in tx.list("schedule").await? {
        if schedule["state"] != "pending"
            || schedule["next"].as_str().is_none_or(|next| {
                next > tx
                    .at
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .as_str()
            })
        {
            continue;
        }
        let id = text(&schedule, "id")?.to_string();
        if schedule["every"].as_i64().is_some_and(|every| every <= 0) {
            schedule["state"] = "canceled".into();
            schedule["next"] = Value::Null;
            tx.put("schedule", &id, &schedule).await?;
            continue;
        }
        let principal: Principal = serde_json::from_value(schedule["_principal"].clone())
            .map_err(|_| BoardError::new(500, "storage_error", "invalid schedule identity"))?;
        let mut body = schedule["_body"].clone();
        body["id"] = format!("schedule:{id}:{}", schedule["sent"]).into();
        let previous_source = tx
            .source
            .replace(engage_core::WriteSource::Schedule { id: id.clone() });
        body["_schedule_n"] = schedule["sent"].clone();
        let sent = messages::post(tx, &principal, &body).await;
        tx.source = previous_source;
        match sent {
            Ok(message) => {
                schedule["last"] = message["id"].clone();
                schedule["sent"] = (schedule["sent"].as_i64().unwrap_or(0) + 1).into();
            }
            Err(error)
                if (400..500).contains(&error.status) && !matches!(error.status, 408 | 429) =>
            {
                schedule["state"] = "canceled".into();
                schedule["next"] = Value::Null;
                tx.put("schedule", &id, &schedule).await?;
                continue;
            }
            Err(error) => return Err(error),
        }
        if let Some(every) = schedule["every"].as_i64() {
            if every <= 0 {
                schedule["state"] = "canceled".into();
                schedule["next"] = Value::Null;
                tx.put("schedule", &id, &schedule).await?;
                continue;
            }
            let first = DateTime::parse_from_rfc3339(text(&schedule, "first")?)
                .map_err(|_| BoardError::new(500, "storage_error", "invalid stored schedule"))?
                .with_timezone(&Utc);
            let elapsed = (tx.at - first).num_milliseconds();
            let slot = elapsed / every + 1;
            let next = first + Duration::milliseconds(slot * every);
            let next = next.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let done = schedule["times"]
                .as_i64()
                .is_some_and(|times| schedule["sent"].as_i64().unwrap_or(0) >= times)
                || schedule["until"]
                    .as_str()
                    .is_some_and(|until| next.as_str() > until);
            if done {
                schedule["state"] = "done".into();
                schedule["next"] = Value::Null;
            } else {
                schedule["next"] = next.into();
            }
        } else {
            schedule["state"] = "done".into();
            schedule["next"] = Value::Null;
        }
        tx.put("schedule", &id, &schedule).await?;
    }
    Ok(())
}
