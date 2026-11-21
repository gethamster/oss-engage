use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use engage_client::Client;
use serde_json::{Value, json};

use crate::args::{Cli, Command, ConfigCommand, Options, PostArgs, ReadArgs, ServeArgs};
use crate::config::{Config, Profile};
use crate::{admin, tasks, util};

pub async fn execute(cli: &Cli) -> Result<()> {
    let directory = crate::config::dir()?;
    let path = directory.join("config.json");
    let mut config = Config::load(&path)?;
    if let Command::Config { command } = &cli.command {
        return configure(command, &mut config, &path, cli.json);
    }
    if let Command::Serve(args) = &cli.command {
        return serve(args).await;
    }
    let client = config.client(
        cli.url.as_deref(),
        cli.profile.as_deref(),
        cli.actor.as_deref(),
    )?;
    let value = match &cli.command {
        Command::Info => client.discovery().await?,
        Command::Me => client.me().await?,
        Command::Actors { id } => client.request("GET", "/actors", &id.iter().map(|id| ("id".into(),id.clone())).collect::<Vec<_>>(), None).await?,
        Command::Channels(args) => admin::channels(&client, args).await?,
        Command::Post(args) => client.write("/messages", &post_body(&client,args).await?).await?,
        Command::Schedule(args) => {
            let mut body = post_body(&client,args).await?;
            for (name, value) in [("in", &args.options.delay), ("at", &args.options.at), ("every", &args.options.every), ("until", &args.options.until), ("for", &args.options.for_value)] { util::optional(&mut body, name, value.as_ref()); }
            util::optional(&mut body, "times", args.options.times.as_ref());
            client.write("/schedules", &body).await?
        }
        Command::Reply(args) => {
            let text = if args.text.is_none() && args.options.file.is_none() && !args.options.attach.is_empty() { String::new() } else { util::read_text(args.text.as_deref(),args.options.file.as_deref())? };
            let mut body = json!({"re":args.id,"text":text});
            decorate_post(&client,&mut body,&args.options).await?;
            client.write("/messages", &body).await?
        }
        Command::Read(args) => read(&client,args,false).await?,
        Command::Threads(args) => read(&client,args,true).await?,
        Command::Search { text, filters } => {
            let mut query = util::filters(filters); query.push(("text".into(),text.clone()));
            client.threads(&query).await?
        }
        Command::Edit { id,text,options } => {
            let mut body = json!({"text":util::read_text(text.as_deref(),options.file.as_deref())?});
            util::optional(&mut body,"revision",options.if_revision.as_ref());
            client.message_action(id,"edit",&body).await?
        }
        Command::Delete { id,options } => {
            let mut body = json!({}); util::optional(&mut body,"revision",options.if_revision.as_ref());
            client.message_action(id,"delete",&body).await?
        }
        Command::React { id,emoji,remove } => client.message_action(id,"reactions",&json!({"emoji":emoji,"remove":remove})).await?,
        Command::Label { id,labels,options } => {
            let mut body = json!({"set":util::labels(labels)?,"unset":options.unset});
            if !options.conditions.is_empty() || !options.if_unset.is_empty() {
                let mut condition = util::labels(&options.conditions)?;
                for key in &options.if_unset { condition[key] = Value::Null; }
                body["if"] = condition;
            }
            client.message_action(id,"labels",&body).await?
        }
        Command::Tail(args) => { follow(&client,args,false,cli.json).await?; return Ok(()); }
        Command::Wait(args) => { follow(&client,args,true,cli.json).await?; return Ok(()); }
        Command::Tasks(args) => tasks::execute(&client,args,directory).await?,
        Command::Schedules(args) => admin::schedules(&client,args).await?,
        Command::Grants(args) => admin::grants(&client,args).await?,
        Command::Schemas(args) => admin::schemas(&client,args).await?,
        Command::Watchers(args) => admin::watchers(&client,args).await?,
        Command::Inbox(args) => {
            let base = format!("/inbox/{}",Client::segment(&args.key));
            match args.action.as_deref().unwrap_or("list") {
                "list" => client.request("GET",&base,&util::query_options(&args.options),None).await?,
                "push" => client.write(&base,&util::body_input(&args.options)?).await?,
                "next" => {
                    let mut query = util::query_options(&args.options);
                    if let Some(wait) = &args.options.wait { query.push(("wait".into(),util::duration(wait)?.to_string())); }
                    client.request("GET",&format!("{base}/next"),&query,None).await?
                }
                "ack" => {
                    let mut body = if args.options.data.is_some() || args.options.file.is_some() { util::body_input(&args.options)? } else { json!({"lease":args.options.lease_id.as_deref().context("ack needs --lease-id")?}) };
                    util::optional(&mut body,"error",args.options.error.as_ref());
                    client.write(&format!("{base}/{}/ack",Client::segment(args.id.as_deref().context("ack needs an inbox ID")?)),&body).await?
                }
                other => bail!("unknown inbox operation {other}"),
            }
        }
        Command::Sync { id,target } => {
            let path = format!("/messages/{}/sync",Client::segment(id));
            if let Some(target) = target { client.write(&path,&json!({"target":target})).await? } else { client.request("GET",&path,&[],None).await? }
        }
        Command::Sources { key,action,target,options } => match (key,action.as_deref()) {
            (None,_) => client.discovery().await?["sync"].clone(),
            (Some(key),Some("read")) => {
                let mut query = util::query_options(options); query.push(("target".into(),target.as_deref().context("sources KEY read needs a target")?.into()));
                client.request("GET",&format!("/sources/{}/read",Client::segment(key)),&query,None).await?
            }
            (Some(key),Some("join")) => client.write(&format!("/sources/{}/join",Client::segment(key)),&json!({"target":target.as_deref().context("sources KEY join needs a channel")?})).await?,
            (Some(key),None) => { let discovery = client.discovery().await?; json!(discovery["sync"].as_array().into_iter().flatten().filter(|item| item["key"].as_str() == Some(key)).collect::<Vec<_>>()) }
            _ => bail!("sources takes KEY read TARGET or KEY join CHANNEL"),
        },
        Command::Typing { id,status } => { let mut body = json!({}); util::optional(&mut body,"status",status.as_ref()); client.message_action(id,"typing",&body).await? }
        Command::Upload { path,content_type } => client.upload(path.file_name().context("upload needs a filename")?.to_str().context("filename is not UTF-8")?,content_type,tokio::fs::read(path).await?).await?,
        Command::Download { id,path } => {
            let bytes = client.download(id).await?;
            if path == Path::new("-") { std::io::stdout().lock().write_all(&bytes)?; return Ok(()); }
            tokio::fs::write(path,&bytes).await?; json!({"path":path,"bytes":bytes.len()})
        }
        Command::Inspect { no_open } => {
            crate::inspect::serve(client,!no_open).await?;
            return Ok(());
        }
        Command::Api { method,path,query,data,file } => {
            let body = if let Some(data) = data { Some(serde_json::from_str(data)?) } else if let Some(file) = file { Some(serde_json::from_str(&util::read_text(None,Some(file))?)?) } else { None };
            client.request(&method.to_uppercase(),path,&util::pairs(query)?,body.as_ref()).await?
        }
        Command::Serve(_) | Command::Config { .. } => unreachable!("local commands handled before connecting"),
    };
    util::output(&value, cli.json)
}

