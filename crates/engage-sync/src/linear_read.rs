//! Linear's public source-read and target resolution behavior.
use crate::{Linear, ResolvedTarget, SourceRead, SyncError, TargetKind};
use serde_json::{Value, json};

const FIELDS: &str = "id identifier title description url createdAt priority priorityLabel state{name} assignee{name displayName url} creator{name displayName url} project{name} labels(first:10){nodes{name}}";
fn team(value: &str) -> Option<String> {
    let regex =
        regex::Regex::new(r"(?:^|/issue/)([A-Z0-9_]+)-[0-9]+").expect("constant team regex");
    regex
        .captures(&value.to_uppercase())
        .and_then(|m| m.get(1))
        .map(|m| m.as_str().to_owned())
}
fn handle(user: &Value) -> String {
    user["url"]
        .as_str()
        .and_then(|url| url.split("/profiles/").nth(1))
        .map(|part| part.split(['?', '#', '/']).next().unwrap_or(part))
        .or(user["displayName"].as_str())
        .or(user["name"].as_str())
        .unwrap_or("unknown")
        .to_owned()
}
fn issue_message(issue: &Value, limit: Option<usize>) -> Value {
    let mut lines = vec![format!(
        "{}: {}",
        issue["identifier"].as_str().unwrap_or_default(),
        issue["title"].as_str().unwrap_or_default()
    )];
    let properties = [
        issue["state"]["name"].as_str().map(str::to_owned),
        issue["priorityLabel"]
            .as_str()
            .filter(|p| *p != "No priority")
            .map(str::to_owned),
        issue["assignee"]
            .is_object()
            .then(|| format!("@{}", handle(&issue["assignee"]))),
        issue["project"]["name"].as_str().map(str::to_owned),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if !properties.is_empty() {
        lines.push(properties.join(" · "));
    }
    if let Some(url) = issue["url"].as_str() {
        lines.push(url.into());
    }
    let description = issue["description"].as_str().unwrap_or_default();
    if !description.trim().is_empty() {
        lines.push(String::new());
        lines.push(
            description
                .chars()
                .take(limit.unwrap_or(usize::MAX))
                .collect(),
        );
    }
    json!({"id":issue["id"],"thread":issue["identifier"],"author":handle(&issue["creator"]),"text":lines.join("\n"),"createdAt":issue["createdAt"],"replies":issue["comments"]["nodes"].as_array().map_or(0,Vec::len),"files":[]})
}

pub(crate) async fn resolve(provider: &Linear, target: &str) -> Result<ResolvedTarget, SyncError> {
    let team_key = regex::Regex::new(r"^[A-Za-z0-9_]+$").expect("constant team key regex");
    let uuid = regex::Regex::new(
        r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$",
    )
    .expect("constant UUID regex");
    if team_key.is_match(target) {
        return Ok(ResolvedTarget {
            target: target.to_uppercase(),
            channel: target.to_uppercase(),
            kind: TargetKind::Channel,
        });
    }
    if let Some((team, id)) = target.split_once(':') {
        if team_key.is_match(team) && uuid.is_match(id) {
            return Ok(ResolvedTarget {
                target: format!("{}:{id}", team.to_uppercase()),
                channel: team.to_uppercase(),
                kind: TargetKind::Thread,
            });
        }
        return Err(SyncError::Payload(
            "sync the Linear issue, not an individual comment thread".into(),
        ));
    }
    let identifier =
        regex::Regex::new(r"^[A-Za-z0-9_]+-[0-9]+$").expect("constant issue identifier regex");
    if !uuid.is_match(target) && !identifier.is_match(target) {
        return Err(SyncError::Payload("invalid Linear target".into()));
    }
    let result = provider
        .graphql(
            "query($id:String!){issue(id:$id){id identifier team{key}}}",
            json!({"id":target}),
        )
        .await?;
    let issue = &result["issue"];
    let id = issue["id"].as_str().ok_or_else(|| SyncError::Platform {
        status: 404,
        code: "channel_not_found".into(),
    })?;
    let team = issue["team"]["key"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| team(issue["identifier"].as_str().unwrap_or_default()))
        .ok_or_else(|| SyncError::Payload("Linear issue has no team".into()))?
        .to_uppercase();
    Ok(ResolvedTarget {
        target: format!("{team}:{id}"),
        channel: team,
        kind: TargetKind::Thread,
    })
}

pub(crate) async fn read(
    provider: &Linear,
    target: &str,
    limit: usize,
    after: Option<&str>,
) -> Result<SourceRead, SyncError> {
    let issue_id = regex::Regex::new(r"(?:^|/issue/)([A-Za-z0-9_]+-[0-9]+)(?:[/?#]|$)")
        .expect("constant issue URL regex")
        .captures(target)
        .and_then(|m| m.get(1))
        .map(|m| m.as_str().to_uppercase());
    if let Some(id) = issue_id {
        let mut issue = Value::Null;
        let mut comments = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let query = format!(
                "query($id:String!,$first:Int!,$after:String){{issue(id:$id){{{FIELDS} comments(first:$first,after:$after){{nodes{{id body createdAt user{{name displayName url}}}}pageInfo{{hasNextPage endCursor}}}}}}}}"
            );
            let result = provider
                .graphql(&query, json!({"id":id,"first":100,"after":cursor}))
                .await?;
            if result["issue"].is_null() {
                return Err(SyncError::Platform {
                    status: 404,
                    code: "channel_not_found".into(),
                });
            }
            let found = result["issue"].clone();
            comments.extend(
                found["comments"]["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
            let next = if found["comments"]["pageInfo"]["hasNextPage"] == true {
                found["comments"]["pageInfo"]["endCursor"]
                    .as_str()
                    .map(str::to_owned)
            } else {
                None
            };
            if issue.is_null() {
                issue = found;
            }
            if next.is_none() || next == cursor || comments.len() >= 1000 {
                break;
            }
            cursor = next;
        }
        comments.sort_by(|a, b| {
            a["createdAt"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["createdAt"].as_str().unwrap_or_default())
        });
        let from = after
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0)
            .min(comments.len());
        let shown = comments
            .iter()
            .skip(from)
            .take(limit.clamp(1, 100))
            .collect::<Vec<_>>();
        let to = from + shown.len();
        let mut messages = Vec::new();
        if from == 0 {
            let mut root = issue_message(&issue, None);
            root["replies"] = json!(comments.len());
            messages.push(root);
        }
        let thread = issue["identifier"].as_str().unwrap_or(&id).to_owned();
        messages.extend(shown.into_iter().map(|comment|json!({"id":comment["id"],"thread":thread,"author":handle(&comment["user"]),"text":comment["body"].as_str().unwrap_or_default(),"createdAt":comment["createdAt"],"replies":0,"files":[]})));
        return Ok(SourceRead {
            channel: team(&thread).unwrap_or(id),
            name: None,
            thread: Some(thread),
            messages,
            more: to < comments.len(),
            next: (to < comments.len()).then(|| to.to_string()),
        });
    }
    let project = regex::Regex::new(r"/project/(?:[^/?#]*-)?([0-9a-f]{8,})(?:[/?#]|$)")
        .expect("constant project URL regex")
        .captures(target)
        .and_then(|m| m.get(1))
        .map(|m| m.as_str().to_owned());
    let (filter, channel, name) = if let Some(slug) = project {
        let result=provider.graphql("query($slugId:String!){projects(filter:{slugId:{eq:$slugId}},first:1){nodes{id name url}}}",json!({"slugId":slug})).await?;
        let project = result["projects"]["nodes"]
            .as_array()
            .and_then(|nodes| nodes.first())
            .ok_or_else(|| SyncError::Platform {
                status: 404,
                code: "channel_not_found".into(),
            })?;
        (
            json!({"project":{"id":{"eq":project["id"]}}}),
            project["url"].as_str().unwrap_or(target).to_owned(),
            project["name"].as_str().map(str::to_owned),
        )
    } else {
        let target = target.trim().to_uppercase();
        if target.is_empty()
            || !target
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(SyncError::Payload(
                "read a Linear team, issue identifier, or project URL".into(),
            ));
        }
        provider.team(&target).await?;
        (json!({"team":{"key":{"eq":target}}}), target, None)
    };
    let query = format!(
        "query($filter:IssueFilter,$first:Int!,$after:String){{issues(filter:$filter,first:$first,after:$after){{nodes{{{FIELDS} comments(first:20){{nodes{{id createdAt}}}}}}pageInfo{{hasNextPage endCursor}}}}}}"
    );
    let result = provider
        .graphql(
            &query,
            json!({"filter":filter,"first":limit.clamp(1,100),"after":after}),
        )
        .await?;
    let issues = &result["issues"];
    let more = issues["pageInfo"]["hasNextPage"] == true;
    Ok(SourceRead {
        channel,
        name,
        thread: None,
        messages: issues["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|issue| issue_message(issue, Some(1500)))
            .collect(),
        more,
        next: if more {
            issues["pageInfo"]["endCursor"].as_str().map(str::to_owned)
        } else {
            None
        },
    })
}
