//! Transactional Board engine for PostgreSQL and SQLite.
mod access;
mod db;
mod messages;
pub mod middleware;
mod registry;
mod tasks;
mod validation;
mod wake;
mod workers;

use chrono::{SecondsFormat, Utc};
use db::Tx;
use engage_core::{BoardError, Principal};
use middleware::Middleware;
use serde_json::{Value, json};
use sqlx::AnyPool;
use std::sync::Arc;

/// An independently hosted board. Clones share a pool; separate instances coordinate in SQL.
#[derive(Debug, Clone)]
pub struct Board {
    pool: AnyPool,
    middleware: Vec<Arc<dyn Middleware>>,
}

impl Board {
    /// Opens and migrates a PostgreSQL or SQLite board.
    ///
    /// # Errors
    /// Returns `storage_error` if the database cannot be opened or migrated.
    pub async fn connect(url: &str) -> Result<Self, BoardError> {
        let pool = db::connect(url).await?;
        let mut tx = Tx::begin(&pool).await?;
        if tx.get("internal", "task-state-index-v1").await?.is_none() {
            for task in tx.list("task").await? {
                let id = text(&task, "id")?;
                let state = task["task"]["state"].as_str().unwrap_or("ready");
                tx.put(&format!("task-state:{state}"), id, &task).await?;
            }
            tx.put("internal", "task-state-index-v1", &json!({"version":1}))
                .await?;
        }
        tx.commit().await?;
        Ok(Self {
            pool,
            middleware: vec![],
        })
    }

    /// Appends a native write middleware handler in registration order.
    #[must_use]
    pub fn with_middleware(mut self, handler: impl Middleware + 'static) -> Self {
        self.middleware.push(Arc::new(handler));
        self
    }

    /// Appends a shared native write middleware handler.
    #[must_use]
    pub fn with_shared_middleware(mut self, handler: Arc<dyn Middleware>) -> Self {
        self.middleware.push(handler);
        self
    }

    /// Executes one Board protocol operation under one consistent transactional snapshot.
    ///
    /// # Errors
    /// Returns the operation's protocol error without committing partial changes.
    pub async fn request(
        &self,
        p: &Principal,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Value,
    ) -> Result<Value, BoardError> {
        let path = if path.len() > 1 {
            path.trim_end_matches('/')
        } else {
            path
        };
        validation::request(method, path, query, &body)?;
        let mut tx = Tx::begin(&self.pool).await?;
        tx.middleware = self.middleware.clone();
        let mut known = actor(p);
        known["id"] = p.actor().into();
        tx.put("actor", &p.actor(), &known).await?;
        tasks::settle(&mut tx).await?;
        let value = if path == "/" && method == "GET" {
            json!({"version":"1","url":"","audience":"engage","team_id":null,"project_id":null,"sync":[]})
        } else if path == "/me" && method == "GET" {
            let view = actor(p);
            json!({"actor":p.actor(),"principal":p.id,"kind":view["kind"],"handle":view["handle"],"name":view["name"],"avatar":view["avatar"],"profile":p.profile,"via":view.get("via").cloned().unwrap_or(Value::Null),"expiresAt":null,"can":access::summary(&mut tx,p).await?})
        } else if let Some(v) = access::route(&mut tx, p, method, path, &body, query).await? {
            v
        } else if let Some(v) = registry::route(&mut tx, p, method, path, &body, query).await? {
            v
        } else if let Some(v) = tasks::route(&mut tx, p, method, path, &body, query).await? {
            v
        } else if let Some(v) = workers::route(&mut tx, p, method, path, &body, query).await? {
            v
        } else if let Some(v) = messages::route(&mut tx, p, method, path, &body, query).await? {
            v
        } else {
            return Err(BoardError::missing("route_not_found"));
        };
        tx.commit().await?;
        Ok(value)
    }

