use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use engage_client::{Client, ClientError, TaskClaim};
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::args::TaskArgs;
use crate::config::Claims;
use crate::util::{duration, labels, optional};

#[cfg(test)]
#[path = "task_run_tests.rs"]
mod tests;

type Renewal<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

pub async fn execute(client: &Client, args: &TaskArgs, claims: &Claims) -> Result<Value> {
    let command = args
        .text
        .as_deref()
        .context("tasks run needs a command after --")?;
    let target = args
        .target
        .as_deref()
        .context("tasks run needs a task ID or channel")?;
    let lease = Duration::from_secs_f64(
        args.options
            .lease
            .as_deref()
            .map(duration)
            .transpose()?
            .unwrap_or(900.0)
            .clamp(30.0, 3600.0),
    );
    let mut body = json!({});
    if let Some(lease) = &args.options.lease {
        body["lease"] = duration(lease)?.into();
    }
    optional(&mut body, "token", args.options.token.as_ref());
    let requested_at = acquisition_clock(
        Instant::now(),
        if args.options.ready {
            args.options.wait.as_deref()
        } else {
            None
        },
    )?;
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
    let claim: TaskClaim = serde_json::from_value(claimed["claim"].clone())
        .context("claim response lacks private credentials")?;
    let held = Held {
        client,
        args,
        claims,
        task,
        lease,
    };
    let result = run_claimed(&held, command, claim, requested_at).await;
    // Failed or uncertain renewals must not leave reusable stale capabilities behind.
    let cleanup = claims.drop(task);
    match result {
        Ok(value) => {
            cleanup?;
            Ok(value)
        }
        Err(error) => match cleanup {
            Ok(()) => Err(error),
            Err(cleanup) => Err(error.context(format!(
                "removing stale private claim also failed: {cleanup}"
            ))),
        },
    }
}

struct Held<'a> {
    client: &'a Client,
    args: &'a TaskArgs,
    claims: &'a Claims,
    task: &'a str,
    lease: Duration,
}

async fn run_claimed(
    held: &Held<'_>,
    command: &str,
    mut claim: TaskClaim,
    requested_at: Instant,
) -> Result<Value> {
    let deadline = confirmed_deadline(&claim.until, requested_at, held.lease)?;
    held.claims.put(held.task, &claim)?;
    let operation = json!({"epoch":claim.epoch,"token":claim.token});
    let mut child = match RunningCommand::spawn(command, &held.args.remaining, held.task) {
        Ok(child) => child,
        Err(error) => {
            let mut failure = operation.clone();
            failure["reason"] = format!("command could not start: {error}").into();
            settle(held, "fail", &failure, deadline).await?;
            return Err(error).context("starting task command");
        }
    };
    let (status, deadline) = monitor(held, &mut child, &mut claim, &operation, deadline).await?;
    // Descendants belong to this execution even if their immediate parent has exited.
    child.stop()?;
    let mut operation = operation;
    if status.success() {
        optional(&mut operation, "result", held.args.options.result.as_ref());
        settle(held, "complete", &operation, deadline).await
    } else {
        operation["reason"] = format!("command exited with {status}").into();
        settle(held, "fail", &operation, deadline).await?;
        bail!("task command exited with {status}")
    }
}

async fn monitor(
    held: &Held<'_>,
    child: &mut RunningCommand,
    claim: &mut TaskClaim,
    operation: &Value,
    mut deadline: Instant,
) -> Result<(std::process::ExitStatus, Instant)> {
    let mut next_renewal = renew_at(deadline, held.lease);
    let mut renewal: Option<Renewal<'_>> = None;
    let mut renewal_started = Instant::now();
    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => return Err(lost("task lease expired; command stopped")),
            _ = tokio::signal::ctrl_c() => {
                child.stop()?;
                settle(held,"release",operation,deadline).await?;
                bail!("task command interrupted; claim released");
            }
            status = child.child.wait() => return Ok((status?,deadline)),
            response = async {renewal.as_mut().expect("renewal branch requires a pending request").await}, if renewal.is_some() => {
                renewal = None;
                let response = response.context("task lease could not be renewed; command stopped")?;
                let until = response["until"].as_str().context("renewal response has no lease expiration")?;
                // Extend only after receiving and validating the server's confirmation.
                deadline = confirmed_deadline(until,renewal_started,held.lease)?;
                claim.until = until.into();
                held.claims.put(held.task,claim)?;
                next_renewal = renew_at(deadline,held.lease);
            }
            _ = tokio::time::sleep_until(next_renewal), if renewal.is_none() => {
                renewal_started = Instant::now();
                let mut body = operation.clone();
                if let Some(lease) = &held.args.options.lease {body["lease"] = duration(lease)?.into();}
                let timeout = request_deadline(deadline)?;
                renewal = Some(Box::pin(async move {
                    tokio::time::timeout_at(timeout,held.client.task_action(held.task,"renew",&body))
                        .await.map_err(|_|lost("task lease renewal timed out; command stopped"))?
                        .map_err(anyhow::Error::from)
                }));
            }
        }
    }
}