async fn read(client: &Client, args: &ReadArgs, summaries: bool) -> Result<Value> {
    let mut query = util::filters(&args.filters);
    let positional_id = args
        .channel
        .as_ref()
        .filter(|value| value.starts_with('#') || value.chars().all(|c| c.is_ascii_digit()));
    if let Some(re) = args.filters.re.as_ref().or(positional_id) {
        let re = re.trim_start_matches('#');
        return Ok(client
            .request(
                "GET",
                &format!("/messages/{}", Client::segment(re)),
                &query
                    .iter()
                    .filter(|(key, _)| ["after", "limit"].contains(&key.as_str()))
                    .cloned()
                    .collect::<Vec<_>>(),
                None,
            )
            .await?);
    }
    if let Some(channel) = &args.channel {
        query.push(("channel".into(), channel.clone()));
    }
    if summaries
        || args.channel.is_none()
        || args.filters.by.is_some()
        || !args.filters.label.is_empty()
        || args.filters.text.is_some()
        || args.filters.source.is_some()
        || args.filters.mentions.is_some()
    {
        return Ok(client.threads(&query).await?);
    }
    Ok(client.request("GET", "/messages", &query, None).await?)
}

async fn post_body(client: &Client, args: &PostArgs) -> Result<Value> {
    if args.options.data.is_some() {
        return util::body_input(&args.options);
    }
    let text =
        if args.text.is_none() && args.options.file.is_none() && !args.options.attach.is_empty() {
            String::new()
        } else {
            util::read_text(args.text.as_deref(), args.options.file.as_deref())?
        };
    let mut body = json!({"text":text});
    if let Some(re) = &args.options.re {
        body["re"] = re.clone().into();
    } else {
        body["channel"] = args
            .channel
            .as_deref()
            .context("post needs a channel, or schedule needs --re")?
            .into();
    }
    decorate_post(client, &mut body, &args.options).await?;
    Ok(body)
}

