use anyhow::{Context, Result, bail};
use engage_client::Client;
use serde_json::{Map, Value, json};

use crate::args::{ObjectArgs, Options};
use crate::util::{body_input, labels, optional, query_options};

pub async fn channels(client: &Client, args: &ObjectArgs) -> Result<Value> {
    let action = args.action.as_deref().unwrap_or("list");
    if action == "create" {
        let path = args.id.as_deref().context("channels create needs a path")?;
        return Ok(if args.options.private {
            client.create_private_channel(path).await?
        } else {
            client.create_channel(path).await?
        });
    }
    let mut query = query_options(&args.options);
    if let Some(prefix) = if action == "list" {
        args.id.as_deref()
    } else {
        Some(action)
    } {
        query.push(("prefix".into(), prefix.into()));
    }
    Ok(client.channels(&query).await?)
}

pub async fn grants(client: &Client, args: &ObjectArgs) -> Result<Value> {
    let options = &args.options;
    match args.action.as_deref().unwrap_or("list") {
        "list" => Ok(client.request("GET", "/grants", &[], None).await?),
        "create" => {
            let mut body = if options.data.is_some() || options.file.is_some() {
                body_input(options)?
            } else {
                json!({"principals":principals(options)?,"can":permissions(&options.can)?})
            };
            if body.is_array() {
                body = json!({"grants":body});
            }
            if options.lock_me_out {
                body["lockMeOut"] = true.into();
            }
            let route = if body.get("grants").is_some() {
                "/grants/batch"
            } else {
                "/grants"
            };
            Ok(client.write(route, &body).await?)
        }
        "update" => {
            let mut body = body_input(options)?;
            if options.lock_me_out {
                body["lockMeOut"] = true.into();
            }
            Ok(client
                .request(
                    "PATCH",
                    &format!(
                        "/grants/{}",
                        Client::segment(args.id.as_deref().context("update needs a grant ID")?)
                    ),
                    &[],
                    Some(&body),
                )
                .await?)
        }
        "revoke" => Ok(client
            .write(
                &format!(
                    "/grants/{}/revoke",
                    Client::segment(args.id.as_deref().context("revoke needs a grant ID")?)
                ),
                &json!({"lockMeOut":options.lock_me_out}),
            )
            .await?),
        "explain" => {
            let mut query = vec![(
                "channel".into(),
                args.id
                    .as_deref()
                    .context("explain needs a channel")?
                    .into(),
            )];
            for (key, values) in [
                ("user", &options.user),
                ("project", &options.project),
                ("github", &options.github),
                ("oidc", &options.oidc),
                ("app", &options.app),
            ] {
                for value in values {
                    query.push((key.into(), value.clone()));
                }
            }
            for (key, value) in [
                ("workflow", &options.workflow),
                ("branch", &options.git_branch),
                ("event", &options.event),
            ] {
                if let Some(value) = value {
                    query.push((key.into(), value.clone()));
                }
            }
            for claim in &options.claim_value {
                query.push(("claim".into(), claim.clone()));
            }
            if let Some(environment) = options.environment.first() {
                query.push(("environment".into(), environment.clone()));
            }
            Ok(client
                .request("GET", "/grants/explain", &query, None)
                .await?)
        }
        other => bail!("unknown grants operation {other}; use list/create/update/explain/revoke"),
    }
}

fn principals(options: &Options) -> Result<Vec<Value>> {
    let mut principals = Vec::new();
    for user in &options.user {
        principals.push(if user.contains('@') {
            json!({"kind":"user","email":user})
        } else {
            json!({"kind":"user","userId":user})
        });
    }
    for project in &options.project {
        let mut principal = json!({"kind":"project","project":project});
        if !options.environment.is_empty() {
            principal["environments"] = json!(
                options
                    .environment
                    .iter()
                    .flat_map(|s| s.split(','))
                    .collect::<Vec<_>>()
            );
        }
        principals.push(principal);
    }
    for repository in &options.github {
        let mut principal = json!({"kind":"github","repository":repository});
        optional(&mut principal, "workflow", options.workflow.as_ref());
        optional(&mut principal, "branch", options.git_branch.as_ref());
        optional(&mut principal, "event", options.event.as_ref());
        principals.push(principal);
    }
    for issuer in &options.oidc {
        principals
            .push(json!({"kind":"oidc","issuer":issuer,"claims":labels(&options.claim_value)?}));
    }
    for app in &options.app {
        principals.push(json!({"kind":"app","app":app}));
    }
    for (enabled, kind) in [
        (options.everyone, "everyone"),
        (options.all_users, "all-users"),
        (options.all_projects, "all-projects"),
        (options.all_apps, "all-apps"),
    ] {
        if enabled {
            principals.push(json!({"kind":kind}));
        }
    }
    Ok(principals)
}

