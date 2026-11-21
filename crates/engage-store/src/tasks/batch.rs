use super::{emit, get, hash, make_ready, validate_assignment, validate_needs};
use crate::{db::Tx, messages, need, public, text};
use engage_core::{BoardError, Principal};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) async fn create(
    tx: &mut Tx<'_>,
    p: &Principal,
    body: &Value,
) -> Result<Value, BoardError> {
    let plan = body["tasks"]
        .as_array()
        .ok_or_else(|| BoardError::invalid("tasks is required"))?;
    if plan.is_empty() || plan.len() > 100 {
        return Err(BoardError::new(
            400,
            "batch_too_large",
            "task batches contain 1 to 100 tasks",
        ));
    }
    let digest = body["request"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| hash(&body.to_string()));
    let client_id = body["id"].as_str().map(|id| format!("{}:{id}", p.actor()));
    if let Some(id) = &client_id
        && let Some(record) = tx.get("batch", id).await?
    {
        if record["request"] != digest {
            return Err(BoardError::conflict(
                "duplicate_id",
                "batch request differs",
            ));
        }
        let mut tasks = vec![];
        for id in record["tasks"].as_array().into_iter().flatten() {
            tasks.push(public(get(tx, p, id.as_str().unwrap_or("")).await?));
        }
        return Ok(json!({"tasks":tasks}));
    }
    let mut keys = BTreeMap::new();
    let mut made = vec![];
    for task in plan {
        let mut post = task.clone();
        post["channel"] = body["channel"].clone();
        post.as_object_mut()
            .ok_or_else(|| BoardError::invalid("invalid task"))?
            .remove("needs");
        post.as_object_mut().expect("task object").remove("key");
        need(tx, p, text(body, "channel")?, "label").await?;
        let message = messages::post(tx, p, &post).await?;
        if let Some(key) = task["key"].as_str()
            && keys
                .insert(key.to_string(), text(&message, "id")?.to_string())
                .is_some()
        {
            return Err(BoardError::new(400, "invalid_needs", "duplicate batch key"));
        }
        made.push(message);
    }
    // All roots exist before resolving forward references; validation sees the
    // complete proposed dependency graph and the transaction commits all or none.
    for (index, task) in plan.iter().enumerate() {
        let mut needs = vec![];
        for needed in task["needs"].as_array().into_iter().flatten() {
            let id = if let Some(id) = needed.as_str() {
                id.to_string()
            } else {
                keys.get(needed["key"].as_str().unwrap_or(""))
                    .cloned()
                    .ok_or_else(|| BoardError::new(400, "invalid_needs", "unknown batch key"))?
            };
            needs.push(id);
        }
        let assigned = task.get("for").cloned().unwrap_or(Value::Null);
        validate_assignment(p, &assigned)?;
        made[index]["task"] = json!({"state":"ready","owner":p.id,"by":p.actor(),"for":assigned,"needs":needs,"epoch":0,"claim":null,"blocked":null,"result":null,"reason":null});
        if !needs.is_empty() {
            made[index]["task"]["state"] = "waiting".into();
            made[index]["labels"]["board.task"] = "waiting".into();
        }
        let id = text(&made[index], "id")?.to_string();
        tx.put("message", &id, &made[index]).await?;
        tx.put("task", &id, &made[index]).await?;
    }
    for message in &made {
        validate_needs(tx, p, text(message, "id")?, &message["task"]["needs"]).await?;
    }
    for message in &mut made {
        let mut before = message.clone();
        before
            .as_object_mut()
            .expect("message object")
            .remove("task");
        before["labels"]
            .as_object_mut()
            .expect("labels object")
            .remove("board.task");
        emit(
            tx,
            p,
            message,
            &before,
            "task.created",
            json!({"needs":message["task"]["needs"],"for":message["task"]["for"]}),
        )
        .await?;
        make_ready(tx, p, message).await?;
    }
    if let Some(id) = &client_id {
        tx.put("batch",id,&json!({"request":digest,"tasks":made.iter().map(|m|m["id"].clone()).collect::<Vec<_>>()})).await?;
    }
    Ok(json!({"tasks":made.into_iter().map(public).collect::<Vec<_>>()}))
}
