//! GitHub PR/issue lifecycle and comment synchronization. Outbound writes are opt-in.
use crate::{
    Capabilities, CredentialResolver, Delivery, Incoming, Provider, ProviderPost, ResolvedTarget,
    SourceRead, SyncError, TargetKind, WebhookRequest, WebhookResponse,
    http::{self, Api},
};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct GitHub {
    pub(crate) api: Api,
    outbound: bool,
}
impl GitHub {
    pub fn new(credentials: impl CredentialResolver + 'static) -> Self {
        Self {
            api: Api::new(credentials, "https://api.github.com"),
            outbound: false,
        }
    }
    /// Explicitly permit existing-thread writes. New issues and PRs are never created.
    pub fn with_outbound(mut self, enabled: bool) -> Self {
        self.outbound = enabled;
        self
    }
    fn writable(&self) -> Result<(), SyncError> {
        if self.outbound {
            Ok(())
        } else {
            Err(SyncError::Unsupported("GitHub outbound sync is disabled"))
        }
    }
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        query: &[(&str, &str)],
    ) -> Result<Value, SyncError> {
        self.api.call(method, path, body, query).await
    }
    pub(crate) fn lifecycle(
        &self,
        key: &str,
        name: &str,
        body: &Value,
        delivery: &str,
    ) -> Vec<Incoming> {
        let Some(repo) = body["repository"]["full_name"].as_str() else {
            return vec![];
        };
        let action = body["action"].as_str().unwrap_or_default();
        if name == "workflow_run" {
            return workflow_events(key, repo, body, delivery);
        }
        let issue = name == "issues";
        let item = if issue {
            &body["issue"]
        } else {
            &body["pull_request"]
        };
        let Some(number) = item["number"].as_u64() else {
            return vec![];
        };
        let title = if issue {
            format!(
                "Issue #{number}: {}",
                item["title"].as_str().unwrap_or_default()
            )
        } else {
            format!(
                "PR #{number}: {}",
                item["title"].as_str().unwrap_or_default()
            )
        };
        let sender = body["sender"]["login"].as_str().unwrap_or("someone");
        let thread = format!("{key}:{repo}:{}{number}", if issue { "issue:" } else { "" });
        let mut labels = serde_json::Map::new();
        let mut unset = Vec::<String>::new();
        let mut owns = Vec::<String>::new();
        let mut create_root = false;
        let mut retitled = false;
        let mut reply = Value::Null;
        let event_type = if name == "pull_request_review" {
            if action == "dismissed" {
                unset.push("review".into());
                "review.dismissed".into()
            } else if action == "submitted" {
                let state = body["review"]["state"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase();
                let verdict = match state.as_str() {
                    "approved" => Some("approved"),
                    "changes_requested" => Some("changes requested"),
                    _ => None,
                };
                if verdict.is_some() {
                    labels.insert("review".into(), json!(state));
                }
                let text = [
                    verdict.map(|v| format!("{v} by {sender}")),
                    body["review"]["body"]
                        .as_str()
                        .filter(|v| !v.trim().is_empty())
                        .map(str::to_owned),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n\n");
                if text.is_empty() {
                    return vec![];
                }
                reply = json!(text);
                "review.submitted".into()
            } else {
                return vec![];
            }
        } else {
            let prefix = if issue { "issue" } else { "pr" };
            match action {
                "opened" | "reopened" | "ready_for_review" => {
                    create_root = true;
                    labels.insert(prefix.into(), json!(format!("{repo}#{number}")));
                    labels.insert(
                        "state".into(),
                        json!(if !issue && item["draft"] == true {
                            "draft"
                        } else {
                            "open"
                        }),
                    );
                    if let Some(sha) = item["head"]["sha"].as_str() {
                        labels.insert("head".into(), json!(sha));
                    }
                    if action == "reopened" {
                        reply = json!(format!("reopened by {sender}"));
                    }
                }
                "closed" => {
                    let state = if item["merged"] == true {
                        "merged"
                    } else {
                        "closed"
                    };
                    labels.insert("state".into(), json!(state));
                    reply = json!(format!("{state} by {sender}"));
                }
                "converted_to_draft" => {
                    labels.insert("state".into(), json!("draft"));
                }
                "edited" => {
                    if body["changes"].get("title").is_none() {
                        return vec![];
                    }
                    retitled = true;
                }
                "synchronize" => {
                    let Some(sha) = item["head"]["sha"].as_str() else {
                        return vec![];
                    };
                    labels.insert("head".into(), json!(sha));
                    owns.push(format!(
                        "ci.{}-{number}.",
                        http::string(&body["repository"], "id")
                    ));
                }
                "labeled" | "unlabeled" | "assigned" | "unassigned" => {}
                _ => return vec![],
            }
            format!(
                "{prefix}.{}",
                match action {
                    "ready_for_review" => "ready",
                    "converted_to_draft" => "draft",
                    "synchronize" => "synchronized",
                    other => other,
                }
            )
        };
        add_tags(item, &mut labels, &mut owns);
        let mut event = json!({"kind":"lifecycle","channel":format!("{key}:{repo}:{}",if issue{"issues"}else{"pulls"}),"thread":thread,"type":event_type,"title":title,"url":item["html_url"],"labels":labels,"unset":unset,"owns":owns,"reply":reply,"createRoot":create_root,"author":author(&body["sender"])});
        if retitled {
            event["retitled"] = json!(true);
        }
        vec![Incoming {
            id: format!("lifecycle:{delivery}"),
            thread,
            event,
            raw: body.clone(),
        }]
    }
    pub(crate) fn comment(
        &self,
        key: &str,
        name: &str,
        body: &Value,
        delivery: &str,
        bot: Option<&str>,
    ) -> Vec<Incoming> {
        let Some(repo) = body["repository"]["full_name"].as_str() else {
            return vec![];
        };
        let comment = &body["comment"];
        let id = http::string(comment, "id");
        if id.is_empty() {
            return vec![];
        }
        if bot.is_some_and(|bot| http::string(&comment["user"], "id") == bot) {
            return vec![];
        }
        let review = name == "pull_request_review_comment";
        let issue = !review && body["issue"].get("pull_request").is_none();
        let number = if review {
            body["pull_request"]["number"].as_u64()
        } else {
            body["issue"]["number"].as_u64()
        };
        let Some(number) = number else { return vec![] };
        let thread = format!("{key}:{repo}:{}{number}", if issue { "issue:" } else { "" });
        let subthread = if review {
            json!(format!(
                "{thread}:rc:{}",
                comment
                    .get("in_reply_to_id")
                    .filter(|v| !v.is_null())
                    .unwrap_or(&comment["id"])
            ))
        } else {
            Value::Null
        };
        let action = body["action"].as_str().unwrap_or_default();
        let mut event = json!({"channel":format!("{key}:{repo}:{}",if issue{"issues"}else{"pulls"}),"thread":thread,"subthread":subthread,"message":id});
        match action {
            "created" | "edited" => {
                event["kind"] = json!(if action == "created" {
                    "message"
                } else {
                    "edited"
                });
                event["text"] = json!(comment["body"].as_str().unwrap_or_default().trim());
                event["author"] = author(&comment["user"]);
                if action == "created" {
                    event["isRoot"] = json!(false);
                    event["context"] = comment["path"]
                        .as_str()
                        .map(|path| match comment["line"].as_u64() {
                            Some(line) => json!(format!("{path}:{line}")),
                            None => json!(path),
                        })
                        .unwrap_or(Value::Null);
                }
            }
            "deleted" => event["kind"] = json!("deleted"),
            _ => return vec![],
        }
        vec![Incoming {
            id: format!("{action}:{delivery}"),
            thread,
            event,
            raw: body.clone(),
        }]
    }
}
fn author(user: &Value) -> Value {
    let id = http::string(user, "id");
    let login = user["login"].as_str().unwrap_or("unknown");
    json!({"id":format!("github:user:{}",if id.is_empty(){"unknown"}else{&id}),"handle":login,"name":login,"avatar":user["avatar_url"],"isBot":user["type"]=="Bot"})
}
fn add_tags(item: &Value, labels: &mut serde_json::Map<String, Value>, owns: &mut Vec<String>) {
    for (array, prefix, field) in [
        ("labels", "tag", "name"),
        ("assignees", "assignee", "login"),
    ] {
        if let Some(values) = item[array].as_array() {
            owns.push(format!("{prefix}."));
            for value in values {
                if let Some(name) = value[field].as_str()
                    && let Some((key, value)) = http::tag(prefix, name)
                {
                    labels.insert(key, value);
                }
            }
        }
    }
}
fn workflow_events(key: &str, repo: &str, body: &Value, delivery: &str) -> Vec<Incoming> {
    if body["action"] != "completed" {
        return vec![];
    }
    let run = &body["workflow_run"];
    let Some(sha) = run["head_sha"].as_str() else {
        return vec![];
    };
    let Some(conclusion) = run["conclusion"].as_str() else {
        return vec![];
    };
    let failed = matches!(conclusion, "failure" | "timed_out" | "startup_failure");
    if !failed && conclusion != "success" {
        return vec![];
    }
    let file = run["path"]
        .as_str()
        .unwrap_or_default()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim_end_matches(".yml")
        .trim_end_matches(".yaml");
    let slug = http::slug(file);
    if slug.is_empty() {
        return vec![];
    }
    run["pull_requests"].as_array().into_iter().flatten().filter_map(|pr|{
        let number=pr["number"].as_u64()?;if pr["head"]["sha"].as_str()!=Some(sha){return None;}
        let label=format!("ci.{}-{number}.{slug}",http::string(&body["repository"],"id"));
        let failed=matches!(conclusion,"failure"|"timed_out"|"startup_failure");
        let thread=format!("{key}:{repo}:{number}");
        let mut labels=serde_json::Map::new();let mut unset=Vec::new();if failed{labels.insert(label.clone(),json!(conclusion));}else{unset.push(label);}
        let event=json!({"kind":"lifecycle","channel":format!("{key}:{repo}:pulls"),"thread":thread,"type":if failed {"ci.failed"} else {"ci.passed"},"title":format!("PR #{number}"),"url":run["html_url"],"labels":labels,"unset":unset,"reply":if failed{json!(format!("{}: {conclusion}\n{}",run["name"].as_str().unwrap_or("CI"),run["html_url"].as_str().unwrap_or_default()))}else{Value::Null},"createRoot":false,"author":author(&json!({"id":41898282,"login":"github-actions[bot]","type":"Bot"}))});
        Some(Incoming{id:format!("lifecycle:{delivery}:{number}"),thread,event,raw:body.clone()})
    }).collect()
}
struct Thread<'a> {
    repo: &'a str,
    number: &'a str,
    issue: bool,
    review: Option<&'a str>,
}
fn parse_thread(target: &str) -> Result<Thread<'_>, SyncError> {
    let (repo, tail) = target.split_once(':').ok_or_else(|| {
        SyncError::Payload("GitHub target is owner/repo:NUMBER or owner/repo:issue:NUMBER".into())
    })?;
    validate_repo(repo)?;
    let (issue, number) = tail
        .strip_prefix("issue:")
        .map_or((false, tail), |n| (true, n));
    let (number, review) = number
        .split_once(":rc:")
        .map_or((number, None), |(number, review)| (number, Some(review)));
    if review.is_some_and(|review| review.is_empty() || !review.chars().all(|c| c.is_ascii_digit()))
        || issue && review.is_some()
    {
        return Err(SyncError::Payload("invalid GitHub review thread".into()));
    }
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return Err(SyncError::Payload("invalid GitHub issue number".into()));
    }
    Ok(Thread {
        repo,
        number,
        issue,
        review,
    })
}
fn validate_repo(repo: &str) -> Result<(), SyncError> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| SyncError::Payload("GitHub repository is owner/name".into()))?;
    http::validate_component(owner)?;
    http::validate_component(name)
}
fn source_message(comment: &Value, thread: &str) -> Value {
    json!({"id":http::string(comment,"id"),"thread":thread,"author":comment["user"]["login"].as_str().unwrap_or("unknown"),"text":comment["body"].as_str().unwrap_or_default(),"createdAt":comment["created_at"],"replies":0,"files":[]})
}

