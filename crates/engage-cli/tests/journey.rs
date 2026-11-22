use std::path::Path;

use engage_server::{BlobConfig, ServerConfig};
use serde_json::Value;

async fn command(
    url: &str,
    config: &Path,
    args: &[&str],
) -> Result<Value, Box<dyn std::error::Error>> {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_engage"))
        .args(["--url", url, "--json"])
        .args(args)
        .env("ENGAGE_CONFIG_DIR", config)
        .env_remove("ENGAGE_TOKEN")
        .env_remove("BOARD_TOKEN")
        .env_remove("ENGAGE_PROFILE")
        .env_remove("BOARD_PROFILE")
        .output()
        .await?;
    assert!(
        output.status.success(),
        "engage {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[tokio::test]
async fn claim_survives_cli_processes_and_command_runner_completes()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let board = engage_store::Board::connect("sqlite::memory:").await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let config = ServerConfig {
        bind: address,
        inspector_dir: None,
        blobs: BlobConfig::Memory,
        ..ServerConfig::default()
    };
    let app = engage_server::router(board, config).await?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let url = format!("http://{address}");
    let channel = command(&url, directory.path(), &["channels", "create", "dev"]).await?;
    assert_eq!(channel["channel"]["path"], "dev");
    let private = command(
        &url,
        directory.path(),
        &["channels", "create", "security", "--private"],
    )
    .await?;
    assert_eq!(private["channel"]["path"], "security");
    assert_eq!(
        private["grants"]
            .as_array()
            .expect("private channel grants")
            .len(),
        2
    );
    let created = command(
        &url,
        directory.path(),
        &["tasks", "create", "work", "verify the release"],
    )
    .await?;
    let id = created["task"]["id"].as_str().expect("task ID");
    let claim = command(
        &url,
        directory.path(),
        &["--as", "adam", "tasks", "claim", id],
    )
    .await?;
    assert!(claim["claim"]["token"].as_str().is_some());
    let completed = command(
        &url,
        directory.path(),
        &["--as", "adam", "tasks", "complete", id, "verified"],
    )
    .await?;
    assert_eq!(completed["task"]["task"]["state"], "completed");
    assert!(
        completed
            .to_string()
            .find(claim["claim"]["token"].as_str().expect("claim token"))
            .is_none()
    );
    let from_json = command(
        &url,
        directory.path(),
        &[
            "tasks",
            "create",
            "--data",
            r#"{"channel":"work","text":"JSON-created task","claim":true}"#,
        ],
    )
    .await?;
    assert_eq!(from_json["task"]["text"], "JSON-created task");
    let json_id = from_json["task"]["id"].as_str().expect("JSON task ID");
    let completed_json = command(
        &url,
        directory.path(),
        &["tasks", "complete", json_id, "saved JSON claim"],
    )
    .await?;
    assert_eq!(completed_json["task"]["task"]["state"], "completed");
    let malformed = tokio::process::Command::new(env!("CARGO_BIN_EXE_engage"))
        .args(["--url", &url, "tasks", "create", "--data", "null"])
        .env("ENGAGE_CONFIG_DIR", directory.path())
        .output()
        .await?;
    assert!(!malformed.status.success());
    let error = String::from_utf8_lossy(&malformed.stderr);
    assert!(error.contains("must be a JSON object"), "{error}");
    assert!(
        !error.contains("panicked"),
        "invalid input must not panic: {error}"
    );
    let huge_timeout = tokio::process::Command::new(env!("CARGO_BIN_EXE_engage"))
        .args(["--url", &url, "wait", "work", "--timeout", "1e300"])
        .env("ENGAGE_CONFIG_DIR", directory.path())
        .output()
        .await?;
    assert!(!huge_timeout.status.success());
    let error = String::from_utf8_lossy(&huge_timeout.stderr);
    assert!(error.contains("timeout is outside"), "{error}");
    assert!(
        !error.contains("panicked"),
        "invalid timeout must not panic: {error}"
    );
    let created = command(
        &url,
        directory.path(),
        &["tasks", "create", "work", "run a command"],
    )
    .await?;
    let id = created["task"]["id"].as_str().expect("task ID");
    #[cfg(unix)]
    {
        let result = command(
            &url,
            directory.path(),
            &[
                "tasks",
                "run",
                id,
                "--result",
                "passed",
                "--",
                "/usr/bin/true",
            ],
        )
        .await?;
        assert_eq!(result["task"]["task"]["state"], "completed");
        assert_eq!(result["task"]["task"]["result"], "passed");
    }
    server.abort();
    Ok(())
}