    /// Recovers expired work and sends due scheduled posts. Safe on every server instance.
    ///
    /// # Errors
    /// Returns `storage_error` when durable work cannot be read or committed.
    pub async fn tick(&self) -> Result<(), BoardError> {
        let mut tx = Tx::begin(&self.pool).await?;
        tx.middleware = self.middleware.clone();
        tasks::settle(&mut tx).await?;
        registry::tick(&mut tx).await?;
        tx.commit().await
    }
}

pub(crate) fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
pub(crate) fn q<'a>(query: &'a [(String, String)], key: &str) -> Option<&'a str> {
    query
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}
pub(crate) fn n(query: &[(String, String)], key: &str, default: usize) -> usize {
    q(query, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
        .min(500)
}
pub(crate) fn text<'a>(body: &'a Value, key: &str) -> Result<&'a str, BoardError> {
    body[key]
        .as_str()
        .ok_or_else(|| BoardError::invalid(format!("{key} must be a string")))
}
pub(crate) fn by(p: &Principal) -> Value {
    json!({"actor":p.actor(),"principal":p.id,"profile":p.profile})
}
pub(crate) fn actor(p: &Principal) -> Value {
    let mut view = json!({"kind":if p.profile.is_some(){"agent"}else{"person"},"handle":p.profile.as_ref().map_or_else(||p.handle.clone(),|profile|format!("{}/{profile}",p.handle)),"name":p.facts["_profile"]["name"].as_str().or(p.profile.as_deref()).unwrap_or(&p.name),"avatar":p.facts["_profile"]["avatar"].as_str().filter(|url|url.starts_with("https://")),"verified":true});
    if p.profile.is_some() {
        view["via"] = json!({"name":p.name,"environment":null});
    }
    view
}
pub(crate) fn public(mut message: Value) -> Value {
    if let Some(map) = message.as_object_mut() {
        map.remove("_claim_token");
    }
    message
}
pub(crate) async fn need(
    tx: &mut Tx<'_>,
    p: &Principal,
    channel: &str,
    permission: &str,
) -> Result<(), BoardError> {
    if access::allowed(tx, p, channel, permission).await? {
        Ok(())
    } else {
        Err(BoardError::new(403, "forbidden", "permission denied"))
    }
}
pub(crate) async fn read_message(
    tx: &mut Tx<'_>,
    p: &Principal,
    id: &str,
) -> Result<Value, BoardError> {
    let message = tx
        .get("message", id)
        .await?
        .ok_or_else(|| BoardError::missing("message_not_found"))?;
    if !access::allowed(tx, p, message["channel"].as_str().unwrap_or(""), "read").await? {
        return Err(BoardError::missing("message_not_found"));
    }
    Ok(message)
}
pub(crate) async fn changed(
    tx: &mut Tx<'_>,
    p: &Principal,
    message: &mut Value,
    before: Option<&Value>,
    kind: &str,
    payload: Value,
) -> Result<(), BoardError> {
    message["version"] = (tx.position + 1).into();
    if matches!(
        kind,
        "message.posted" | "message.edited" | "message.deleted"
    ) {
        message["revision"] = (tx.position + 1).into();
    }
    let mut event_actor = tx
        .get("actor", &p.actor())
        .await?
        .unwrap_or_else(|| actor(p));
    event_actor["id"] = p.actor().into();
    let mut event = json!({"type":kind,"createdAt":now(),"payload":payload,"message":public(message.clone()),"actor":event_actor});
    tx.event(&mut event, before).await?;
    let id = text(message, "id")?.to_string();
    tx.put("message", &id, message).await?;
    if message["task"].is_object() {
        tx.put("task", &id, message).await?;
        let state = message["task"]["state"].as_str().unwrap_or("ready");
        if let Some(previous) = before
            .and_then(|m| m["task"]["state"].as_str())
            .filter(|old| *old != state)
        {
            tx.remove(&format!("task-state:{previous}"), &id).await?;
        }
        tx.put(&format!("task-state:{state}"), &id, message).await?;
    }
    workers::match_event(tx, &event, p, before).await?;
    Ok(())
}
