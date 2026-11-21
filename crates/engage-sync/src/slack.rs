//! Slack Events API verification and bidirectional message delivery.
use crate::{
    Capabilities, CredentialResolver, Delivery, Incoming, Provider, ProviderPost, ResolvedTarget,
    SourceJoin, SourceRead, SyncError, TargetKind, WebhookRequest, WebhookResponse,
    http::{self, Api},
};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct Slack {
    pub(crate) api: Api,
    allow_join: bool,
}
impl Slack {
    pub fn new(credentials: impl CredentialResolver + 'static) -> Self {
        Self {
            api: Api::new(credentials, "https://slack.com/api"),
            allow_join: false,
        }
    }
    /// Channel joining changes workspace membership and must be explicitly enabled.
    pub fn with_join(mut self, allow: bool) -> Self {
        self.allow_join = allow;
        self
    }
    pub(crate) async fn call(&self, method: &str, body: Value) -> Result<Value, SyncError> {
        self.api.call(Method::POST, method, Some(&body), &[]).await
    }
    async fn channel(&self, target: &str) -> Result<String, SyncError> {
        let target = target.trim_start_matches('#');
        if target.starts_with('C') || target.starts_with('G') || target.starts_with('D') {
            http::validate_component(target)?;
            return Ok(target.into());
        }
        let mut cursor = String::new();
        loop {
            let page = self.call("conversations.list", json!({"types":"public_channel,private_channel","limit":200,"exclude_archived":true,"cursor":cursor})).await?;
            if let Some(channel) = page["channels"]
                .as_array()
                .and_then(|list| list.iter().find(|v| v["name"].as_str() == Some(target)))
            {
                return Ok(http::required(channel, "id")?.into());
            }
            cursor = page["response_metadata"]["next_cursor"]
                .as_str()
                .unwrap_or_default()
                .into();
            if cursor.is_empty() {
                return Err(SyncError::Platform {
                    status: 404,
                    code: "channel_not_found".into(),
                });
            }
        }
    }
    pub(crate) async fn normalized(
        &self,
        key: &str,
        envelope: &Value,
    ) -> Result<Vec<Incoming>, SyncError> {
        let event = &envelope["event"];
        let kind = event["type"].as_str().unwrap_or_default();
        let subtype = event["subtype"].as_str().unwrap_or_default();
        if matches!(subtype, "channel_join" | "channel_leave") {
            return Ok(vec![]);
        }
        let credentials = self.api.credentials.resolve().await?;
        let message = match subtype {
            "message_changed" => &event["message"],
            "message_deleted" => &event["previous_message"],
            _ => event,
        };
        let user = message["user"]
            .as_str()
            .or(event["user"].as_str())
            .unwrap_or("unknown");
        if credentials.bot_user_id.as_deref() == Some(user) {
            return Ok(vec![]);
        }
        let channel = event["channel"]
            .as_str()
            .or(event["item"]["channel"].as_str())
            .ok_or_else(|| SyncError::Payload("missing Slack channel".into()))?;
        let ts = message["ts"]
            .as_str()
            .or(event["deleted_ts"].as_str())
            .or(event["item"]["ts"].as_str())
            .ok_or_else(|| SyncError::Payload("missing Slack message timestamp".into()))?;
        let root = message["thread_ts"]
            .as_str()
            .or(event["item"]["thread_ts"].as_str())
            .unwrap_or(ts);
        let thread = format!("{key}:{channel}:{root}");
        let author = json!({"id":format!("slack:user:{user}"),"handle":user,"name":message["user_profile"]["display_name"].as_str().filter(|v| !v.is_empty()).unwrap_or(user),"avatar":message["user_profile"]["image_72"],"isBot":message.get("bot_id").is_some()});
        let mut normalized = json!({"channel":format!("{key}:{channel}"),"thread":thread,"subthread":null,"message":ts});
        let action = if kind == "reaction_added" || kind == "reaction_removed" {
            normalized["kind"] = json!("reaction");
            normalized["emoji"] = event["reaction"].clone();
            normalized["added"] = json!(kind == "reaction_added");
            normalized["author"] = author;
            kind
        } else if kind == "message" {
            if subtype == "message_deleted" {
                normalized["kind"] = json!("deleted");
            } else {
                normalized["kind"] = json!(if subtype == "message_changed" {
                    "edited"
                } else {
                    "message"
                });
                normalized["text"] =
                    json!(slack_text(message["text"].as_str().unwrap_or_default()));
                normalized["author"] = author;
                let links = crate::slack_links::links(
                    key,
                    message["text"].as_str().unwrap_or_default(),
                    &message["attachments"],
                );
                if !links.is_empty() {
                    normalized["links"] = json!(links);
                }
                if subtype != "message_changed" {
                    normalized["isRoot"] = json!(ts == root);
                    normalized["context"] = Value::Null;
                    normalized["files"] = Value::Array(message["files"].as_array().into_iter().flatten().take(20).map(|file| {
                        let mut result = json!({"type":if file["mimetype"].as_str().is_some_and(|s|s.starts_with("image/")){"image"}else{"file"},"fetchMetadata":{"id":http::string(file,"id")}});
                        for (source,dest) in [("name","name"),("mimetype","mimeType"),("size","size")] { if let Some(value)=file.get(source).filter(|value| !value.is_null()) {result[dest]=value.clone();} }
                        if let Some(url)=file["url_private_download"].as_str().or(file["url_private"].as_str()) {result["url"]=json!(url);}
                        result
                    }).collect());
                }
            }
            subtype
        } else {
            return Ok(vec![]);
        };
        let delivery = envelope["event_id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "{action}:{channel}:{ts}:{}",
                    event["event_ts"].as_str().unwrap_or(ts)
                )
            });
        Ok(vec![Incoming {
            id: delivery,
            thread,
            event: normalized,
            raw: event.clone(),
        }])
    }
    async fn upload(
        &self,
        channel: &str,
        thread: Option<&str>,
        body: &ProviderPost,
    ) -> Result<Delivery, SyncError> {
        let mut files = Vec::new();
        for file in &body.files {
            let response = self
                .call(
                    "files.getUploadURLExternal",
                    json!({"filename":file.filename,"length":file.data.len()}),
                )
                .await?;
            let upload = http::required(&response, "upload_url")?;
            let url = url::Url::parse(upload)
                .map_err(|_| SyncError::Payload("invalid Slack upload URL".into()))?;
            if url.scheme() != "https"
                || !url
                    .host_str()
                    .is_some_and(|h| h == "slack.com" || h.ends_with(".slack.com"))
            {
                return Err(SyncError::Payload("untrusted Slack upload host".into()));
            }
            let sent = self
                .api
                .client
                .post(url)
                .body(file.data.clone())
                .send()
                .await?;
            if !sent.status().is_success() {
                return Err(SyncError::Platform {
                    status: sent.status().as_u16(),
                    code: "upload_failed".into(),
                });
            }
            files.push(json!({"id":http::required(&response,"file_id")?,"title":file.filename}));
        }
        let mut input =
            json!({"files":files,"channel_id":channel,"initial_comment":outgoing_text(body)});
        if let Some(thread) = thread {
            input["thread_ts"] = json!(thread);
        }
        let result = self.call("files.completeUploadExternal", input).await?;
        let ids: Vec<String> = files
            .iter()
            .filter_map(|f| f["id"].as_str().map(str::to_owned))
            .collect();
        let first = result["files"]
            .as_array()
            .and_then(|v| v.first())
            .ok_or_else(|| SyncError::Payload("Slack upload returned no files".into()))?;
        let ts = first["shares"]["public"][channel]
            .as_array()
            .or(first["shares"]["private"][channel].as_array())
            .and_then(|v| v.first())
            .and_then(|v| v["ts"].as_str())
            .map(str::to_owned);
        let ts = match ts {
            Some(ts) => ts,
            None => {
                let info = self.call("files.info", json!({"file":ids.first()})).await?;
                info["file"]["shares"]["public"][channel]
                    .as_array()
                    .or(info["file"]["shares"]["private"][channel].as_array())
                    .and_then(|v| v.first())
                    .and_then(|v| v["ts"].as_str())
                    .ok_or_else(|| {
                        SyncError::Payload("Slack upload has no shared message timestamp".into())
                    })?
                    .to_owned()
            }
        };
        Ok(Delivery {
            thread: format!("{channel}:{}", thread.unwrap_or(&ts)),
            message: ts,
            files: ids,
        })
    }
}
fn split_thread(thread: &str) -> Result<(&str, &str), SyncError> {
    let (channel, ts) = thread
        .split_once(':')
        .ok_or_else(|| SyncError::Payload("Slack thread is CHANNEL:TIMESTAMP".into()))?;
    http::validate_component(channel)?;
    http::validate_component(ts)?;
    Ok((channel, ts))
}
pub(crate) fn slack_text(text: &str) -> String {
    let mentions =
        regex::Regex::new(r"<@([UW][A-Z0-9_]+)(?:\|[^<>]*)?>").expect("constant mention regex");
    let links =
        regex::Regex::new(r"<(https?://[^>|]+)(?:\|([^>]+))?>").expect("constant Slack link regex");
    let text = mentions.replace_all(text, "@[slack:user:$1]");
    links
        .replace_all(&text, |c: &regex::Captures<'_>| {
            c.get(2).map_or_else(
                || c[1].to_owned(),
                |label| format!("[{}]({})", label.as_str(), &c[1]),
            )
        })
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}
fn outgoing_text(body: &ProviderPost) -> String {
    let mentions = regex::Regex::new(r"@\[slack:user:([UW][A-Z0-9_]+)\]")
        .expect("constant Slack user mention regex");
    let text = mentions.replace_all(&body.markdown, "<@$1>").into_owned();
    if body.external {
        text.replace("<!", "<\u{2060}!")
    } else {
        text
    }
}

