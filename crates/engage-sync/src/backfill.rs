//! Historical reads enter through the same normalization contract as verified webhooks.
use crate::{Backfill, GitHub, Linear, Provider, Slack, SyncError, TargetKind, http};
use reqwest::Method;
use serde_json::{Value, json};

pub(crate) async fn slack(
    provider: &Slack,
    key: &str,
    target: &str,
    limit: usize,
    after: Option<&str>,
) -> Result<Backfill, SyncError> {
    let target = provider.resolve_target(target).await?;
    let mut request = json!({"channel":target.channel,"limit":limit.clamp(1,200)});
    if let Some(after) = after {
        request["cursor"] = json!(after);
    }
    let method = if target.kind == TargetKind::Thread {
        request["ts"] = json!(
            target
                .target
                .split_once(':')
                .ok_or_else(|| SyncError::Payload("invalid Slack thread".into()))?
                .1
        );
        "conversations.replies"
    } else {
        "conversations.history"
    };
    let page = provider.call(method, request).await?;
    let mut messages = page["messages"].as_array().cloned().unwrap_or_default();
    if target.kind == TargetKind::Channel {
        messages.reverse();
    }
    let mut incoming = Vec::new();
    for mut message in messages {
        let ts = http::required(&message, "ts")?.to_owned();
        message["type"] = json!("message");
        message["channel"] = json!(target.channel);
        let envelope = json!({"type":"event_callback","event_id":format!("backfill:{}:{ts}",target.channel),"event":message});
        incoming.extend(provider.normalized(key, &envelope).await?);
    }
    Ok(Backfill {
        incoming,
        more: page["has_more"] == true,
        next: page["response_metadata"]["next_cursor"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
    })
}

pub(crate) async fn github(
    provider: &GitHub,
    key: &str,
    target: &str,
    limit: usize,
    after: Option<&str>,
) -> Result<Backfill, SyncError> {
    let target = provider.resolve_target(target).await?;
    let limit = limit.clamp(1, 100).to_string();
    let page = after.unwrap_or("1");
    let page_number = page
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0 && *n < u64::MAX)
        .ok_or_else(|| SyncError::Payload("invalid GitHub backfill page".into()))?;
    let mut incoming = Vec::new();
    let more;
    if target.kind == TargetKind::Channel {
        let list = provider
            .call(
                Method::GET,
                &format!("repos/{}/issues", target.target),
                None,
                &[("state", "all"), ("per_page", &limit), ("page", page)],
            )
            .await?;
        let items = list.as_array().cloned().unwrap_or_default();
        more = items.len() == limit.parse::<usize>().unwrap_or(100);
        for item in items {
            incoming.extend(github_root(provider, key, &target.target, &item));
        }
    } else {
        let (repo, tail) = target
            .target
            .split_once(':')
            .ok_or_else(|| SyncError::Payload("invalid GitHub thread".into()))?;
        let issue = tail.starts_with("issue:");
        let number = tail.trim_start_matches("issue:");
        if page_number == 1 {
            let item = provider
                .call(
                    Method::GET,
                    &format!(
                        "repos/{repo}/{}/{number}",
                        if issue { "issues" } else { "pulls" }
                    ),
                    None,
                    &[],
                )
                .await?;
            incoming.extend(github_root(provider, key, repo, &item));
        }
        let bot = provider.api.credentials.resolve().await?.bot_user_id;
        let mut has_more = false;
        for (kind, path) in std::iter::once((
            "issue_comment",
            format!("repos/{repo}/issues/{number}/comments"),
        ))
        .chain((!issue).then(|| {
            (
                "pull_request_review_comment",
                format!("repos/{repo}/pulls/{number}/comments"),
            )
        })) {
            let result = provider
                .call(
                    Method::GET,
                    &path,
                    None,
                    &[("per_page", &limit), ("page", page)],
                )
                .await?;
            let comments = result.as_array().cloned().unwrap_or_default();
            has_more |= comments.len() == limit.parse::<usize>().unwrap_or(100);
            for comment in comments {
                let delivery = format!("backfill:comment:{}", http::string(&comment, "id"));
                let mut item = json!({"action":"created","repository":{"full_name":repo},"comment":comment,"issue":{"number":number.parse::<u64>().unwrap_or(0)},"pull_request":{"number":number.parse::<u64>().unwrap_or(0)}});
                if !issue {
                    item["issue"]["pull_request"] = json!({});
                }
                incoming.extend(provider.comment(key, kind, &item, &delivery, bot.as_deref()));
            }
        }
        more = has_more;
    }
    Ok(Backfill {
        incoming,
        more,
        next: if more {
            Some((page_number + 1).to_string())
        } else {
            None
        },
    })
}
fn github_root(provider: &GitHub, key: &str, repo: &str, item: &Value) -> Vec<crate::Incoming> {
    let issue = item.get("pull_request").is_none()
        && item.get("merged").is_none()
        && item.get("head").is_none();
    let kind = if issue { "issues" } else { "pull_request" };
    let delivery = format!("backfill:{kind}:{}", http::string(item, "number"));
    let mut payload =
        json!({"action":"opened","repository":{"full_name":repo},"sender":item["user"]});
    payload[if issue { "issue" } else { "pull_request" }] = item.clone();
    let mut events = provider.lifecycle(key, kind, &payload, &delivery);
    for incoming in &mut events {
        incoming.event["labels"]["state"] = json!(if item["merged"] == true {
            "merged"
        } else if item["state"] == "closed" {
            "closed"
        } else if item["draft"] == true {
            "draft"
        } else {
            "open"
        });
    }
    events
}