fn permissions(values: &[String]) -> Result<Value> {
    let mut result = Map::new();
    for value in values {
        let (pattern, permissions) = value
            .split_once('=')
            .context("permissions look like CHANNEL=read,post or *=all")?;
        result.insert(
            pattern.into(),
            match permissions {
                "all" => json!("all"),
                "none" => json!([]),
                value => json!(value.split(',').collect::<Vec<_>>()),
            },
        );
    }
    if result.is_empty() {
        bail!("a grant needs at least one --can pattern=permissions");
    }
    Ok(result.into())
}

pub async fn schemas(client: &Client, args: &ObjectArgs) -> Result<Value> {
    match args.action.as_deref().unwrap_or("list") {
        "list" => Ok(client.request("GET", "/schemas", &[], None).await?),
        "read" => Ok(client
            .request(
                "GET",
                &format!(
                    "/schemas/{}",
                    Client::segment(args.id.as_deref().context("read needs name@version")?)
                ),
                &[],
                None,
            )
            .await?),
        "register" | "create" => {
            let mut body = body_input(&args.options)?;
            if body.get("schema").is_none() {
                let name = args
                    .id
                    .as_deref()
                    .context("register needs a name, --version, and --file schema.json")?;
                body = json!({"name":name,"version":args.options.version.context("register needs --version")?,"schema":body});
                optional(&mut body, "description", args.options.description.as_ref());
            }
            Ok(client.write("/schemas", &body).await?)
        }
        id => Ok(client
            .request(
                "GET",
                &format!("/schemas/{}", Client::segment(id)),
                &[],
                None,
            )
            .await?),
    }
}

pub async fn schedules(client: &Client, args: &ObjectArgs) -> Result<Value> {
    if args.action.as_deref() == Some("cancel") {
        return Ok(client
            .write(
                &format!(
                    "/schedules/{}/cancel",
                    Client::segment(args.id.as_deref().context("cancel needs a schedule ID")?)
                ),
                &json!({}),
            )
            .await?);
    }
    let mut query = query_options(&args.options);
    if let Some(channel) = args.action.as_deref().filter(|s| *s != "list") {
        query.push(("channel".into(), channel.into()));
    }
    Ok(client.request("GET", "/schedules", &query, None).await?)
}

pub async fn watchers(client: &Client, args: &ObjectArgs) -> Result<Value> {
    match args.action.as_deref().unwrap_or("list") {
        "list" => Ok(client.request("GET", "/watchers", &[], None).await?),
        "register" => Ok(client
            .write("/watchers", &body_input(&args.options)?)
            .await?),
        "matches" => {
            let mut query = query_options(&args.options);
            if let Some(wait) = &args.options.wait {
                query.push(("wait".into(), crate::util::duration(wait)?.to_string()));
            }
            Ok(client
                .request("GET", "/watchers/matches", &query, None)
                .await?)
        }
        "ack" => {
            let mut body = if args.options.file.is_some() || args.options.data.is_some() {
                body_input(&args.options)?
            } else {
                json!({"lease":args.options.lease_id.as_deref().context("ack needs --lease-id or --data")?})
            };
            optional(&mut body, "error", args.options.error.as_ref());
            if args.options.skip {
                body["skip"] = true.into();
            }
            Ok(client
                .write(
                    &format!(
                        "/watchers/matches/{}/ack",
                        Client::segment(args.id.as_deref().context("ack needs a match ID")?)
                    ),
                    &body,
                )
                .await?)
        }
        other => bail!("unknown watcher operation {other}; use list/register/matches/ack"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permission_lists_preserve_explicit_denials() -> Result<()> {
        let result = permissions(&["*=read,post".into(), "private/=none".into()])?;
        assert_eq!(result["private/"], json!([]));
        assert_eq!(result["*"], json!(["read", "post"]));
        Ok(())
    }
}