async fn decorate_post(client: &Client, body: &mut Value, options: &Options) -> Result<()> {
    if !options.label.is_empty() {
        body["labels"] = util::labels(&options.label)?;
    }
    util::optional(body, "id", options.idempotency_key.as_ref());
    util::optional(body, "after", options.after.as_ref());
    if options.no_sync {
        body["noSync"] = true.into();
    }
    if !options.sync.is_empty() {
        body["sync"] = json!(options.sync);
    }
    if !options.attach.is_empty() {
        body["attachments"] = json!(upload_attachments(client, &options.attach).await?);
    }
    if let Some(schema) = &options.schema {
        body["text"] = format!(
            "```json#{schema}\n{}\n```",
            body["text"].as_str().unwrap_or_default()
        )
        .into();
    }
    Ok(())
}

pub async fn upload_attachments(
    client: &Client,
    paths: &[std::path::PathBuf],
) -> Result<Vec<String>> {
    let mut attachments = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .context("attachment needs a file name")?
            .to_str()
            .context("attachment name is not UTF-8")?;
        let result = client
            .upload(
                name,
                "application/octet-stream",
                tokio::fs::read(path).await?,
            )
            .await?;
        attachments.push(
            result["attachment"]["id"]
                .as_str()
                .context("upload response has no attachment ID")?
                .into(),
        );
    }
    Ok(attachments)
}

