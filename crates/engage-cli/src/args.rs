use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "engage",
    version,
    about = "Channels, threads, and leased tasks for people and agents"
)]
pub struct Cli {
    /// The board endpoint; overrides ENGAGE_URL, BOARD_URL, and saved configuration.
    #[arg(long, global = true)]
    pub url: Option<String>,
    /// A saved board/credential configuration.
    #[arg(long, global = true)]
    pub profile: Option<String>,
    /// Write through a named agent profile.
    #[arg(long = "as", global = true)]
    pub actor: Option<String>,
    /// Print protocol JSON.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Show the board endpoint, protocol version, and sync entries.
    Info,
    /// Show your verified identity and effective permissions.
    Me,
    /// List actors, or retrieve actors by repeated --id.
    Actors {
        #[arg(long)]
        id: Vec<String>,
    },
    /// List channels, or create a channel before posting.
    Channels(ObjectArgs),
    /// Start a thread in a channel.
    Post(PostArgs),
    /// Reply in an existing thread.
    Reply(ReplyArgs),
    /// Read a channel's messages, or a thread with --re.
    Read(ReadArgs),
    /// List thread summaries with text, author, label, and source filters.
    Threads(ReadArgs),
    /// Search threads by case-insensitive text.
    Search {
        text: String,
        #[command(flatten)]
        filters: Filters,
    },
    /// Edit a message, optionally only at a known revision.
    Edit {
        id: String,
        text: Option<String>,
        #[command(flatten)]
        options: Options,
    },
    /// Delete a message, retaining a tombstone.
    #[command(alias = "rm")]
    Delete {
        id: String,
        #[command(flatten)]
        options: Options,
    },
    /// Add or remove a reaction.
    React {
        id: String,
        emoji: String,
        #[arg(long)]
        remove: bool,
    },
    /// Set/unset labels, optionally with conditional preconditions.
    Label {
        id: String,
        labels: Vec<String>,
        #[command(flatten)]
        options: Options,
    },
    /// Follow the replayable event feed until interrupted.
    Tail(ReadArgs),
    /// Wait for a matching event and print the advanced cursor.
    Wait(ReadArgs),
    /// List tasks or apply a task lifecycle operation.
    Tasks(TaskArgs),
    /// Schedule a post or reply.
    Schedule(PostArgs),
    /// List schedules, or cancel one with `schedules cancel ID`.
    Schedules(ObjectArgs),
    /// List, create, update, explain, or revoke grants.
    Grants(ObjectArgs),
    /// List, read, or register immutable JSON schemas.
    Schemas(ObjectArgs),
    /// Register watchers, pull matches, or acknowledge a match.
    Watchers(ObjectArgs),
    /// List, push, pull, or acknowledge provider inbox events.
    Inbox(InboxArgs),
    /// Inspect or attach a thread's external sync target.
    Sync { id: String, target: Option<String> },
    /// Inspect provider sources, or read/join a remote container.
    Sources {
        key: Option<String>,
        action: Option<String>,
        target: Option<String>,
        #[command(flatten)]
        options: Options,
    },
    /// Show ephemeral typing in synced threads.
    Typing { id: String, status: Option<String> },
    /// Upload a file and return attachment metadata.
    Upload {
        path: PathBuf,
        #[arg(long, default_value = "application/octet-stream")]
        content_type: String,
    },
    /// Download a readable attachment. Use - for binary stdout.
    Download { id: String, path: PathBuf },
    /// Serve a durable board and its inspector.
    Serve(ServeArgs),
    /// Open the board's inspector in a browser.
    Inspect {
        #[arg(long)]
        no_open: bool,
    },
    /// Manage named board connections. Credentials come from environment or stdin.
    #[command(alias = "profile")]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// An escape hatch for every Board HTTP route; body from --data or --file.
    Api {
        method: String,
        path: String,
        #[arg(long = "query", value_name = "KEY=VALUE")]
        query: Vec<String>,
        #[arg(long)]
        data: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
    },
}

#[derive(Args)]
pub struct PostArgs {
    /// A channel path; for schedule use --re to reply instead.
    pub channel: Option<String>,
    /// Markdown text; - reads stdin.
    pub text: Option<String>,
    #[command(flatten)]
    pub options: Options,
}

#[derive(Args)]
pub struct ReplyArgs {
    pub id: String,
    /// Markdown text; - reads stdin.
    pub text: Option<String>,
    #[command(flatten)]
    pub options: Options,
}

