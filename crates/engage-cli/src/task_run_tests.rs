use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use axum::extract::State;
use axum::http::Request;
use axum::response::{IntoResponse, Response};
use clap::Parser;

use super::*;
use crate::args::{Cli, Command};

#[derive(Clone)]
struct Fixture {
    until: String,
    renewed_until: Option<String>,
    renewals: Arc<AtomicUsize>,
    completions: Arc<AtomicUsize>,
}

async fn fixture(State(fixture): State<Fixture>, request: Request<axum::body::Body>) -> Response {
    match request.uri().path() {
        "/tasks/1/claim" => axum::Json(json!({"task":{"id":"1"},"claim":{"epoch":1,"token":"private-fixture-token","until":fixture.until}})).into_response(),
        "/tasks/1/renew" => {
            fixture.renewals.fetch_add(1, Ordering::SeqCst);
            if let Some(until) = fixture.renewed_until {return axum::Json(json!({"until":until})).into_response();}
            std::future::pending::<Response>().await
        }
        "/tasks/1/complete" => {
            fixture.completions.fetch_add(1, Ordering::SeqCst);
            axum::Json(json!({"task":{"id":"1","task":{"state":"completed"}}})).into_response()
        }
        _ => axum::Json(json!({"ok":true})).into_response(),
    }
}

async fn start(
    until: SystemTime,
    renewed_until: Option<SystemTime>,
) -> anyhow::Result<(
    Client,
    Fixture,
    tokio::task::JoinHandle<std::io::Result<()>>,
)> {
    let until = chrono::DateTime::<chrono::Utc>::from(until)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let state = Fixture {
        until,
        renewed_until: renewed_until.map(|until| {
            chrono::DateTime::<chrono::Utc>::from(until)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        }),
        renewals: Arc::default(),
        completions: Arc::default(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = axum::Router::new()
        .fallback(axum::routing::any(fixture))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    Ok((Client::new(&format!("http://{address}"))?, state, server))
}

fn arguments(script: &str, remaining: &[&str]) -> TaskArgs {
    let mut args = vec![
        "engage",
        "tasks",
        "run",
        "1",
        "--lease",
        "30s",
        "--",
        "/bin/sh",
        "-c",
        script,
        "engage-test",
    ];
    args.extend_from_slice(remaining);
    let cli = Cli::try_parse_from(args).expect("task runner arguments");
    let Command::Tasks(task) = cli.command else {
        panic!("expected task command")
    };
    task
}

#[cfg(unix)]
#[tokio::test]
async fn stalled_renewal_cannot_keep_command_or_descendants_past_lease() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let started = temp.path().join("started");
    let escaped = temp.path().join("escaped");
    let (client, state, server) =
        start(SystemTime::now() + Duration::from_millis(250), None).await?;
    let claims = Claims::new(temp.path().join("claims"), client.base_url(), None);
    let args = arguments(
        "printf started > \"$1\"; (sleep 0.8; printf escaped > \"$2\") & sleep 3; wait",
        &[
            started.to_str().expect("path"),
            escaped.to_str().expect("path"),
        ],
    );
    let began = Instant::now();
    let result =
        tokio::time::timeout(Duration::from_secs(1), execute(&client, &args, &claims)).await;
    let elapsed = began.elapsed();
    tokio::time::sleep(Duration::from_millis(900)).await;
    server.abort();
    assert!(
        result.is_ok(),
        "command ignored the confirmed lease expiration"
    );
    assert!(
        result.expect("bounded result").is_err(),
        "an expired claim cannot complete"
    );
    assert!(
        elapsed < Duration::from_millis(800),
        "lease stop took {elapsed:?}"
    );
    assert!(started.exists(), "command actually started");
    assert!(
        state.renewals.load(Ordering::SeqCst) > 0,
        "fixture must stall an in-flight renewal"
    );
    assert!(
        !escaped.exists(),
        "a command descendant survived loss of the lease"
    );
    assert_eq!(
        state.completions.load(Ordering::SeqCst),
        0,
        "expired work must never be completed"
    );
    assert!(
        claims.get("1").is_err(),
        "private stale claim must be removed"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn child_exit_is_observed_while_renewal_is_pending() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let (client, state, server) =
        start(SystemTime::now() + Duration::from_millis(600), None).await?;
    let claims = Claims::new(temp.path().join("claims"), client.base_url(), None);
    let args = arguments("sleep 0.3", &[]);
    let result =
        tokio::time::timeout(Duration::from_secs(1), execute(&client, &args, &claims)).await;
    server.abort();
    let response = result.context("child exit was blocked by an in-flight renewal")??;
    assert_eq!(response["task"]["task"]["state"], "completed");
    assert!(
        state.renewals.load(Ordering::SeqCst) > 0,
        "fixture must stall a renewal before exit"
    );
    assert_eq!(state.completions.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn deadlines_reject_bad_expirations_and_never_exceed_request_lease() {
    let requested_at = tokio::time::Instant::now() - Duration::from_millis(100);
    let lease = Duration::from_millis(200);
    let remote_until =
        chrono::DateTime::<chrono::Utc>::from(SystemTime::now() + Duration::from_secs(3600))
            .to_rfc3339();
    assert_eq!(
        confirmed_deadline(&remote_until, requested_at, lease).expect("valid expiration"),
        requested_at + lease
    );
    assert!(confirmed_deadline("not-a-timestamp", requested_at, lease).is_err());
    assert!(confirmed_deadline("1970-01-01T00:00:00Z", requested_at, lease).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn confirmed_renewal_allows_command_to_finish_beyond_initial_lease() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let now = SystemTime::now();
    let (client, state, server) = start(
        now + Duration::from_millis(250),
        Some(now + Duration::from_secs(2)),
    )
    .await?;
    let claims = Claims::new(temp.path().join("claims"), client.base_url(), None);
    let args = arguments("sleep 0.4", &[]);
    let result =
        tokio::time::timeout(Duration::from_secs(1), execute(&client, &args, &claims)).await;
    server.abort();
    let response = result.context("confirmed renewal should extend the deadline")??;
    assert_eq!(response["task"]["task"]["state"], "completed");
    assert!(state.renewals.load(Ordering::SeqCst) > 0);
    assert_eq!(state.completions.load(Ordering::SeqCst), 1);
    Ok(())
}