async fn follow(client: &Client, args: &ReadArgs, once: bool, json_output: bool) -> Result<()> {
    let mut query = util::filters(&args.filters);
    if let Some(channel) = &args.channel {
        query.push(("channel".into(), channel.clone()));
    }
    if args.all {
        query.push(("all".into(), "true".into()));
    }
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs_f64(util::duration(&args.timeout)?);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if once && remaining.is_zero() {
            bail!("wait timed out without a matching event");
        }
        let mut poll = query.clone();
        if !poll.iter().any(|(key, _)| key == "last") {
            poll.push((
                "wait".into(),
                if once {
                    remaining.as_secs_f64().min(25.0).to_string()
                } else {
                    "25".into()
                },
            ));
        }
        let page = tokio::select! {
            result = client.events(&poll) => result?,
            _ = tokio::signal::ctrl_c() => return Ok(()),
        };
        let mut matched = false;
        for event in &page.events {
            matched = true;
            if json_output {
                println!("{}", serde_json::to_string(event)?);
            } else if let Some(message) = &event.message {
                println!("{} #{} {}", event.kind, message.id, message.text);
            } else {
                println!("{} {}", event.cursor, event.kind);
            }
        }
        query.retain(|(key, _)| key != "after" && key != "last");
        query.push(("after".into(), page.cursor.clone()));
        if once && matched {
            util::output(&json!({"cursor":page.cursor}), json_output)?;
            return Ok(());
        }
        if !matched {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn configure(
    command: &ConfigCommand,
    config: &mut Config,
    path: &Path,
    json_output: bool,
) -> Result<()> {
    match command {
        ConfigCommand::Show => return util::output(&config.redacted(), json_output),
        ConfigCommand::Add {
            name,
            url,
            actor,
            token_stdin,
        } => {
            Client::new(url)?;
            if name.trim().is_empty() {
                bail!("a configuration name cannot be empty");
            }
            let token = if *token_stdin {
                let mut value = String::new();
                std::io::stdin().read_to_string(&mut value)?;
                let value = value.trim().to_owned();
                if value.is_empty() {
                    bail!("stdin contained no credential");
                }
                Some(value)
            } else {
                None
            };
            config.profiles.insert(
                name.clone(),
                Profile {
                    url: url.clone(),
                    token,
                    actor: actor.clone(),
                },
            );
            if config.active.is_none() {
                config.active = Some(name.clone());
            }
        }
        ConfigCommand::Use { name } => {
            if !config.profiles.contains_key(name) {
                bail!("unknown configuration {name}");
            }
            config.active = Some(name.clone());
        }
        ConfigCommand::Remove { name } => {
            if config.profiles.remove(name).is_none() {
                bail!("unknown configuration {name}");
            }
            if config.active.as_deref() == Some(name) {
                config.active = None;
            }
        }
    }
    config.save(path)?;
    util::output(&config.redacted(), json_output)
}

async fn serve(args: &ServeArgs) -> Result<()> {
    use engage_server::{AuthConfig, BlobConfig, OidcConfig, ServerConfig};
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    tokio::fs::create_dir_all(&args.dir).await?;
    let database = args
        .database
        .clone()
        .unwrap_or_else(|| format!("sqlite://{}?mode=rwc", args.dir.join("board.db").display()));
    let mut config = ServerConfig {
        bind: args.bind.unwrap_or(std::net::SocketAddr::new(
            args.host.parse().context("--host must be an IP address")?,
            args.port,
        )),
        public_url: args.public_url.clone(),
        ..ServerConfig::default()
    };
    if args.tailscale {
        config.auth = AuthConfig::Tailscale {
            socket: args.tailscale_socket.clone(),
            owner_logins: args.tailscale_owner.clone(),
        };
    } else if let Some(issuer) = &args.oidc_issuer {
        config.auth = AuthConfig::Oidc(OidcConfig {
            issuer: issuer.clone(),
            audience: args
                .oidc_audience
                .clone()
                .context("OIDC requires --oidc-audience")?,
            jwks_url: args
                .oidc_jwks_url
                .clone()
                .context("OIDC requires --oidc-jwks-url")?,
            owner_subjects: std::env::var("ENGAGE_OIDC_OWNERS")
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        });
    } else if let Ok(token) = std::env::var("ENGAGE_SERVER_TOKEN") {
        config.auth = AuthConfig::Bearer {
            token,
            principal: engage_core::Principal::default(),
        };
    }
    config.session_key = std::env::var("ENGAGE_SESSION_KEY").ok();
    config.wake_allow_private = args.allow_private_wake;
    if let Some(path) = &args.sync_config {
        let provider_config: engage_sync::SyncConfig =
            serde_json::from_slice(&tokio::fs::read(path).await?)
                .context("reading sync provider configuration")?;
        config.sync = Some(engage_sync::Registry::from_config(&provider_config)?);
    }
    config.blobs = if let Some(bucket) = &args.s3_bucket {
        BlobConfig::S3 {
            bucket: bucket.clone(),
            region: args.s3_region.clone(),
            endpoint: args.s3_endpoint.clone(),
        }
    } else {
        BlobConfig::Filesystem(args.dir.join("blobs"))
    };
    if args.no_inspect {
        config.inspector_dir = None;
    } else if let Some(path) = &args.inspector_dir {
        config.inspector_dir = Some(path.clone());
    }
    let board = engage_store::Board::connect(&database).await?;
    eprintln!("Engage listening at http://{}", config.bind);
    engage_server::serve(board, config, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}

pub fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    if !command.arg(url).status()?.success() {
        bail!("could not open the browser; open {url}");
    }
    Ok(())
}
