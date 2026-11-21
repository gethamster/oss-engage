use anyhow::{Context, Result, bail};
use engage_client::{Client, TaskClaim};
use serde_json::{Value, json};

use crate::args::TaskArgs;
use crate::config::Claims;
use crate::util::{body_input, duration, labels, optional, query_options, read_text};

pub async fn execute(
    client: &Client,
    args: &TaskArgs,
    directory: std::path::PathBuf,
) -> Result<Value> {
    let me = client.me().await?;
    let claims = Claims::new(
        directory.join("claims"),
        client.base_url(),
        me["actor"].as_str(),
    );
    let action = args.action.as_deref().unwrap_or("list");
    let options = &args.options;
    let actions = [
        "list", "create", "batch", "created", "claim", "run", "renew", "release", "complete",
        "fail", "block", "unblock", "reopen", "assign", "needs", "cancel",
    ];
    if !actions.contains(&action) || action == "list" {
        let mut query = query_options(options);
        let channel = if action == "list" {
            args.target.as_deref()
        } else {
            Some(action)
        };
        if let Some(channel) = channel {
            query.push(("channel".into(), channel.into()));
        }
        return Ok(client.tasks(&query).await?);
    }
    let target = args.target.as_deref();
    let mut body = if options.data.is_some() {
        body_input(options)?
    } else {
        json!({})
    };
    if action == "run" {
        return crate::task_run::execute(client, args, &claims).await;
    }
    match action {
        "create" | "batch" => {
            if options.file.is_some() || action == "batch" {
                let input = body_input(options)?;
                body = if input.is_array() {
                    json!({"channel":target.context("a task batch needs a channel")?,"tasks":input})
                } else {
                    input
                };
                optional(&mut body, "id", options.idempotency_key.as_ref());
                return Ok(client.write("/tasks/batch", &body).await?);
            }
            if let Some(re) = options
                .re
                .as_deref()
                .or(target.filter(|_| args.text.is_none() && options.attach.is_empty()))
            {
                if !options.label.is_empty()
                    || options.idempotency_key.is_some()
                    || options.no_sync
                    || !options.attach.is_empty()
                {
                    bail!(
                        "promoting a thread keeps its current text, labels, attachments, and idempotency key"
                    );
                }
                body["re"] = re.into();
            } else {
                body["channel"] = target.context("create needs a channel or --re")?.into();
                body["text"] = if args.text.is_none() && !options.attach.is_empty() {
                    String::new()
                } else {
                    read_text(args.text.as_deref(), None)?
                }
                .into();
                if !options.label.is_empty() {
                    body["labels"] = labels(&options.label)?;
                }
                optional(&mut body, "id", options.idempotency_key.as_ref());
                if options.no_sync {
                    body["noSync"] = true.into();
                }
                if !options.attach.is_empty() {
                    body["attachments"] =
                        json!(crate::run::upload_attachments(client, &options.attach).await?);
                }
            }
            if !options.needs.is_empty() {
                body["needs"] = json!(options.needs);
            }
            if let Some(assigned) = &options.for_value {
                body["for"] = assigned_actor(&me, assigned).into();
            }
            if options.claim {
                body["claim"] = true.into();
            }
            if let Some(lease) = &options.lease {
                body["lease"] = duration(lease)?.into();
            }
            let result = client.create_task(&body).await?;
            save_claim(&claims, &result)?;
            Ok(result)
        }
        "created" => Ok(client
            .request(
                "GET",
                &format!(
                    "/tasks/batch/{}",
                    Client::segment(target.context("created needs a batch ID")?)
                ),
                &[],
                None,
            )
            .await?),
        "claim" => {
            if let Some(lease) = &options.lease {
                body["lease"] = duration(lease)?.into();
            }
            optional(&mut body, "token", options.token.as_ref());
            let result = if options.ready {
                body["channel"] = target
                    .context("claim --ready needs a channel or subtree")?
                    .into();
                if !options.label.is_empty() {
                    body["label"] = labels(&options.label)?;
                }
                if !options.from.is_empty() {
                    body["from"] = json!(options.from);
                }
                if let Some(wait) = &options.wait {
                    body["wait"] = duration(wait)?.into();
                }
                client.write("/tasks/claim", &body).await?
            } else {
                client
                    .task_action(
                        target.context("claim needs a task ID or --ready CHANNEL")?,
                        "claim",
                        &body,
                    )
                    .await?
            };
            save_claim(&claims, &result)?;
            Ok(result)
        }
        _ => {
            let target = target.context("task operation needs a task ID")?;
            if ["renew", "release", "complete", "fail"].contains(&action) {
                add_claim(
                    &mut body,
                    &claims,
                    target,
                    options.epoch,
                    options.token.as_deref(),
                )?;
            }
            match action {
                "renew" => {
                    if let Some(lease) = &options.lease {
                        body["lease"] = duration(lease)?.into();
                    }
                }
                "complete" => {
                    if let Some(text) = &args.text {
                        body["result"] = text.clone().into();
                    }
                }
                "fail" | "cancel" => {
                    body["reason"] = args
                        .text
                        .as_deref()
                        .context("this operation needs a reason")?
                        .into()
                }
                "block" => {
                    body["reason"] = args.text.as_deref().context("block needs a reason")?.into();
                    optional(&mut body, "kind", options.kind.as_ref());
                    if options.epoch.is_some()
                        || options.token.is_some()
                        || claims.get(target).is_ok()
                    {
                        add_claim(
                            &mut body,
                            &claims,
                            target,
                            options.epoch,
                            options.token.as_deref(),
                        )?;
                    }
                }
                "assign" => {
                    body["for"] = if options.pool {
                        Value::Null
                    } else {
                        assigned_actor(
                            &me,
                            args.text
                                .as_deref()
                                .or(options.for_value.as_deref())
                                .context("assign needs an actor or --pool")?,
                        )
                        .into()
                    };
                }
                "needs" => {
                    let mut needs = options.needs.clone();
                    if let Some(text) = &args.text {
                        needs.extend(text.split(',').map(str::to_owned));
                    }
                    needs.extend(args.remaining.iter().cloned());
                    if needs.is_empty() && !options.none {
                        bail!("needs requires --needs ID or --none");
                    }
                    body["needs"] = json!(needs);
                }
                _ => {}
            }
            let result = match client.task_action(target, action, &body).await {
                Ok(result) => result,
                Err(error) => {
                    if matches!(&error, engage_client::ClientError::Api { code, .. } if code == "claim_lost")
                    {
                        claims.drop(target)?;
                    }
                    return Err(error.into());
                }
            };
            if action == "renew" {
                let mut claim = match (options.epoch, options.token.as_deref()) {
                    (Some(epoch), Some(token)) => TaskClaim {
                        epoch,
                        token: token.into(),
                        until: String::new(),
                    },
                    _ => claims.get(target)?,
                };
                if let Some(until) = result["until"].as_str() {
                    claim.until = until.into();
                    claims.put(target, &claim)?;
                }
            } else if ["release", "complete", "fail", "block", "cancel"].contains(&action) {
                claims.drop(target)?;
            }
            Ok(result)
        }
    }
}

fn add_claim(
    body: &mut Value,
    claims: &Claims,
    task: &str,
    epoch: Option<u64>,
    token: Option<&str>,
) -> Result<()> {
    match (epoch, token) {
        (Some(epoch), Some(token)) => {
            body["epoch"] = epoch.into();
            body["token"] = token.into();
        }
        (None, None) => {
            let claim = claims.get(task)?;
            body["epoch"] = claim.epoch.into();
            body["token"] = claim.token.into();
        }
        _ => bail!("a claim needs both --epoch and --token"),
    }
    Ok(())
}

fn save_claim(claims: &Claims, response: &Value) -> Result<()> {
    if let (Some(task), Some(claim)) = (response["task"]["id"].as_str(), response.get("claim")) {
        let claim: TaskClaim = serde_json::from_value(claim.clone())?;
        claims.put(task, &claim)?;
    }
    Ok(())
}

fn assigned_actor(me: &Value, value: &str) -> String {
    let principal = me["principal"].as_str().unwrap_or_default();
    if value == principal || value.contains('+') || value.contains(':') || principal.is_empty() {
        value.into()
    } else {
        format!("{principal}+{value}")
    }
}
