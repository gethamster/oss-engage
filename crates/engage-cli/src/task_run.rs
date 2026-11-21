use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use engage_client::{Client, TaskClaim};
use serde_json::{Value, json};

use crate::args::TaskArgs;
use crate::config::Claims;
use crate::util::{duration, labels, optional};

pub async fn execute(client: &Client, args: &TaskArgs, claims: &Claims) -> Result<Value> {
    let command = args
        .text
        .as_deref()
        .context("tasks run needs a command after --")?;
    let target = args
        .target
        .as_deref()
        .context("tasks run needs a task ID or channel")?;
    let mut body = json!({});
    if let Some(lease) = &args.options.lease {
        body["lease"] = duration(lease)?.into();
    }
    optional(&mut body, "token", args.options.token.as_ref());
    let claimed = if args.options.ready {
        body["channel"] = target.into();
        if !args.options.label.is_empty() {
            body["label"] = labels(&args.options.label)?;
        }
        if !args.options.from.is_empty() {
            body["from"] = json!(args.options.from);
        }
        if let Some(wait) = &args.options.wait {
            body["wait"] = duration(wait)?.into();
        }
        client.write("/tasks/claim", &body).await?
    } else {
        client.task_action(target, "claim", &body).await?
    };
    let task = claimed["task"]["id"]
        .as_str()
        .context("no ready task was available")?;
    let mut claim: TaskClaim = serde_json::from_value(claimed["claim"].clone())
        .context("claim response lacks private credentials")?;
    claims.put(task, &claim)?;
    let mut operation = json!({"epoch":claim.epoch,"token":claim.token});
    let mut child = match tokio::process::Command::new(command)
        .args(&args.remaining)
        .env("BOARD_TASK", task)
        .env("ENGAGE_TASK", task)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            operation["reason"] = format!("command could not start: {error}").into();
            client.task_action(task, "fail", &operation).await?;
            claims.drop(task)?;
            return Err(error).context("starting task command");
        }
    };
    let requested_lease = args
        .options
        .lease
        .as_deref()
        .map(duration)
        .transpose()?
        .unwrap_or(900.0)
        .clamp(30.0, 3600.0);
    // Renewal runs before even the smallest lease a local backend grants.
    let mut renewals = tokio::time::interval(Duration::from_secs_f64(
        (requested_lease / 3.0).clamp(1.0, 10.0),
    ));
    renewals.tick().await;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status?,
            _ = renewals.tick() => {
                let mut renewal = operation.clone();
                if let Some(lease) = &args.options.lease { renewal["lease"] = duration(lease)?.into(); }
                match client.task_action(task,"renew",&renewal).await {
                    Ok(response) => {
                        if let Some(until) = response["until"].as_str() { claim.until = until.into(); claims.put(task,&claim)?; }
                    }
                    Err(error) => {
                        let _ = child.kill().await;
                        claims.drop(task)?;
                        return Err(error).context("task lease could not be renewed; command stopped");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                let _ = child.kill().await;
                client.task_action(task,"release",&operation).await?;
                claims.drop(task)?;
                bail!("task command interrupted; claim released");
            }
        }
    };
    if status.success() {
        optional(&mut operation, "result", args.options.result.as_ref());
        let result = client.task_action(task, "complete", &operation).await?;
        claims.drop(task)?;
        Ok(result)
    } else {
        operation["reason"] = format!("command exited with {status}").into();
        client.task_action(task, "fail", &operation).await?;
        claims.drop(task)?;
        bail!("task command exited with {status}")
    }
}
