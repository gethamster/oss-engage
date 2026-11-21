//! Slack permalink parsing and privacy-preserving quoted-message snapshots.
use crate::{Slack, SyncError};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub(crate) struct Permalink {
    pub url: String,
    pub channel: String,
    pub thread: String,
    pub message: String,
}
pub(crate) fn parse(raw: &str) -> Option<Permalink> {
    let raw = raw.replace("&amp;", "&");
    let url = url::Url::parse(&raw).ok()?;
    if url.scheme() != "https" || !url.host_str()?.ends_with(".slack.com") {
        return None;
    }
    let mut path = url.path_segments()?;
    if path.next()? != "archives" {
        return None;
    }
    let channel = path.next()?;
    if channel.is_empty()
        || !channel
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return None;
    }
    let compact = path.next()?.strip_prefix('p')?;
    if compact.len() != 16 || !compact.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if path.next().is_some() {
        return None;
    }
    let message = format!("{}.{}", &compact[..10], &compact[10..]);
    let thread = url
        .query_pairs()
        .find(|(key, value)| {
            key == "thread_ts"
                && value.split_once('.').is_some_and(|(seconds, micros)| {
                    !seconds.is_empty()
                        && !micros.is_empty()
                        && seconds
                            .chars()
                            .chain(micros.chars())
                            .all(|c| c.is_ascii_digit())
                })
        })
        .map(|(_, value)| value.into_owned())
        .unwrap_or_else(|| message.clone());
    Some(Permalink {
        url: raw,
        channel: channel.into(),
        thread,
        message,
    })
}
fn timestamp(value: &str) -> Value {
    let Some((seconds, micros)) = value.split_once('.') else {
        return Value::Null;
    };
    let Some(seconds) = seconds.parse::<i64>().ok() else {
        return Value::Null;
    };
    let Ok(micros) = micros.chars().take(6).collect::<String>().parse::<u32>() else {
        return Value::Null;
    };
    chrono::DateTime::from_timestamp(seconds, micros * 1000)
        .map(|at| json!(at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)))
        .unwrap_or(Value::Null)
}
fn files(value: &Value) -> Vec<Value> {
    value.as_array().into_iter().flatten().take(20).map(|file|json!({"name":file["name"].as_str().or(file["title"].as_str()).unwrap_or("file").chars().take(1000).collect::<String>(),"url":file["permalink"].as_str().map(|url|url.chars().take(4000).collect::<String>())})).collect()
}
fn snapshot(attachment: &Value, message: &str) -> Value {
    json!({"author":attachment["author_subname"].as_str().or(attachment["author_name"].as_str()).or(attachment["author_id"].as_str()).unwrap_or("someone").chars().take(256).collect::<String>(),"text":super::slack::slack_text(attachment["text"].as_str().unwrap_or_default()).chars().take(65536).collect::<String>(),"createdAt":timestamp(attachment["ts"].as_str().unwrap_or(message)),"files":files(&attachment["files"])})
}

pub(crate) fn links(key: &str, text: &str, attachments: &Value) -> Vec<Value> {
    let regex = regex::Regex::new(
        r"https://[a-z0-9.-]+\.slack\.com/archives/[A-Z0-9]+/p[0-9]{16}(?:\?[^\s|<>]*)?",
    )
    .expect("constant Slack permalink regex");
    let attachments = attachments.as_array().cloned().unwrap_or_default();
    let mut links = Vec::<Value>::new();
    let candidates = regex
        .find_iter(text)
        .map(|m| (m.as_str().to_owned(), None))
        .chain(
            attachments
                .iter()
                .filter(|att| att["is_share"] == true)
                .filter_map(|att| {
                    att["from_url"]
                        .as_str()
                        .or(att["original_url"].as_str())
                        .map(|url| (url.to_owned(), Some(att)))
                }),
        );
    for (raw, shared) in candidates {
        let Some(found) = parse(&raw) else { continue };
        if found.url.len() > 4000 {
            continue;
        }
        let attachment = shared.or_else(|| {
            attachments.iter().find(|att| {
                att["is_msg_unfurl"] == true
                    && ((att["channel_id"] == found.channel && att["ts"] == found.message)
                        || att["from_url"]
                            .as_str()
                            .is_some_and(|url| url.replace("&amp;", "&") == found.url))
            })
        });
        let thread = format!("{key}:{}:{}", found.channel, found.thread);
        if let Some(known) = links.iter_mut().find(|link| {
            link["url"] == found.url
                || (link["thread"] == thread && link["message"] == found.message)
        }) {
            if known["snapshot"].is_null()
                && let Some(attachment) = attachment
            {
                known["snapshot"] = snapshot(attachment, &found.message);
            }
            continue;
        }
        if links.len() >= 10 {
            break;
        }
        links.push(json!({"url":found.url,"thread":thread,"message":found.message,"snapshot":attachment.map(|attachment|snapshot(attachment,&found.message))}));
    }
    links
}
fn native_thread(value: &str) -> Option<(&str, &str)> {
    let pieces = value.split(':').collect::<Vec<_>>();
    if pieces.len() < 2 {
        return None;
    }
    Some((pieces[pieces.len() - 2], pieces[pieces.len() - 1]))
}
async fn one(
    provider: &Slack,
    channel: &str,
    thread: &str,
    message: &str,
) -> Result<Option<Value>, SyncError> {
    let page=provider.call("conversations.replies",json!({"channel":channel,"ts":thread,"oldest":message,"latest":message,"inclusive":true,"limit":1})).await?;
    Ok(page["messages"]
        .as_array()
        .and_then(|messages| messages.iter().find(|found| found["ts"] == message))
        .cloned())
}

