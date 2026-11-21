//! Linear issue/comment lifecycle synchronization via its signed webhooks and GraphQL API.
use crate::{
    Capabilities, CredentialResolver, Delivery, Incoming, Provider, ProviderPost, ResolvedTarget,
    SourceRead, SyncError, TargetKind, WebhookRequest, WebhookResponse,
    http::{self, Api},
};
use async_trait::async_trait;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct Linear {
    pub(crate) api: Api,
    api_key: bool,
}
impl Linear {
    pub fn new(credentials: impl CredentialResolver + 'static) -> Self {
        Self {
            api: Api::new(credentials, "https://api.linear.app/graphql"),
            api_key: false,
        }
    }
    /// Personal Linear API keys use a raw Authorization value instead of OAuth Bearer.
    pub fn with_api_key(mut self) -> Self {
        self.api_key = true;
        self
    }
    pub(crate) async fn graphql(&self, query: &str, variables: Value) -> Result<Value, SyncError> {
        let credentials = self.api.credentials.resolve().await?;
        let mut request = self
            .api
            .client
            .post(&self.api.base)
            .json(&json!({"query":query,"variables":variables}));
        request = if self.api_key {
            request.header("Authorization", credentials.access_token.expose())
        } else {
            request.bearer_auth(credentials.access_token.expose())
        };
        let result = http::json_response(request.send().await?).await?;
        if result["errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty())
        {
            return Err(SyncError::Platform {
                status: 200,
                code: "graphql_error".into(),
            });
        }
        result
            .get("data")
            .cloned()
            .ok_or_else(|| SyncError::Payload("Linear returned no data".into()))
    }
    pub(crate) async fn team(&self, key: &str) -> Result<String, SyncError> {
        http::validate_component(key)?;
        let result = self
            .graphql(
                "query($key:String!){teams(filter:{key:{eq:$key}}){nodes{id key}}}",
                json!({"key":key.to_uppercase()}),
            )
            .await?;
        result["teams"]["nodes"]
            .as_array()
            .and_then(|nodes| nodes.first())
            .and_then(|team| team["id"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| SyncError::Platform {
                status: 404,
                code: "channel_not_found".into(),
            })
    }
    pub(crate) async fn normalize(
        &self,
        key: &str,
        payload: &Value,
        delivery: &str,
    ) -> Result<Vec<Incoming>, SyncError> {
        let kind = payload["type"].as_str().unwrap_or_default();
        let action = payload["action"].as_str().unwrap_or_default();
        let data = &payload["data"];
        let credentials = self.api.credentials.resolve().await?;
        let actor = payload
            .get("actor")
            .filter(|v| !v.is_null())
            .unwrap_or(&data["user"]);
        let own = credentials
            .bot_user_id
            .as_deref()
            .is_some_and(|bot| actor["id"].as_str() == Some(bot));
        if kind == "Issue" {
            if !matches!(action, "create" | "update") {
                return Ok(vec![]);
            }
            let id = http::required(data, "id")?;
            let identifier = http::required(data, "identifier")?;
            let team = data["team"]["key"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| team_from(identifier))
                .ok_or_else(|| SyncError::Payload("missing Linear team".into()))?;
            let thread = format!("{key}:{team}:{id}");
            let state = data["state"]["type"]
                .as_str()
                .or(data["state"]["name"].as_str());
            let (mut labels, unset, owns) = properties(data);
            labels.insert("issue".into(), json!(identifier));
            if let Some(state) = state {
                labels.insert("state".into(), json!(state.to_lowercase()));
            }
            let create_root = if action == "create" {
                !own
            } else {
                !own && actor["type"] == "user"
                    && data["archivedAt"].is_null()
                    && !credentials
                        .bot_user_id
                        .as_deref()
                        .is_some_and(|bot| data["creatorId"].as_str() == Some(bot))
            };
            let mut event = json!({"kind":"lifecycle","channel":format!("{key}:{team}"),"thread":thread,"type":if action=="create"{"issue.created"}else{"issue.updated"},"title":format!("{identifier}: {}",data["title"].as_str().unwrap_or_default()),"url":data["url"].as_str().or(payload["url"].as_str()),"body":data["description"].as_str().filter(|v|!v.trim().is_empty()),"labels":labels,"unset":unset,"owns":owns,"reply":null,"createRoot":create_root,"author":author(actor,own)});
            if action == "update"
                && (payload["updatedFrom"].get("title").is_some()
                    || payload["updatedFrom"].get("description").is_some())
            {
                event["retitled"] = json!(true);
            }
            return Ok(vec![Incoming {
                id: format!("lifecycle:{delivery}"),
                thread,
                event,
                raw: payload.clone(),
            }]);
        }
        if kind != "Comment" || !matches!(action, "create" | "update" | "remove") {
            return Ok(vec![]);
        }
        if own {
            return Ok(vec![]);
        }
        let issue = data["issueId"]
            .as_str()
            .or(data["issue"]["id"].as_str())
            .ok_or_else(|| SyncError::Payload("missing Linear issue ID".into()))?;
        let team = team_from(payload["url"].as_str().unwrap_or_default())
            .or_else(|| team_from(data["issue"]["identifier"].as_str().unwrap_or_default()));
        let team = match team {
            Some(team) => team,
            None => {
                let result = self
                    .graphql(
                        "query($id:String!){issue(id:$id){identifier team{key}}}",
                        json!({"id":issue}),
                    )
                    .await?;
                result["issue"]["team"]["key"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| {
                        team_from(result["issue"]["identifier"].as_str().unwrap_or_default())
                    })
                    .ok_or_else(|| SyncError::Payload("Linear issue has no team".into()))?
            }
        };
        let id = http::required(data, "id")?;
        let root = data["parentId"].as_str().unwrap_or(id);
        let thread = format!("{key}:{team}:{issue}");
        let mut event = json!({"kind":match action{"create"=>"message","update"=>"edited",_=>"deleted"},"channel":format!("{key}:{team}"),"thread":thread,"subthread":format!("{thread}:c:{root}"),"message":id});
        if action != "remove" {
            event["text"] = json!(data["body"].as_str().unwrap_or_default().trim());
            event["author"] = author(&data["user"], false);
            if action == "create" {
                event["isRoot"] = json!(false);
                event["context"] = Value::Null;
            }
        }
        Ok(vec![Incoming {
            id: format!("{action}:{delivery}"),
            thread,
            event,
            raw: payload.clone(),
        }])
    }
}
fn team_from(value: &str) -> Option<String> {
    let regex = regex::Regex::new(r"(?:^|/issue/)([A-Za-z0-9_]+)-[0-9]+")
        .expect("constant Linear identifier regex");
    regex
        .captures(value)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_uppercase())
}
fn author(user: &Value, own: bool) -> Value {
    let id = user["id"].as_str().unwrap_or("unknown");
    let name = user["name"]
        .as_str()
        .or(user["displayName"].as_str())
        .unwrap_or("unknown");
    let handle = user["url"]
        .as_str()
        .and_then(|url| url.split("/profiles/").nth(1))
        .map(|s| s.split(['?', '#', '/']).next().unwrap_or(s))
        .or(user["displayName"].as_str())
        .unwrap_or(name);
    json!({"id":format!("linear:user:{id}"),"handle":handle,"name":name,"avatar":user["avatarUrl"],"isBot":own||user["type"].as_str().is_some_and(|t|t!="user")})
}
fn properties(data: &Value) -> (serde_json::Map<String, Value>, Vec<String>, Vec<String>) {
    let mut labels = serde_json::Map::new();
    let mut unset = Vec::new();
    let mut owns = Vec::new();
    if data["priority"] == 0 || data["priorityLabel"] == "No priority" {
        unset.push("priority".into());
    } else if let Some(priority) = data["priorityLabel"].as_str() {
        let slug = http::slug(priority);
        if !slug.is_empty() {
            labels.insert("priority".into(), json!(slug));
        }
    }
    if let Some(project) = data["project"]["name"].as_str() {
        labels.insert("project".into(), json!(http::slug(project)));
    } else if data["project"].is_null() && data["projectId"].is_null() {
        unset.push("project".into());
    }
    if let Some(tags) = data["labels"].as_array() {
        owns.push("tag.".into());
        for tag in tags {
            if let Some(name) = tag["name"].as_str() {
                let slug = http::slug(name);
                if !slug.is_empty() {
                    labels.insert(
                        format!("tag.{slug}"),
                        json!(name.chars().take(256).collect::<String>()),
                    );
                }
            }
        }
    }
    if data["assignee"].is_object() {
        owns.push("assignee.".into());
        let actor = author(&data["assignee"], false);
        let handle = actor["handle"].as_str().unwrap_or_default();
        let slug = http::slug(handle);
        if !slug.is_empty() {
            labels.insert(format!("assignee.{slug}"), json!(handle));
        }
    } else if data["assigneeId"].is_null() {
        owns.push("assignee.".into());
    }
    (labels, unset, owns)
}
fn parse_thread(target: &str) -> Result<(&str, &str, Option<&str>), SyncError> {
    let mut parts = target.split(':');
    let team = parts.next().unwrap_or_default();
    let issue = parts
        .next()
        .ok_or_else(|| SyncError::Payload("Linear thread is TEAM:ISSUE_ID".into()))?;
    http::validate_component(team)?;
    http::validate_component(issue)?;
    let comment = match parts.next() {
        None => None,
        Some("c") => Some(
            parts
                .next()
                .ok_or_else(|| SyncError::Payload("missing Linear comment ID".into()))?,
        ),
        _ => return Err(SyncError::Payload("invalid Linear thread".into())),
    };
    if parts.next().is_some() {
        return Err(SyncError::Payload("invalid Linear thread".into()));
    }
    if let Some(comment) = comment {
        http::validate_component(comment)?;
    }
    Ok((team, issue, comment))
}
fn mutation_success(result: &Value, name: &str) -> Result<(), SyncError> {
    if result[name]["success"] == true {
        Ok(())
    } else {
        Err(SyncError::Platform {
            status: 200,
            code: format!("{name}_failed"),
        })
    }
}

#[async_trait]
impl Provider for Linear {
    fn platform(&self) -> &'static str {
        "linear"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            starts_threads: true,
            edits: true,
            deletes: true,
            reactions: true,
            ..Default::default()
        }
    }
    async fn receive(
        &self,
        key: &str,
        request: &WebhookRequest,
    ) -> Result<WebhookResponse, SyncError> {
        let signature = request
            .header("linear-signature")
            .ok_or(SyncError::Signature)?;
        let credentials = self.api.credentials.resolve().await?;
        http::verify(
            credentials.webhook_secret.expose(),
            &request.body,
            signature,
        )?;
        let body = http::payload(&request.body)?;
        let timestamp = body["webhookTimestamp"]
            .as_i64()
            .ok_or_else(|| SyncError::Payload("missing webhook timestamp".into()))?;
        if request.now_unix.abs_diff(timestamp / 1000) > 60 {
            return Err(SyncError::Stale);
        }
        let digest = hex::encode(sha2::Sha256::digest(&request.body));
        let delivery = request.header("linear-delivery").unwrap_or(&digest);
        Ok(WebhookResponse::accepted(
            self.normalize(key, &body, delivery).await?,
        ))
    }
    async fn resolve_target(&self, target: &str) -> Result<ResolvedTarget, SyncError> {
        crate::linear_read::resolve(self, target).await
    }
    async fn post(
        &self,
        target: &str,
        kind: TargetKind,
        body: &ProviderPost,
    ) -> Result<Delivery, SyncError> {
        if !body.files.is_empty() {
            return Err(SyncError::Unsupported("Linear file uploads"));
        }
        if kind == TargetKind::Thread {
            let (_, issue, parent) = parse_thread(target)?;
            let mut input = json!({"issueId":issue,"body":body.markdown});
            if let Some(parent) = parent {
                input["parentId"] = json!(parent);
            }
            let result=self.graphql("mutation($input:CommentCreateInput!){commentCreate(input:$input){success comment{id}}}",json!({"input":input})).await?;
            mutation_success(&result, "commentCreate")?;
            Ok(Delivery {
                thread: target.into(),
                message: http::required(&result["commentCreate"]["comment"], "id")?.into(),
                files: vec![],
            })
        } else {
            let team = self.team(target).await?;
            let title = body
                .markdown
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("Untitled")
                .trim_start_matches(['#', '>', ' '])
                .replace("**", "")
                .replace('`', "")
                .chars()
                .take(255)
                .collect::<String>();
            let result=self.graphql("mutation($input:IssueCreateInput!){issueCreate(input:$input){success issue{id}}}",json!({"input":{"teamId":team,"title":title,"description":body.markdown}})).await?;
            mutation_success(&result, "issueCreate")?;
            Ok(Delivery {
                thread: format!(
                    "{}:{}",
                    target.to_uppercase(),
                    http::required(&result["issueCreate"]["issue"], "id")?
                ),
                message: "issue".into(),
                files: vec![],
            })
        }
    }
    async fn edit(
        &self,
        thread: &str,
        message: &str,
        body: &ProviderPost,
    ) -> Result<(), SyncError> {
        let (_, issue, _) = parse_thread(thread)?;
        if message == "issue" {
            let result=self.graphql("mutation($id:String!,$input:IssueUpdateInput!){issueUpdate(id:$id,input:$input){success}}",json!({"id":issue,"input":{"description":body.markdown}})).await?;
            mutation_success(&result, "issueUpdate")
        } else if message.contains(':') {
            Ok(())
        } else {
            http::validate_component(message)?;
            let result=self.graphql("mutation($id:String!,$input:CommentUpdateInput!){commentUpdate(id:$id,input:$input){success}}",json!({"id":message,"input":{"body":body.markdown}})).await?;
            mutation_success(&result, "commentUpdate")
        }
    }
    async fn delete(&self, _thread: &str, message: &str) -> Result<(), SyncError> {
        // Removing a board root never closes or deletes a Linear issue.
        if message == "issue" || message.contains(':') {
            return Ok(());
        }
        http::validate_component(message)?;
        let result = self
            .graphql(
                "mutation($id:String!){commentDelete(id:$id){success}}",
                json!({"id":message}),
            )
            .await?;
        mutation_success(&result, "commentDelete")
    }
    async fn react(
        &self,
        _thread: &str,
        message: &str,
        emoji: &str,
        added: bool,
    ) -> Result<(), SyncError> {
        if message == "issue" || message.contains(':') || !added {
            return Ok(());
        }
        http::validate_component(message)?;
        let result = self
            .graphql(
                "mutation($input:ReactionCreateInput!){reactionCreate(input:$input){success}}",
                json!({"input":{"commentId":message,"emoji":emoji}}),
            )
            .await?;
        mutation_success(&result, "reactionCreate")
    }
    async fn fetch_root(&self, thread: &str) -> Result<Option<crate::SyncRoot>, SyncError> {
        let (_, issue, _) = parse_thread(thread)?;
        let result = self.graphql("query($id:String!){issue(id:$id){id identifier title description url priority priorityLabel state{name type} creator{id name displayName url avatarUrl} assignee{id name displayName url} labels{nodes{name}} project{name}}}",json!({"id":issue})).await?;
        let mut item = result["issue"].clone();
        if item.is_null() {
            return Ok(None);
        }
        item["labels"] = item["labels"]["nodes"].clone();
        let (mut labels, _, _) = properties(&item);
        labels.insert("issue".into(), item["identifier"].clone());
        if let Some(state) = item["state"]["type"]
            .as_str()
            .or(item["state"]["name"].as_str())
        {
            labels.insert("state".into(), json!(state.to_lowercase()));
        }
        Ok(Some(crate::SyncRoot {
            text: format!(
                "{}: {}\n{}\n\n{}",
                item["identifier"].as_str().unwrap_or_default(),
                item["title"].as_str().unwrap_or_default(),
                item["url"].as_str().unwrap_or_default(),
                item["description"].as_str().unwrap_or_default()
            ),
            author: author(&item["creator"], false),
            labels: labels
                .into_iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k, v.to_owned())))
                .collect(),
        }))
    }
    async fn backfill(
        &self,
        key: &str,
        target: &str,
        limit: usize,
        after: Option<&str>,
    ) -> Result<crate::Backfill, SyncError> {
        crate::backfill::linear(self, key, target, limit, after).await
    }
    async fn read(
        &self,
        target: &str,
        limit: usize,
        after: Option<&str>,
    ) -> Result<SourceRead, SyncError> {
        crate::linear_read::read(self, target, limit, after).await
    }
}
use sha2::Digest;