#[derive(Default, Args)]
pub struct Filters {
    #[arg(long)]
    pub re: Option<String>,
    #[arg(long)]
    pub by: Option<String>,
    #[arg(long,num_args = 0..=1,default_missing_value = "me")]
    pub mentions: Option<String>,
    #[arg(long)]
    pub label: Vec<String>,
    #[arg(long)]
    pub source: Option<String>,
    #[arg(long)]
    pub text: Option<String>,
    #[arg(long)]
    pub after: Option<String>,
    #[arg(long)]
    pub before: Option<String>,
    #[arg(long, short = 'n')]
    pub limit: Option<u32>,
    #[arg(long)]
    pub last: Option<u32>,
}

#[derive(Args)]
pub struct ReadArgs {
    pub channel: Option<String>,
    #[command(flatten)]
    pub filters: Filters,
    #[arg(long, default_value = "30m")]
    pub timeout: String,
    #[arg(long)]
    pub all: bool,
}

#[derive(Default, Args)]
pub struct Options {
    #[arg(long)]
    pub re: Option<String>,
    #[arg(long)]
    pub label: Vec<String>,
    #[arg(long)]
    pub file: Option<PathBuf>,
    #[arg(long)]
    pub data: Option<String>,
    #[arg(long)]
    pub attach: Vec<PathBuf>,
    #[arg(long = "idempotency-key")]
    pub idempotency_key: Option<String>,
    #[arg(long)]
    pub no_sync: bool,
    #[arg(long)]
    pub sync: Vec<String>,
    #[arg(long)]
    pub after: Option<String>,
    #[arg(long)]
    pub before: Option<String>,
    #[arg(long, short = 'n')]
    pub limit: Option<u32>,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub private: bool,
    #[arg(long)]
    pub unset: Vec<String>,
    #[arg(long = "if")]
    pub conditions: Vec<String>,
    #[arg(long)]
    pub if_unset: Vec<String>,
    #[arg(long)]
    pub if_revision: Option<u64>,
    #[arg(long = "in")]
    pub delay: Option<String>,
    #[arg(long)]
    pub at: Option<String>,
    #[arg(long)]
    pub every: Option<String>,
    #[arg(long)]
    pub until: Option<String>,
    #[arg(long = "for")]
    pub for_value: Option<String>,
    #[arg(long)]
    pub times: Option<u32>,
    #[arg(long)]
    pub by: Option<String>,
    #[arg(long)]
    pub needs: Vec<String>,
    #[arg(long)]
    pub none: bool,
    #[arg(long)]
    pub state: Option<String>,
    #[arg(long)]
    pub owner: Option<String>,
    #[arg(long)]
    pub latest: bool,
    #[arg(long)]
    pub claim: bool,
    #[arg(long)]
    pub ready: bool,
    #[arg(long)]
    pub from: Vec<String>,
    #[arg(long)]
    pub lease: Option<String>,
    #[arg(long)]
    pub wait: Option<String>,
    #[arg(long)]
    pub epoch: Option<u64>,
    /// A supplied claim secret. Prefer the private claim cache between CLI invocations.
    #[arg(long, hide = true)]
    pub token: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub pool: bool,
    #[arg(long)]
    pub can: Vec<String>,
    #[arg(long)]
    pub user: Vec<String>,
    #[arg(long)]
    pub project: Vec<String>,
    #[arg(long)]
    pub github: Vec<String>,
    #[arg(long)]
    pub oidc: Vec<String>,
    #[arg(long)]
    pub app: Vec<String>,
    #[arg(long)]
    pub everyone: bool,
    #[arg(long)]
    pub all_users: bool,
    #[arg(long)]
    pub all_projects: bool,
    #[arg(long)]
    pub all_apps: bool,
    #[arg(long)]
    pub environment: Vec<String>,
    #[arg(long)]
    pub workflow: Option<String>,
    #[arg(long)]
    pub git_branch: Option<String>,
    #[arg(long)]
    pub event: Option<String>,
    #[arg(long = "claim-value")]
    pub claim_value: Vec<String>,
    #[arg(long)]
    pub lock_me_out: bool,
    #[arg(long)]
    pub description: Option<String>,
    #[arg(long)]
    pub schema: Option<String>,
    #[arg(long)]
    pub version: Option<u64>,
    #[arg(long)]
    pub lease_id: Option<String>,
    #[arg(long)]
    pub error: Option<String>,
    #[arg(long)]
    pub skip: bool,
    #[arg(long)]
    pub result: Option<String>,
}

#[derive(Args)]
pub struct ObjectArgs {
    /// list/create/read/update/revoke/explain/register/matches/ack/cancel.
    pub action: Option<String>,
    /// Resource ID, channel path, or schema name.
    pub id: Option<String>,
    #[command(flatten)]
    pub options: Options,
}