pub(crate) async fn linear(
    provider: &Linear,
    key: &str,
    target: &str,
    limit: usize,
    after: Option<&str>,
) -> Result<Backfill, SyncError> {
    let limit = limit.clamp(1, 100);
    let issue_target = (target.contains(':') && !target.contains("://"))
        || regex::Regex::new(r"(?:^|/issue/)[A-Za-z0-9_]+-[0-9]+")
            .expect("constant Linear target regex")
            .is_match(target);
    let mut incoming = Vec::new();
    if issue_target {
        let id = if target.contains(':') && !target.contains("://") {
            target.split(':').nth(1).unwrap_or(target)
        } else {
            target
                .split("/issue/")
                .nth(1)
                .map(|v| v.split('/').next().unwrap_or(v))
                .unwrap_or(target)
        };
        let query = "query($id:String!,$first:Int!,$after:String){issue(id:$id){id identifier title description url creator{id name displayName url avatarUrl} state{name type} priority priorityLabel assignee{id name displayName url} project{name} labels{nodes{name}} comments(first:$first,after:$after){nodes{id body parent{id} user{id name displayName url avatarUrl}}pageInfo{hasNextPage endCursor}}}}";
        let result = provider
            .graphql(query, json!({"id":id,"first":limit,"after":after}))
            .await?;
        let issue = &result["issue"];
        if issue.is_null() {
            return Err(SyncError::Platform {
                status: 404,
                code: "issue_not_found".into(),
            });
        }
        if after.is_none() {
            incoming.extend(linear_root(provider, key, issue).await?);
        }
        for comment in issue["comments"]["nodes"].as_array().into_iter().flatten() {
            let payload = json!({"type":"Comment","action":"create","url":issue["url"],"data":{"id":comment["id"],"issueId":issue["id"],"issue":{"id":issue["id"],"identifier":issue["identifier"]},"body":comment["body"],"parentId":comment["parent"]["id"],"user":comment["user"]}});
            incoming.extend(
                provider
                    .normalize(
                        key,
                        &payload,
                        &format!("backfill:comment:{}", http::string(comment, "id")),
                    )
                    .await?,
            );
        }
        Ok(Backfill {
            incoming,
            more: issue["comments"]["pageInfo"]["hasNextPage"] == true,
            next: issue["comments"]["pageInfo"]["endCursor"]
                .as_str()
                .map(str::to_owned),
        })
    } else {
        http::validate_component(target)?;
        let query = "query($key:String!,$first:Int!,$after:String){issues(filter:{team:{key:{eq:$key}}},first:$first,after:$after){nodes{id identifier title description url creator{id name displayName url avatarUrl} state{name type} priority priorityLabel assignee{id name displayName url} project{name} labels{nodes{name}}}pageInfo{hasNextPage endCursor}}}";
        let result = provider
            .graphql(
                query,
                json!({"key":target.to_uppercase(),"first":limit,"after":after}),
            )
            .await?;
        for issue in result["issues"]["nodes"].as_array().into_iter().flatten() {
            incoming.extend(linear_root(provider, key, issue).await?);
        }
        Ok(Backfill {
            incoming,
            more: result["issues"]["pageInfo"]["hasNextPage"] == true,
            next: result["issues"]["pageInfo"]["endCursor"]
                .as_str()
                .map(str::to_owned),
        })
    }
}
async fn linear_root(
    provider: &Linear,
    key: &str,
    issue: &Value,
) -> Result<Vec<crate::Incoming>, SyncError> {
    let mut data = issue.clone();
    data["labels"] = issue["labels"]["nodes"].clone();
    provider
        .normalize(
            key,
            &json!({"type":"Issue","action":"create","actor":issue["creator"],"data":data}),
            &format!("backfill:issue:{}", http::string(issue, "id")),
        )
        .await
}