fn confirmed_deadline(until: &str, requested_at: Instant, lease: Duration) -> Result<Instant> {
    let until =
        chrono::DateTime::parse_from_rfc3339(until).context("claim expiration is not RFC 3339")?;
    let timestamp = until
        .timestamp_nanos_opt()
        .context("claim expiration is outside the supported range")?;
    let timestamp = u64::try_from(timestamp).map_err(|_| lost("task lease already expired"))?;
    let until = SystemTime::UNIX_EPOCH + Duration::from_nanos(timestamp);
    let now = Instant::now();
    let remaining = until
        .duration_since(SystemTime::now())
        .map_err(|_| lost("task lease already expired"))?;
    let cap = requested_at
        .checked_add(lease)
        .context("task lease exceeds the monotonic clock range")?;
    let deadline = now
        .checked_add(remaining)
        .map_or(cap, |until| until.min(cap));
    if deadline <= now {
        return Err(lost("task lease already expired"));
    }
    Ok(deadline)
}

fn acquisition_clock(requested_at: Instant, wait: Option<&str>) -> Result<Instant> {
    // A ready-task long poll grants the lease when work arrives, possibly long
    // after the request starts. Bound that grant by the server's maximum wait.
    let wait = wait
        .map(duration)
        .transpose()?
        .unwrap_or_default()
        .min(300.0);
    requested_at
        .checked_add(Duration::from_secs_f64(wait))
        .context("task wait exceeds the monotonic clock range")
}

fn renew_at(deadline: Instant, lease: Duration) -> Instant {
    let now = Instant::now();
    now + (deadline.saturating_duration_since(now) / 3)
        .min(lease / 3)
        .min(Duration::from_secs(10))
}

fn request_deadline(deadline: Instant) -> Result<Instant> {
    let now = Instant::now();
    let remaining = deadline.saturating_duration_since(now);
    if remaining.is_zero() {
        return Err(lost("task lease expired; command stopped"));
    }
    Ok(deadline - (remaining / 10).min(Duration::from_millis(100)))
}

async fn settle(held: &Held<'_>, action: &str, body: &Value, deadline: Instant) -> Result<Value> {
    let timeout = request_deadline(deadline)?;
    tokio::select! {
        biased;
        _ = tokio::signal::ctrl_c() => bail!("task result request interrupted"),
        response = tokio::time::timeout_at(timeout,held.client.task_action(held.task,action,body)) => {
            let response = response.map_err(|_|lost("task result could not be confirmed before lease expiration"))?
                .map_err(anyhow::Error::from)?;
            if Instant::now()>=deadline {return Err(lost("task result arrived after lease expiration"));}
            Ok(response)
        }
    }
}

fn lost(message: &str) -> anyhow::Error {
    ClientError::Api {
        status: 409,
        code: "claim_lost".into(),
        message: message.into(),
        hint: None,
    }
    .into()
}

struct RunningCommand {
    child: tokio::process::Child,
    stopped: bool,
    #[cfg(unix)]
    group: nix::unistd::Pid,
}

impl RunningCommand {
    fn spawn(program: &str, args: &[String], task: &str) -> std::io::Result<Self> {
        let mut command = tokio::process::Command::new(program);
        // Worker commands are noninteractive: a separate Unix group must not read the foreground TTY.
        command
            .args(args)
            .env("BOARD_TASK", task)
            .env("ENGAGE_TASK", task)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        let child = command.spawn()?;
        #[cfg(unix)]
        let group = nix::unistd::Pid::from_raw(
            i32::try_from(
                child.id().filter(|pid| *pid > 0).ok_or_else(|| {
                    std::io::Error::other("task command has no positive process ID")
                })?,
            )
            .map_err(|_| std::io::Error::other("task process ID is outside the supported range"))?,
        );
        Ok(Self {
            child,
            stopped: false,
            #[cfg(unix)]
            group,
        })
    }

    fn stop(&mut self) -> std::io::Result<()> {
        if self.stopped {
            return Ok(());
        }
        #[cfg(unix)]
        {
            match nix::sys::signal::killpg(self.group, nix::sys::signal::Signal::SIGKILL) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(error) => {
                    self.child.start_kill()?;
                    return Err(std::io::Error::from_raw_os_error(error as i32));
                }
            }
        }
        // Non-Unix targets stop the immediate child; descendant job control is platform-specific.
        self.child.start_kill()?;
        self.stopped = true;
        Ok(())
    }
}

impl Drop for RunningCommand {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
