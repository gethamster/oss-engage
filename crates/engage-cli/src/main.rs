mod admin;
mod args;
mod config;
mod inspect;
mod run;
mod task_run;
mod tasks;
mod util;

use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = args::Cli::parse();
    if let Err(error) = run::execute(&cli).await {
        if cli.json {
            let value = if let Some(engage_client::ClientError::Api {
                status,
                code,
                message,
                hint,
            }) = error.downcast_ref::<engage_client::ClientError>()
            {
                serde_json::json!({"error":{"status":status,"code":code,"message":message,"hint":hint}})
            } else {
                serde_json::json!({"error":{"code":"cli_error","message":format!("{error:#}")}})
            };
            eprintln!("{value}");
        } else {
            eprintln!("engage: {error:#}");
        }
        std::process::exit(1);
    }
}