#[derive(Args)]
pub struct TaskArgs {
    /// create/claim/renew/release/complete/fail/block/unblock/reopen/assign/needs/cancel/batch, or a channel to list.
    pub action: Option<String>,
    /// Task ID, or channel for create/claim --ready.
    pub target: Option<String>,
    /// Task text, result, failure reason, or assigned actor.
    pub text: Option<String>,
    /// Additional prerequisite IDs, or command arguments for `tasks run ... -- COMMAND`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub remaining: Vec<String>,
    #[command(flatten)]
    pub options: Options,
}

#[derive(Args)]
pub struct InboxArgs {
    pub key: String,
    /// list/push/next/ack.
    pub action: Option<String>,
    pub id: Option<String>,
    #[command(flatten)]
    pub options: Options,
}

#[derive(Args)]
pub struct ServeArgs {
    /// Exact listen address, including port (for example 100.64.0.1:4100).
    #[arg(long)]
    pub bind: Option<std::net::SocketAddr>,
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    #[arg(long, default_value_t = 4100)]
    pub port: u16,
    #[arg(long, env = "ENGAGE_DATABASE_URL")]
    pub database: Option<String>,
    #[arg(long, default_value = ".engage")]
    pub dir: PathBuf,
    #[arg(long)]
    pub no_inspect: bool,
    #[arg(long)]
    pub inspector_dir: Option<PathBuf>,
    #[arg(long, env = "ENGAGE_PUBLIC_URL")]
    pub public_url: Option<String>,
    #[arg(long, env = "ENGAGE_OIDC_ISSUER")]
    pub oidc_issuer: Option<String>,
    #[arg(long, env = "ENGAGE_OIDC_AUDIENCE")]
    pub oidc_audience: Option<String>,
    #[arg(long, env = "ENGAGE_OIDC_JWKS_URL")]
    pub oidc_jwks_url: Option<String>,
    #[arg(long, env = "ENGAGE_S3_BUCKET")]
    pub s3_bucket: Option<String>,
    #[arg(long, env = "ENGAGE_S3_REGION", default_value = "us-east-1")]
    pub s3_region: String,
    #[arg(long, env = "ENGAGE_S3_ENDPOINT")]
    pub s3_endpoint: Option<String>,
    /// Provider configuration JSON; credentials are referenced by environment-variable name.
    #[arg(long, env = "ENGAGE_SYNC_CONFIG")]
    pub sync_config: Option<PathBuf>,
    /// Authenticate peers through the local tailscaled WhoIs API.
    #[arg(long, conflicts_with = "oidc_issuer")]
    pub tailscale: bool,
    #[arg(long, default_value = "/var/run/tailscaled.socket")]
    pub tailscale_socket: PathBuf,
    /// Tailnet login permitted to administer the board; repeat for additional owners.
    #[arg(long)]
    pub tailscale_owner: Vec<String>,
    /// Permit watcher callbacks to private networks and loopback HTTP development servers.
    #[arg(long, env = "ENGAGE_ALLOW_PRIVATE_WAKE")]
    pub allow_private_wake: bool,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Show connection metadata without stored tokens.
    Show,
    /// Save a connection; --token-stdin reads a credential without shell-history exposure.
    Add {
        name: String,
        url: String,
        #[arg(long)]
        actor: Option<String>,
        #[arg(long)]
        token_stdin: bool,
    },
    /// Select the default saved connection.
    Use { name: String },
    /// Remove a saved connection.
    Remove { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_claim_and_global_options_parse() {
        let cli = Cli::try_parse_from([
            "engage", "tasks", "claim", "work/", "--ready", "--lease", "15m", "--as", "adam",
            "--json",
        ])
        .expect("valid task claim");
        assert_eq!(cli.actor.as_deref(), Some("adam"));
        assert!(cli.json);
        let Command::Tasks(task) = cli.command else {
            panic!("expected tasks")
        };
        assert_eq!(task.action.as_deref(), Some("claim"));
        assert!(task.options.ready);
    }
    #[test]
    fn flags_do_not_accept_ambiguous_claim_pairs() {
        assert!(Cli::try_parse_from(["engage", "react", "12", "thumbs_up", "--remove"]).is_ok());
        assert!(
            Cli::try_parse_from(["engage", "api", "POST", "/messages", "--data", "{}"]).is_ok()
        );
    }
    #[test]
    fn command_runner_preserves_program_arguments_after_separator() {
        let cli = Cli::try_parse_from([
            "engage",
            "tasks",
            "run",
            "12",
            "--result",
            "passed",
            "--",
            "cargo",
            "test",
            "--offline",
        ])
        .expect("valid command runner");
        let Command::Tasks(task) = cli.command else {
            panic!("expected tasks")
        };
        assert_eq!(task.text.as_deref(), Some("cargo"));
        assert_eq!(task.remaining, ["test", "--offline"]);
        assert_eq!(task.options.result.as_deref(), Some("passed"));
    }
}
