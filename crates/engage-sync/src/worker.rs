//! Durable inbox and watcher consumers. The board owns leases and delivery receipts.
use crate::{ProviderPost, Registry, SyncError, TargetKind};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

mod helpers;
use helpers::{
    BatchResult, clamp_text, derived_id, keyed_id, lifecycle_text, operation_uuid, provider_text,
    required, safe_error, segment,
};

/// A board transport bound to trusted service principals, not platform payload claims.
#[async_trait]
pub trait BoardApi: Send + Sync {
    async fn request(
        &self,
        principal: &str,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Value,
    ) -> Result<Value, SyncError>;
    async fn upload(
        &self,
        _principal: &str,
        _name: &str,
        _mime_type: &str,
        _bytes: Vec<u8>,
    ) -> Result<Value, SyncError> {
        Err(SyncError::Unsupported("board attachments"))
    }
    async fn download(&self, _principal: &str, _id: &str) -> Result<Vec<u8>, SyncError> {
        Err(SyncError::Unsupported("board attachments"))
    }
}

/// Runs at-least-once deliveries; logical posts use deterministic idempotency identities.
#[derive(Clone, Debug)]
pub struct SyncWorker {
    registry: Registry,
}
impl SyncWorker {
    pub fn new(registry: Registry) -> Self {
        Self { registry }
    }
    /// Registers sync subscriptions once at startup. Existing identical registrations are retained.
    pub async fn register(&self, api: &dyn BoardApi) -> Result<(), SyncError> {
        let mut principals = BTreeMap::<&str, Vec<Value>>::new();
        for (key, entry) in &self.registry.entries {
            principals
                .entry(&entry.principal)
                .or_default()
                .push(json!({"key":key,"platform":entry.provider.platform()}));
        }
        for (principal, sync) in principals {
            api.request(
                principal,
                "POST",
                "/watchers",
                &[],
                json!({"watch":{},"sync":sync,"url":null}),
            )
            .await?;
        }
        Ok(())
    }
    /// Processes bounded batches without holding SQL locks during platform calls.
    pub async fn run_once(&self, api: &dyn BoardApi, limit: usize) -> Result<usize, SyncError> {
        let mut batch = BatchResult::default();
        let mut principals = BTreeSet::new();
        for (key, entry) in &self.registry.entries {
            batch.record(self.inbox(api, key, limit).await);
            principals.insert(entry.principal.as_str());
        }
        for principal in principals {
            batch.record(self.matches(api, principal, limit).await);
        }
        batch.finish()
    }
    /// Queues a page of authenticated history through the same durable inbox as webhooks.
    /// Call again with `next` while `more` is true; re-queuing a page is idempotent.
    pub async fn backfill(
        &self,
        api: &dyn BoardApi,
        key: &str,
        target: &str,
        limit: usize,
        after: Option<&str>,
    ) -> Result<crate::BackfillProgress, SyncError> {
        let entry = self
            .registry
            .entries
            .get(key)
            .ok_or_else(|| SyncError::UnknownProvider(key.into()))?;
        let page = entry.provider.backfill(key, target, limit, after).await?;
        let queued = page.incoming.len();
        for incoming in page.incoming.chunks(50) {
            let events = incoming
                .iter()
                .map(|event| json!({"id":event.id,"event":event.event,"raw":event.raw}))
                .collect::<Vec<_>>();
            api.request(
                &entry.principal,
                "POST",
                &format!("/inbox/{key}"),
                &[],
                json!({"events":events}),
            )
            .await?;
        }
        Ok(crate::BackfillProgress {
            queued,
            more: page.more,
            next: page.next,
        })
    }
    /// Pulls one leased event per external thread, importing before acknowledging it.
    pub async fn inbox(
        &self,
        api: &dyn BoardApi,
        key: &str,
        limit: usize,
    ) -> Result<usize, SyncError> {
        let entry = self
            .registry
            .entries
            .get(key)
            .ok_or_else(|| SyncError::UnknownProvider(key.into()))?;
        let page = api
            .request(
                &entry.principal,
                "GET",
                &format!("/inbox/{key}/next"),
                &[("limit".into(), limit.clamp(1, 50).to_string())],
                Value::Null,
            )
            .await?;
        let mut batch = BatchResult::default();
        for item in page["items"].as_array().into_iter().flatten() {
            let id = required(item, "id")?;
            let lease = required(item, "lease")?;
            let outcome = self.import(api, key, item).await;
            let mut ack = json!({"lease":lease});
            if let Err(error) = &outcome {
                ack["error"] = json!(safe_error(error));
            }
            let acknowledged = api
                .request(
                    &entry.principal,
                    "POST",
                    &format!("/inbox/{key}/{}/ack", segment(id)),
                    &[],
                    ack,
                )
                .await;
            batch.record(acknowledged.and(outcome).map(|()| 1));
        }
        batch.finish()
    }
    async fn board_channel(&self, key: &str, event: &Value) -> Result<Option<String>, SyncError> {
        let entry = &self.registry.entries[key];
        let external = required(event, "channel")?;
        let raw = external
            .strip_prefix(&format!("{key}:"))
            .unwrap_or(external);
        if let Some(channel) = entry.channels.get(raw) {
            return Ok(Some(channel.clone()));
        }
        // Named channels are resolved through the provider; IDs take the direct path above.
        for (target, channel) in &entry.channels {
            let target = entry.provider.resolve_target(target).await?;
            if target.channel == raw
                || entry.provider.platform() == "github"
                    && (raw == format!("{}:pulls", target.channel)
                        || raw == format!("{}:issues", target.channel))
            {
                return Ok(Some(channel.clone()));
            }
        }
        Ok(None)
    }
    async fn import(&self, api: &dyn BoardApi, key: &str, item: &Value) -> Result<(), SyncError> {
        let entry = &self.registry.entries[key];
        let principal = &entry.principal;
        let event = &item["event"];
        let Some(channel) = self.board_channel(key, event).await? else {
            return Ok(());
        };
        let thread = required(event, "thread")?;
        let kind = required(event, "kind")?;
        let mut author = event["author"].clone();
        if author.is_null() {
            author = json!({"id":format!("{key}:user:unknown"),"handle":"unknown","name":"unknown","avatar":null,"isBot":false});
        }
        if let Some(id) = author["id"].as_str() {
            author["id"] = json!(keyed_id(key, entry.provider.platform(), id));
        }
        let find = api
            .request(
                principal,
                "GET",
                "/sync/find",
                &[("thread".into(), thread.into())],
                Value::Null,
            )
            .await?;
        let mut root = find["thread"].as_str().map(str::to_owned);
        let origin = json!({"channel":event["channel"],"thread":thread,"subthread":event["subthread"],"message":event["message"],"context":event["context"]});
        let source = json!({"key":key,"channel":event["channel"],"thread":thread});
        let from = json!(key);
        let operation = derived_id(key, required(item, "id")?);
        if kind == "lifecycle" {
            if root.is_none() && event["createRoot"] == true {
                let text = lifecycle_text(event);
                let result=api.request(principal,"POST","/sync/imports",&[],json!({"op":"post","author":author,"from":from,"channel":channel,"text":text,"labels":event["labels"],"origin":{"channel":event["channel"],"thread":thread,"subthread":null,"message":format!("lifecycle:{}",required(item,"id")?),"context":null},"source":source,"id":operation})).await?;
                root = result["message"]
                    .as_str()
                    .or(result["message"]["id"].as_str())
                    .map(str::to_owned);
            }
            if let Some(root) = root {
                let current = api
                    .request(
                        principal,
                        "GET",
                        &format!("/messages/{}", segment(&root)),
                        &[],
                        Value::Null,
                    )
                    .await?;
                let labels = current["message"]["labels"].as_object();
                let desired = event["labels"].as_object();
                let mut unset: Vec<Value> = event["unset"].as_array().cloned().unwrap_or_default();
                for (label, _) in labels.into_iter().flatten() {
                    if !desired.is_some_and(|values| values.contains_key(label))
                        && event["owns"].as_array().is_some_and(|prefixes| {
                            prefixes.iter().any(|prefix| {
                                prefix
                                    .as_str()
                                    .is_some_and(|prefix| label.starts_with(prefix))
                            })
                        })
                    {
                        unset.push(json!(label));
                    }
                }
                api.request(principal,"POST","/sync/imports",&[],json!({"op":"labels","author":author,"from":from,"message":root,"set":event["labels"],"unset":unset})).await?;
                if event["retitled"] == true
                    && current["message"]["origin"]["thread"].as_str() == Some(thread)
                {
                    api.request(principal,"POST","/sync/imports",&[],json!({"op":"edit","author":author,"from":from,"message":root,"text":lifecycle_text(event),"source":source})).await?;
                }
                if let Some(reply) = event["reply"].as_str().filter(|s| !s.is_empty()) {
                    api.request(principal,"POST","/sync/imports",&[],json!({"op":"post","author":author,"from":from,"re":root,"text":reply,"origin":null,"source":source,"id":format!("{operation}-reply")})).await?;
                }
            }
            return Ok(());
        }
        if kind == "message" {
            // A crash after posting but before inbox acknowledgment must not post again,
            // nor download/upload the same attachments a second time.
            let existing = api
                .request(
                    principal,
                    "GET",
                    "/sync/find",
                    &[
                        ("thread".into(), thread.into()),
                        ("message".into(), required(event, "message")?.into()),
                    ],
                    Value::Null,
                )
                .await?;
            if existing["message"].as_str().is_some() {
                return Ok(());
            }
            if root.is_none() && event["isRoot"] != true {
                let native = thread.strip_prefix(&format!("{key}:")).unwrap_or(thread);
                let fetched = entry.provider.fetch_root(native).await?;
                if let Some(mut fetched) = fetched {
                    if let Some(id) = fetched.author["id"].as_str() {
                        fetched.author["id"] = json!(keyed_id(key, entry.provider.platform(), id));
                    }
                    let root_message = entry
                        .provider
                        .root_message_id(native)
                        .unwrap_or_else(|| format!("root:{operation}"));
                    let result=api.request(principal,"POST","/sync/imports",&[],json!({"op":"post","author":fetched.author,"from":from,"channel":channel,"text":fetched.text,"labels":fetched.labels,"origin":{"channel":event["channel"],"thread":thread,"subthread":null,"message":root_message,"context":null},"source":source,"id":derived_id(key,thread)})).await?;
                    root = result["message"]
                        .as_str()
                        .or(result["message"]["id"].as_str())
                        .map(str::to_owned);
                } else {
                    return Err(SyncError::Payload(
                        "platform thread root is unavailable".into(),
                    ));
                }
            }
            let mut text = event["text"].as_str().unwrap_or_default().replace('\0', "");
            let mut attachments = Vec::new();
            for file in event["files"].as_array().into_iter().flatten() {
                let name = file["name"].as_str().unwrap_or("attachment");
                match entry.provider.fetch_file(file).await {
                    Ok(Some(bytes)) => match api
                        .upload(
                            principal,
                            name,
                            file["mimeType"]
                                .as_str()
                                .unwrap_or("application/octet-stream"),
                            bytes,
                        )
                        .await
                    {
                        Ok(value) => {
                            let id = value["attachment"]["id"]
                                .as_str()
                                .or(value["id"].as_str())
                                .ok_or_else(|| {
                                    SyncError::Payload("upload returned no attachment ID".into())
                                })?;
                            attachments.push(json!(id));
                        }
                        Err(error) => {
                            text.push_str(&format!(
                                "\n\n📎 {name} didn't come over: {}",
                                safe_error(&error)
                            ));
                        }
                    },
                    Ok(None) => {
                        text.push_str(&format!("\n\n📎 {name} didn't come over: file unavailable"))
                    }
                    Err(error) => text.push_str(&format!(
                        "\n\n📎 {name} didn't come over: {}",
                        safe_error(&error)
                    )),
                }
            }
            if text.trim().is_empty() && attachments.is_empty() {
                text = "(no text)".into();
            }
            text = clamp_text(&text);
            if key != entry.provider.platform() {
                text = text.replace(
                    &format!("@[{}:", entry.provider.platform()),
                    &format!("@[{key}:"),
                );
            }
            let mut input = json!({"op":"post","author":author,"from":from,"text":text,"attachments":attachments,"origin":origin,"source":source,"id":operation});
            if let Some(root) = root {
                input["re"] = json!(root);
            } else {
                input["channel"] = json!(channel);
            }
            if event.get("links").is_some() {
                input["links"] = json!(self.import_links(api, key, event).await?);
            }
            if let Some(mentioned) = event["mentioned"].as_array() {
                input["mentioned"] = json!(
                    mentioned
                        .iter()
                        .map(|author| {
                            let mut author = author.clone();
                            if let Some(id) = author["id"].as_str() {
                                author["id"] = json!(keyed_id(key, entry.provider.platform(), id));
                            }
                            author
                        })
                        .collect::<Vec<_>>()
                );
            }
            api.request(principal, "POST", "/sync/imports", &[], input)
                .await?;
        } else {
            let mut input = json!({"author":author,"from":from,"origin":{"thread":thread,"message":event["message"]}});
            match kind {
                "edited" => {
                    if event["linksOnly"] == true {
                        input["op"] = json!("links");
                        input["links"] = json!(self.import_links(api, key, event).await?);
                    } else {
                        input["op"] = json!("edit");
                        input["text"] =
                            json!(clamp_text(event["text"].as_str().unwrap_or_default()));
                        input["source"] = source;
                    }
                }
                "deleted" => input["op"] = json!("delete"),
                "reaction" => {
                    input["op"] = json!("react");
                    input["emoji"] = event["emoji"].clone();
                    input["remove"] = json!(event["added"] != true);
                }
                _ => return Err(SyncError::Payload("unknown external event kind".into())),
            }
            api.request(principal, "POST", "/sync/imports", &[], input)
                .await?;
        }
        Ok(())
    }
    async fn import_links(
        &self,
        api: &dyn BoardApi,
        key: &str,
        event: &Value,
    ) -> Result<Vec<Value>, SyncError> {
        let entry = &self.registry.entries[key];
        let mut links = Vec::new();
        for link in event["links"].as_array().into_iter().flatten().take(10) {
            let mut snapshot = link["snapshot"].clone();
            let mut unavailable = Value::Null;
            if snapshot.is_null() {
                match entry
                    .provider
                    .fetch_link(
                        link,
                        &json!({"thread":event["thread"],"message":event["message"]}),
                    )
                    .await
                {
                    Ok(fetched) => {
                        snapshot = fetched["snapshot"].clone();
                        unavailable = fetched["unavailable"].clone();
                    }
                    Err(error) => unavailable = json!(safe_error(&error)),
                }
            }
            let found = api
                .request(
                    &entry.principal,
                    "GET",
                    "/sync/find",
                    &[
                        ("thread".into(), required(link, "thread")?.into()),
                        ("message".into(), required(link, "message")?.into()),
                    ],
                    Value::Null,
                )
                .await?;
            links.push(json!({"url":link["url"],"message":found["message"],"snapshot":snapshot,"unavailable":unavailable}));
        }
        Ok(links)
    }
    async fn matches(
        &self,
        api: &dyn BoardApi,
        principal: &str,
        limit: usize,
    ) -> Result<usize, SyncError> {
        let page = api
            .request(
                principal,
                "GET",
                "/watchers/matches",
                &[("limit".into(), limit.clamp(1, 50).to_string())],
                Value::Null,
            )
            .await?;
        let mut batch = BatchResult::default();
        for item in page["matches"].as_array().into_iter().flatten() {
            // A sync consumer must not acknowledge another application's watcher.
            if item["watcher"] != "@sync" {
                continue;
            }
            let outcome = self.deliver(api, principal, &item["event"]).await;
            let mut ack = json!({"lease":item["lease"]});
            if let Err(error) = &outcome {
                ack["error"] = json!(safe_error(error));
            }
            let acknowledged = api
                .request(
                    principal,
                    "POST",
                    &format!("/watchers/matches/{}/ack", segment(required(item, "id")?)),
                    &[],
                    ack,
                )
                .await;
            batch.record(acknowledged.and(outcome).map(|()| 1));
        }
        batch.finish()
    }
    /// Delivers an already-leased board event; callers acknowledge only after this succeeds.
    pub async fn deliver(
        &self,
        api: &dyn BoardApi,
        principal: &str,
        event: &Value,
    ) -> Result<(), SyncError> {
        let message = &event["message"];
        if message.is_null() || message["noSync"] == true {
            return Ok(());
        }
        let id = required(message, "id")?;
        let thread = required(message, "thread")?;
        let kind = event["type"].as_str().unwrap_or_default();
        let sync = match api
            .request(
                principal,
                "GET",
                &format!("/sync/threads/{}", segment(thread)),
                &[],
                Value::Null,
            )
            .await
        {
            Ok(value) => value,
            Err(SyncError::Board { status: 404, .. }) => json!({"targets":[],"external":{}}),
            Err(error) => return Err(error),
        };
        let copies = api
            .request(
                principal,
                "GET",
                "/sync/copies",
                &[("message".into(), id.into())],
                Value::Null,
            )
            .await?;
        for (key, entry) in &self.registry.entries {
            let source = if kind == "message.posted" {
                message["from"].as_str()
            } else {
                event["payload"]["from"].as_str()
            };
            if entry.principal != principal
                || source == Some(key)
                || !entry.provider.outbound_enabled()
            {
                continue;
            }
            let mut targets: BTreeMap<String, TargetKind> = BTreeMap::new();
            for target in sync["targets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if let Some(raw) = target.strip_prefix(&format!("{key}:")) {
                    targets.insert(
                        raw.into(),
                        if raw.contains(':') {
                            TargetKind::Thread
                        } else {
                            TargetKind::Channel
                        },
                    );
                }
            }
            if message["re"].is_null()
                && kind == "message.posted"
                && entry.provider.capabilities().starts_threads
            {
                for (target, channel) in &entry.channels {
                    if message["channel"].as_str() == Some(channel) {
                        let resolved = entry.provider.resolve_target(target).await?;
                        targets.insert(resolved.target, resolved.kind);
                    }
                }
            }
            for (target, target_kind) in targets {
                let full_target = format!("{key}:{target}");
                if kind == "message.posted"
                    && message["origin"]["thread"].as_str() == Some(full_target.as_str())
                {
                    continue;
                }
                let copy = &copies["copies"][id][&full_target];
                let native_thread = sync["external"][&full_target]
                    .as_str()
                    .map(|value| value.strip_prefix(&format!("{key}:")).unwrap_or(value))
                    .unwrap_or(&target);
                // A board reply to an imported review/comment stays in that exact
                // platform subthread, while the board conversation remains grouped.
                let referenced = if let Some(reply_to) = message["replyTo"].as_str() {
                    Some(
                        api.request(
                            principal,
                            "GET",
                            &format!("/messages/{}", segment(reply_to)),
                            &[],
                            Value::Null,
                        )
                        .await?,
                    )
                } else {
                    None
                };
                let reply_origin = referenced
                    .as_ref()
                    .and_then(|view| {
                        let reply_to = message["replyTo"].as_str()?;
                        std::iter::once(&view["message"])
                            .chain(view["replies"].as_array().into_iter().flatten())
                            .find(|candidate| candidate["id"].as_str() == Some(reply_to))
                    })
                    .and_then(|message| message["origin"]["subthread"].as_str())
                    .and_then(|value| value.strip_prefix(&format!("{key}:")));
                let native_thread = reply_origin.unwrap_or(native_thread);
                let mut post = ProviderPost {
                    markdown: format!(
                        "**{}**: {}",
                        message["author"]["name"]
                            .as_str()
                            .or(message["by"]["actor"].as_str())
                            .unwrap_or("agent"),
                        provider_text(
                            key,
                            entry.provider.platform(),
                            &entry.principal,
                            message["text"].as_str().unwrap_or_default()
                        )
                    ),
                    external: !message["from"].is_null(),
                    operation_id: Some(operation_uuid(&format!(
                        "{}:{full_target}",
                        event["cursor"]
                    ))),
                    ..Default::default()
                };
                for attachment in message["attachments"].as_array().into_iter().flatten() {
                    if !entry.provider.capabilities().uploads_files {
                        post.markdown.push_str(&format!(
                            "\n\n📎 {} didn't come over: platform does not support file uploads",
                            attachment["name"].as_str().unwrap_or("attachment")
                        ));
                        continue;
                    }
                    let file_id = required(attachment, "id")?;
                    post.files.push(crate::ProviderFile {
                        data: api.download(principal, file_id).await?,
                        filename: attachment["name"].as_str().unwrap_or("attachment").into(),
                        mime_type: attachment["type"].as_str().map(str::to_owned),
                    });
                }
                match kind {
                    "message.posted" | "thread.synced" => {
                        if !copy.is_null() {
                            continue;
                        }
                        if message["re"].is_null() {
                            api.request(principal,"POST","/sync/receipts",&[],json!({"kind":"thread","message":id,"target":full_target,"targetKind":target_kind,"cause":"config"})).await?;
                        }
                        let delivery = entry
                            .provider
                            .post(
                                native_thread,
                                if native_thread != target || target_kind == TargetKind::Thread {
                                    TargetKind::Thread
                                } else {
                                    TargetKind::Channel
                                },
                                &post,
                            )
                            .await?;
                        api.request(principal,"POST","/sync/receipts",&[],json!({"kind":"message","message":id,"target":full_target,"external":{"thread":format!("{key}:{}",delivery.thread),"message":delivery.message},"files":delivery.files})).await?;
                    }
                    "message.edited" | "labels.changed" => {
                        if !entry.provider.capabilities().edits {
                            continue;
                        }
                        if let (Some(thread), Some(message)) =
                            (copy["thread"].as_str(), copy["message"].as_str())
                        {
                            entry
                                .provider
                                .edit(
                                    thread.strip_prefix(&format!("{key}:")).unwrap_or(thread),
                                    message,
                                    &post,
                                )
                                .await?;
                        }
                    }
                    "message.deleted" => {
                        if !entry.provider.capabilities().deletes {
                            continue;
                        }
                        if let (Some(thread), Some(message)) =
                            (copy["thread"].as_str(), copy["message"].as_str())
                        {
                            entry
                                .provider
                                .delete(
                                    thread.strip_prefix(&format!("{key}:")).unwrap_or(thread),
                                    message,
                                )
                                .await?;
                        }
                    }
                    "reaction.added" | "reaction.removed" => {
                        if !entry.provider.capabilities().reactions {
                            continue;
                        }
                        if let (Some(thread), Some(message)) =
                            (copy["thread"].as_str(), copy["message"].as_str())
                        {
                            entry
                                .provider
                                .react(
                                    thread.strip_prefix(&format!("{key}:")).unwrap_or(thread),
                                    message,
                                    event["payload"]["emoji"].as_str().unwrap_or_default(),
                                    kind == "reaction.added",
                                )
                                .await?;
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}