#[async_trait]
impl Provider for Slack {
    fn platform(&self) -> &'static str {
        "slack"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            starts_threads: true,
            uploads_files: true,
            edits: true,
            deletes: true,
            reactions: true,
            joins_channels: self.allow_join,
            typing: true,
        }
    }
    async fn receive(
        &self,
        key: &str,
        request: &WebhookRequest,
    ) -> Result<WebhookResponse, SyncError> {
        let timestamp = request
            .header("x-slack-request-timestamp")
            .ok_or(SyncError::Signature)?;
        let time = timestamp.parse::<i64>().map_err(|_| SyncError::Signature)?;
        if request.now_unix.abs_diff(time) > 300 {
            return Err(SyncError::Stale);
        }
        let credentials = self.api.credentials.resolve().await?;
        let mut signed = format!("v0:{timestamp}:").into_bytes();
        signed.extend_from_slice(&request.body);
        let signature = request
            .header("x-slack-signature")
            .and_then(|v| v.strip_prefix("v0="))
            .ok_or(SyncError::Signature)?;
        http::verify(credentials.webhook_secret.expose(), &signed, signature)?;
        let body = http::payload(&request.body)?;
        if body["type"] == "url_verification" {
            return Ok(WebhookResponse {
                status: 200,
                body: json!({"challenge":http::required(&body,"challenge")?}),
                incoming: vec![],
            });
        }
        if body["type"] != "event_callback" {
            return Ok(WebhookResponse::accepted(vec![]));
        }
        Ok(WebhookResponse::accepted(
            self.normalized(key, &body).await?,
        ))
    }
    async fn resolve_target(&self, target: &str) -> Result<ResolvedTarget, SyncError> {
        if let Some(link) = crate::slack_links::parse(target) {
            return Ok(ResolvedTarget {
                target: format!("{}:{}", link.channel, link.thread),
                channel: link.channel,
                kind: TargetKind::Thread,
            });
        }
        if let Some((channel, ts)) = target.split_once(':') {
            http::validate_component(ts)?;
            let channel = self.channel(channel).await?;
            return Ok(ResolvedTarget {
                target: format!("{channel}:{ts}"),
                channel,
                kind: TargetKind::Thread,
            });
        }
        let channel = self.channel(target).await?;
        Ok(ResolvedTarget {
            target: channel.clone(),
            channel,
            kind: TargetKind::Channel,
        })
    }
    async fn post(
        &self,
        target: &str,
        kind: TargetKind,
        body: &ProviderPost,
    ) -> Result<Delivery, SyncError> {
        let (channel, thread) = if kind == TargetKind::Thread {
            let (c, t) = split_thread(target)?;
            (c.to_owned(), Some(t.to_owned()))
        } else {
            (self.channel(target).await?, None)
        };
        if !body.files.is_empty() {
            return self.upload(&channel, thread.as_deref(), body).await;
        }
        let mut input = json!({"channel":channel,"markdown_text":outgoing_text(body),"unfurl_links":false,"unfurl_media":false});
        if let Some(thread) = &thread {
            input["thread_ts"] = json!(thread);
        }
        if let Some(name) = &body.sender_name {
            input["username"] = json!(name.chars().take(80).collect::<String>());
        }
        if let Some(avatar) = &body.sender_avatar {
            input["icon_url"] = json!(avatar);
        }
        if let Some(id) = &body.operation_id {
            input["client_msg_id"] = json!(id);
        }
        let response = self.call("chat.postMessage", input).await?;
        let ts = http::required(&response, "ts")?.to_owned();
        Ok(Delivery {
            thread: format!("{channel}:{}", thread.as_deref().unwrap_or(&ts)),
            message: ts,
            files: vec![],
        })
    }
    async fn edit(
        &self,
        thread: &str,
        message: &str,
        body: &ProviderPost,
    ) -> Result<(), SyncError> {
        let (c, _) = split_thread(thread)?;
        self.call(
            "chat.update",
            json!({"channel":c,"ts":message,"markdown_text":outgoing_text(body)}),
        )
        .await?;
        Ok(())
    }
    async fn delete(&self, thread: &str, message: &str) -> Result<(), SyncError> {
        let (c, _) = split_thread(thread)?;
        self.call("chat.delete", json!({"channel":c,"ts":message}))
            .await?;
        Ok(())
    }
    async fn react(
        &self,
        thread: &str,
        message: &str,
        emoji: &str,
        added: bool,
    ) -> Result<(), SyncError> {
        let (c, _) = split_thread(thread)?;
        self.call(
            if added {
                "reactions.add"
            } else {
                "reactions.remove"
            },
            json!({"channel":c,"timestamp":message,"name":emoji.trim_matches(':')}),
        )
        .await?;
        Ok(())
    }
    async fn typing(&self, thread: &str, status: Option<&str>) -> Result<(), SyncError> {
        let (channel, ts) = split_thread(thread)?;
        self.call(
            "assistant.threads.setStatus",
            json!({"channel_id":channel,"thread_ts":ts,"status":status.unwrap_or("is working…")}),
        )
        .await?;
        Ok(())
    }
    async fn backfill(
        &self,
        key: &str,
        target: &str,
        limit: usize,
        after: Option<&str>,
    ) -> Result<crate::Backfill, SyncError> {
        crate::backfill::slack(self, key, target, limit, after).await
    }
    async fn read(
        &self,
        target: &str,
        limit: usize,
        after: Option<&str>,
    ) -> Result<SourceRead, SyncError> {
        let resolved = self.resolve_target(target).await?;
        let limit = limit.clamp(1, 200);
        let mut input = json!({"channel":resolved.channel,"limit":limit});
        if let Some(after) = after {
            input["cursor"] = json!(after);
        }
        let method = if resolved.kind == TargetKind::Thread {
            input["ts"] = json!(split_thread(&resolved.target)?.1);
            "conversations.replies"
        } else {
            "conversations.history"
        };
        let result = self.call(method, input).await?;
        let mut messages = result["messages"].as_array().cloned().unwrap_or_default();
        if resolved.kind == TargetKind::Channel {
            messages.reverse();
        }
        let messages=messages.iter().map(|m|json!({"id":m["ts"],"thread":format!("{}:{}",resolved.channel,m["thread_ts"].as_str().or(m["ts"].as_str()).unwrap_or_default()),"author":m["user"].as_str().unwrap_or("unknown"),"text":slack_text(m["text"].as_str().unwrap_or_default()),"createdAt":null,"replies":m["reply_count"].as_u64().unwrap_or(0),"files":m["files"].as_array().map(|v|v.iter().map(|f|json!({"name":f["name"],"url":f["permalink"]})).collect::<Vec<_>>()).unwrap_or_default()})).collect();
        Ok(SourceRead {
            channel: resolved.channel,
            name: None,
            thread: if resolved.kind == TargetKind::Thread {
                Some(resolved.target)
            } else {
                None
            },
            messages,
            more: result["has_more"].as_bool().unwrap_or(false),
            next: result["response_metadata"]["next_cursor"]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(str::to_owned),
        })
    }
    async fn join(&self, target: &str) -> Result<SourceJoin, SyncError> {
        if !self.allow_join {
            return Err(SyncError::Unsupported("channel joining is disabled"));
        }
        let channel = self.channel(target).await?;
        let info = self
            .call("conversations.info", json!({"channel":channel}))
            .await?;
        if info["channel"]["is_private"].as_bool() == Some(true)
            || info["channel"]["is_im"].as_bool() == Some(true)
            || info["channel"]["is_mpim"].as_bool() == Some(true)
        {
            return Err(SyncError::Unsupported("only public channels may be joined"));
        }
        let joined = info["channel"]["is_member"].as_bool() != Some(true);
        if joined {
            self.call("conversations.join", json!({"channel":channel}))
                .await?;
        }
        Ok(SourceJoin {
            channel,
            name: info["channel"]["name"].as_str().map(str::to_owned),
            joined,
        })
    }
    async fn find_by_email(&self, email: &str) -> Result<Option<Value>, SyncError> {
        let result = self
            .call("users.lookupByEmail", json!({"email":email}))
            .await?;
        let user = &result["user"];
        let id = http::required(user, "id")?;
        Ok(Some(
            json!({"id":format!("slack:user:{id}"),"handle":user["name"].as_str().unwrap_or(id),"name":user["real_name"].as_str().unwrap_or(id),"avatar":user["profile"]["image_72"],"isBot":user["is_bot"].as_bool().unwrap_or(false)}),
        ))
    }
    async fn fetch_file(&self, file: &Value) -> Result<Option<Vec<u8>>, SyncError> {
        let Some(raw) = file["url"].as_str() else {
            return Ok(None);
        };
        let url =
            url::Url::parse(raw).map_err(|_| SyncError::Payload("invalid file URL".into()))?;
        if url.scheme() != "https"
            || !url
                .host_str()
                .is_some_and(|h| h == "files.slack.com" || h.ends_with(".slack-files.com"))
        {
            return Err(SyncError::Payload("untrusted Slack file host".into()));
        }
        let credentials = self.api.credentials.resolve().await?;
        let response = self
            .api
            .client
            .get(url)
            .bearer_auth(credentials.access_token.expose())
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(SyncError::Platform {
                status: response.status().as_u16(),
                code: "file_download_failed".into(),
            });
        }
        if response
            .content_length()
            .is_some_and(|size| size > 100 * 1024 * 1024)
        {
            return Err(SyncError::Payload("file exceeds 100 MiB".into()));
        }
        let bytes = http::bounded_bytes(response, 100 * 1024 * 1024).await?;
        if bytes.len() > 100 * 1024 * 1024 {
            return Err(SyncError::Payload("file exceeds 100 MiB".into()));
        }
        Ok(Some(bytes.to_vec()))
    }
    fn locate(&self, url: &str) -> Option<(String, Option<String>)> {
        let link = crate::slack_links::parse(url)?;
        Some((
            format!("{}:{}", link.channel, link.thread),
            Some(link.message),
        ))
    }
    async fn fetch_link(&self, link: &Value, linker: &Value) -> Result<Value, SyncError> {
        crate::slack_links::fetch(self, link, linker).await
    }
    async fn fetch_root(&self, thread: &str) -> Result<Option<crate::SyncRoot>, SyncError> {
        let (channel, ts) = split_thread(thread)?;
        let result = self
            .call(
                "conversations.replies",
                json!({"channel":channel,"ts":ts,"limit":1}),
            )
            .await?;
        let Some(message) = result["messages"]
            .as_array()
            .and_then(|messages| messages.first())
        else {
            return Ok(None);
        };
        let user = message["user"].as_str().unwrap_or("unknown");
        Ok(Some(crate::SyncRoot {
            text: slack_text(message["text"].as_str().unwrap_or_default()),
            author: json!({"id":format!("slack:user:{user}"),"handle":user,"name":user,"avatar":null,"isBot":message.get("bot_id").is_some()}),
            labels: Default::default(),
        }))
    }
    async fn url(&self, thread: &str, message: &str) -> Result<Option<String>, SyncError> {
        let (channel, _) = split_thread(thread)?;
        let result = self
            .call(
                "chat.getPermalink",
                json!({"channel":channel,"message_ts":message}),
            )
            .await?;
        Ok(result["permalink"].as_str().map(str::to_owned))
    }
    fn mention_by_id(&self, actor: &str) -> Option<String> {
        let user = actor.strip_prefix("slack:user:")?;
        if (user.starts_with('U') || user.starts_with('W'))
            && user
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            Some(format!("<@{user}>"))
        } else {
            None
        }
    }
}