pub(crate) async fn fetch(
    provider: &Slack,
    link: &Value,
    linker: &Value,
) -> Result<Value, SyncError> {
    let Some((channel, thread)) = link["thread"].as_str().and_then(native_thread) else {
        return Ok(json!({"snapshot":null,"unavailable":"invalid_link"}));
    };
    let info = match provider
        .call("conversations.info", json!({"channel":channel}))
        .await
    {
        Ok(value) => value,
        Err(_) => return Ok(json!({"snapshot":null,"unavailable":"channel_not_found"})),
    };
    let info = &info["channel"];
    let unavailable = if info.is_null() {
        Some("channel_not_found")
    } else if info["is_private"] == true || info["is_im"] == true || info["is_mpim"] == true {
        Some("private_channel")
    } else if info["is_ext_shared"] == true {
        Some("shared_channel")
    } else if info["is_member"] != true {
        Some("not_in_channel")
    } else {
        None
    };
    if let Some(reason) = unavailable {
        return Ok(json!({"snapshot":null,"unavailable":reason}));
    }
    let Some((at, root)) = linker["thread"].as_str().and_then(native_thread) else {
        return Ok(json!({"snapshot":null,"unavailable":"poster_cannot_read"}));
    };
    let Some(poster) = one(
        provider,
        at,
        root,
        linker["message"].as_str().unwrap_or_default(),
    )
    .await?
    else {
        return Ok(json!({"snapshot":null,"unavailable":"poster_cannot_read"}));
    };
    if let Some(user) = poster["user"].as_str() {
        let user = provider.call("users.info", json!({"user":user})).await?;
        let user = &user["user"];
        if user["is_bot"] != true {
            let team = provider.call("auth.test", json!({})).await?;
            if user.is_null()
                || user["is_restricted"] == true
                || user["is_ultra_restricted"] == true
                || user["is_stranger"] == true
                || user["team_id"].as_str().is_none()
                || user["team_id"] != team["team_id"]
            {
                return Ok(json!({"snapshot":null,"unavailable":"poster_cannot_read"}));
            }
        }
    }
    let Some(message) = one(
        provider,
        channel,
        thread,
        link["message"].as_str().unwrap_or_default(),
    )
    .await?
    else {
        return Ok(json!({"snapshot":null,"unavailable":"message_not_found"}));
    };
    Ok(
        json!({"snapshot":{"author":message["username"].as_str().or(message["bot_profile"]["name"].as_str()).or(message["user"].as_str()).unwrap_or("someone"),"text":super::slack::slack_text(message["text"].as_str().unwrap_or_default()),"createdAt":timestamp(message["ts"].as_str().unwrap_or_default()),"files":files(&message["files"])},"unavailable":null}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_permalink_without_admitting_an_untrusted_host() {
        let found=parse("https://acme.slack.com/archives/C0123/p1727000000123456?thread_ts=1727000000.000001&amp;cid=C0123").expect("Slack link");
        assert_eq!(found.message, "1727000000.123456");
        assert_eq!(found.thread, "1727000000.000001");
        assert!(
            parse("https://acme.slack.com.evil.test/archives/C0123/p1727000000123456").is_none()
        );
    }
    #[test]
    fn deduplicates_unfurls_and_preserves_platform_snapshot() {
        let url = "https://acme.slack.com/archives/C1/p1727000000123456";
        let links = links(
            "slack",
            &format!("<{url}> <{url}>"),
            &json!([{"is_msg_unfurl":true,"channel_id":"C1","ts":"1727000000.123456","author_name":"Ada","text":"proof"}]),
        );
        assert_eq!(links.len(), 1);
        assert_eq!(links[0]["snapshot"]["author"], "Ada");
        assert_eq!(links[0]["snapshot"]["text"], "proof");
    }
    #[tokio::test]
    async fn private_and_shared_channels_are_never_fetched_for_link_snapshots() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (channel, reason) in [
            (
                json!({"is_private":true,"is_member":true}),
                "private_channel",
            ),
            (
                json!({"is_ext_shared":true,"is_member":true}),
                "shared_channel",
            ),
            (json!({"is_member":false}), "not_in_channel"),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("test listener");
            let address = listener.local_addr().expect("address");
            let task = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("test connection");
                let mut buffer = [0; 8192];
                let _ = stream.read(&mut buffer).await.expect("request");
                let body = json!({"ok":true,"channel":channel}).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("response");
            });
            let credentials = crate::StaticCredentials(crate::Credentials {
                access_token: crate::Secret::new("fixture"),
                webhook_secret: crate::Secret::new("fixture"),
                bot_user_id: None,
            });
            let mut provider = Slack::new(credentials);
            provider.api.base = format!("http://{address}");
            let result = fetch(
                &provider,
                &json!({"thread":"slack:C1:1.0","message":"1.0"}),
                &json!({"thread":"slack:C2:2.0","message":"2.0"}),
            )
            .await
            .expect("safe unavailable result");
            assert_eq!(result["unavailable"], reason);
            assert!(result["snapshot"].is_null());
            task.await.expect("test server");
        }
    }
}