#[async_trait]
impl Provider for GitHub {
    fn platform(&self) -> &'static str {
        "github"
    }
    fn outbound_enabled(&self) -> bool {
        self.outbound
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            edits: self.outbound,
            deletes: self.outbound,
            reactions: self.outbound,
            ..Default::default()
        }
    }
    async fn receive(
        &self,
        key: &str,
        request: &WebhookRequest,
    ) -> Result<WebhookResponse, SyncError> {
        let signature = request
            .header("x-hub-signature-256")
            .and_then(|v| v.strip_prefix("sha256="))
            .ok_or(SyncError::Signature)?;
        let credentials = self.api.credentials.resolve().await?;
        http::verify(
            credentials.webhook_secret.expose(),
            &request.body,
            signature,
        )?;
        let body = http::payload(&request.body)?;
        let name = request
            .header("x-github-event")
            .ok_or_else(|| SyncError::Payload("missing GitHub event type".into()))?;
        let digest = hex::encode(sha2::Sha256::digest(&request.body));
        let delivery = request.header("x-github-delivery").unwrap_or(&digest);
        let incoming = match name {
            "issue_comment" | "pull_request_review_comment" => self.comment(
                key,
                name,
                &body,
                delivery,
                credentials.bot_user_id.as_deref(),
            ),
            "pull_request" | "pull_request_review" | "issues" | "workflow_run" => {
                self.lifecycle(key, name, &body, delivery)
            }
            _ => vec![],
        };
        Ok(WebhookResponse::accepted(incoming))
    }
    async fn resolve_target(&self, target: &str) -> Result<ResolvedTarget, SyncError> {
        if target.contains(":rc:") {
            return Err(SyncError::Payload(
                "sync the PR, not an individual review thread".into(),
            ));
        }
        if target.contains(':') {
            let parsed = parse_thread(target)?;
            Ok(ResolvedTarget {
                target: target.into(),
                channel: format!(
                    "{}:{}",
                    parsed.repo,
                    if parsed.issue { "issues" } else { "pulls" }
                ),
                kind: TargetKind::Thread,
            })
        } else {
            validate_repo(target)?;
            Ok(ResolvedTarget {
                target: target.into(),
                channel: target.into(),
                kind: TargetKind::Channel,
            })
        }
    }
    async fn post(
        &self,
        target: &str,
        kind: TargetKind,
        body: &ProviderPost,
    ) -> Result<Delivery, SyncError> {
        self.writable()?;
        if kind != TargetKind::Thread {
            return Err(SyncError::Unsupported(
                "GitHub never creates issues or PRs from board roots",
            ));
        }
        if !body.files.is_empty() {
            return Err(SyncError::Unsupported("GitHub file uploads"));
        }
        let parsed = parse_thread(target)?;
        let response = self
            .call(
                Method::POST,
                &if let Some(review) = parsed.review {
                    format!(
                        "repos/{}/pulls/{}/comments/{review}/replies",
                        parsed.repo, parsed.number
                    )
                } else {
                    format!("repos/{}/issues/{}/comments", parsed.repo, parsed.number)
                },
                Some(&json!({"body":body.markdown})),
                &[],
            )
            .await?;
        Ok(Delivery {
            thread: target.into(),
            message: http::string(&response, "id"),
            files: vec![],
        })
    }
    async fn edit(
        &self,
        thread: &str,
        message: &str,
        body: &ProviderPost,
    ) -> Result<(), SyncError> {
        self.writable()?;
        let parsed = parse_thread(thread)?;
        http::validate_component(message)?;
        self.call(
            Method::PATCH,
            &format!(
                "repos/{}/{}/comments/{message}",
                parsed.repo,
                if parsed.review.is_some() {
                    "pulls"
                } else {
                    "issues"
                }
            ),
            Some(&json!({"body":body.markdown})),
            &[],
        )
        .await?;
        Ok(())
    }
    async fn delete(&self, thread: &str, message: &str) -> Result<(), SyncError> {
        self.writable()?;
        let parsed = parse_thread(thread)?;
        http::validate_component(message)?;
        self.call(
            Method::DELETE,
            &format!(
                "repos/{}/{}/comments/{message}",
                parsed.repo,
                if parsed.review.is_some() {
                    "pulls"
                } else {
                    "issues"
                }
            ),
            None,
            &[],
        )
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
        self.writable()?;
        let parsed = parse_thread(thread)?;
        let content = match emoji.trim_matches(':') {
            "thumbs_up" | "+1" => "+1",
            "thumbs_down" | "-1" => "-1",
            "laugh" => "laugh",
            "hooray" | "tada" => "hooray",
            "confused" => "confused",
            "heart" => "heart",
            "rocket" => "rocket",
            "eyes" => "eyes",
            _ => {
                return Err(SyncError::Unsupported(
                    "GitHub supports eight reaction types",
                ));
            }
        };
        let path = if message.chars().all(|c| c.is_ascii_digit()) && !message.is_empty() {
            format!(
                "repos/{}/{}/comments/{message}/reactions",
                parsed.repo,
                if parsed.review.is_some() {
                    "pulls"
                } else {
                    "issues"
                }
            )
        } else {
            format!("repos/{}/issues/{}/reactions", parsed.repo, parsed.number)
        };
        if added {
            self.call(Method::POST, &path, Some(&json!({"content":content})), &[])
                .await?;
        } else {
            let credentials = self.api.credentials.resolve().await?;
            let bot = credentials.bot_user_id.ok_or_else(|| {
                SyncError::Credentials(
                    "GitHub bot user ID is required to remove its reaction".into(),
                )
            })?;
            let list = self
                .call(
                    Method::GET,
                    &path,
                    None,
                    &[("content", content), ("per_page", "100")],
                )
                .await?;
            for reaction in list.as_array().into_iter().flatten() {
                if http::string(&reaction["user"], "id") == bot {
                    let id = http::string(reaction, "id");
                    http::validate_component(&id)?;
                    self.call(Method::DELETE, &format!("{path}/{id}"), None, &[])
                        .await?;
                }
            }
        }
        Ok(())
    }
    async fn backfill(
        &self,
        key: &str,
        target: &str,
        limit: usize,
        after: Option<&str>,
    ) -> Result<crate::Backfill, SyncError> {
        crate::backfill::github(self, key, target, limit, after).await
    }
    async fn read(
        &self,
        target: &str,
        limit: usize,
        after: Option<&str>,
    ) -> Result<SourceRead, SyncError> {
        let limit = limit.clamp(1, 100).to_string();
        let page = after.unwrap_or("1");
        let page_number = page
            .parse::<u64>()
            .ok()
            .filter(|number| *number > 0 && *number < u64::MAX)
            .ok_or_else(|| SyncError::Payload("invalid GitHub page".into()))?;
        if !page.chars().all(|c| c.is_ascii_digit()) || page.is_empty() {
            return Err(SyncError::Payload("invalid GitHub page".into()));
        }
        if target.contains(':') {
            let parsed = parse_thread(target)?;
            let root = self
                .call(
                    Method::GET,
                    &format!("repos/{}/issues/{}", parsed.repo, parsed.number),
                    None,
                    &[],
                )
                .await?;
            let comments = self
                .call(
                    Method::GET,
                    &if let Some(review) = parsed.review {
                        format!(
                            "repos/{}/pulls/{}/comments/{review}/replies",
                            parsed.repo, parsed.number
                        )
                    } else {
                        format!("repos/{}/issues/{}/comments", parsed.repo, parsed.number)
                    },
                    None,
                    &[("per_page", &limit), ("page", page)],
                )
                .await?;
            let list = comments.as_array().cloned().unwrap_or_default();
            let more = list.len() == limit.parse::<usize>().unwrap_or(100);
            let mut messages = Vec::new();
            if page == "1" {
                messages.push(json!({"id":"issue","thread":target,"author":root["user"]["login"].as_str().unwrap_or("unknown"),"text":format!("{}\n\n{}",root["title"].as_str().unwrap_or_default(),root["body"].as_str().unwrap_or_default()),"createdAt":root["created_at"],"replies":root["comments"].as_u64().unwrap_or(0),"files":[]}));
            }
            messages.extend(list.iter().map(|comment| source_message(comment, target)));
            Ok(SourceRead {
                channel: parsed.repo.into(),
                name: None,
                thread: Some(target.into()),
                messages,
                more,
                next: if more {
                    Some((page_number + 1).to_string())
                } else {
                    None
                },
            })
        } else {
            validate_repo(target)?;
            let response = self
                .call(
                    Method::GET,
                    &format!("repos/{target}/issues"),
                    None,
                    &[("per_page", &limit), ("page", page), ("state", "all")],
                )
                .await?;
            let list = response.as_array().cloned().unwrap_or_default();
            let more = list.len() == limit.parse::<usize>().unwrap_or(100);
            let messages=list.iter().map(|item|json!({"id":http::string(item,"number"),"thread":format!("{target}:{}{}",if item.get("pull_request").is_none(){"issue:"}else{""},http::string(item,"number")),"author":item["user"]["login"].as_str().unwrap_or("unknown"),"text":format!("{}\n\n{}",item["title"].as_str().unwrap_or_default(),item["body"].as_str().unwrap_or_default()),"createdAt":item["created_at"],"replies":item["comments"].as_u64().unwrap_or(0),"files":[]})).collect();
            Ok(SourceRead {
                channel: target.into(),
                name: None,
                thread: None,
                messages,
                more,
                next: if more {
                    Some((page_number + 1).to_string())
                } else {
                    None
                },
            })
        }
    }
    fn locate(&self, raw: &str) -> Option<(String, Option<String>)> {
        let url = url::Url::parse(raw).ok()?;
        if url.scheme() != "https" || url.host_str()? != "github.com" {
            return None;
        }
        let parts = url.path_segments()?.collect::<Vec<_>>();
        if parts.len() < 4
            || !matches!(parts[2], "pull" | "issues")
            || !parts[3].chars().all(|c| c.is_ascii_digit())
            || parts[3].is_empty()
        {
            return None;
        }
        let repo = format!("{}/{}", parts[0], parts[1]);
        validate_repo(&repo).ok()?;
        let message = url
            .fragment()
            .and_then(|fragment| {
                fragment
                    .strip_prefix("issuecomment-")
                    .or_else(|| fragment.strip_prefix("discussion_r"))
            })
            .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
            .map(str::to_owned);
        Some((
            format!(
                "{repo}:{}{}",
                if parts[2] == "issues" { "issue:" } else { "" },
                parts[3]
            ),
            message,
        ))
    }
    async fn fetch_root(&self, thread: &str) -> Result<Option<crate::SyncRoot>, SyncError> {
        let parsed = parse_thread(thread)?;
        let item = self
            .call(
                Method::GET,
                &format!(
                    "repos/{}/{}/{}",
                    parsed.repo,
                    if parsed.issue { "issues" } else { "pulls" },
                    parsed.number
                ),
                None,
                &[],
            )
            .await?;
        let mut labels = serde_json::Map::new();
        let mut owns = Vec::new();
        add_tags(&item, &mut labels, &mut owns);
        labels.insert(
            if parsed.issue { "issue" } else { "pr" }.into(),
            json!(format!("{}#{}", parsed.repo, parsed.number)),
        );
        labels.insert(
            "state".into(),
            json!(if item["merged"] == true {
                "merged"
            } else if item["state"] == "closed" {
                "closed"
            } else if item["draft"] == true {
                "draft"
            } else {
                "open"
            }),
        );
        Ok(Some(crate::SyncRoot {
            text: format!(
                "{} #{}: {}\n{}",
                if parsed.issue { "Issue" } else { "PR" },
                parsed.number,
                item["title"].as_str().unwrap_or_default(),
                item["html_url"].as_str().unwrap_or_default()
            ),
            author: author(&item["user"]),
            labels: labels
                .into_iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k, v.to_owned())))
                .collect(),
        }))
    }
    async fn url(&self, thread: &str, message: &str) -> Result<Option<String>, SyncError> {
        let parsed = parse_thread(thread)?;
        let mut url = format!(
            "https://github.com/{}/{}/{}",
            parsed.repo,
            if parsed.issue { "issues" } else { "pull" },
            parsed.number
        );
        if message.chars().all(|c| c.is_ascii_digit()) && !message.is_empty() {
            url.push_str(&format!(
                "#{}{message}",
                if parsed.review.is_some() {
                    "discussion_r"
                } else {
                    "issuecomment-"
                }
            ));
        }
        Ok(Some(url))
    }
}
use sha2::Digest;
