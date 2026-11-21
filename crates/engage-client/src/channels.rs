// Adapted from vercel/board client/client.ts at fef19e5, Copyright 2026 Vercel Inc.
// Rust implementation and HTTP transport Copyright 2026 GetHamster contributors.
use serde_json::{Value, json};

use crate::{Client, ClientError};

impl Client {
    /// Creates a channel before posting. Existing channels are returned unchanged.
    ///
    /// # Errors
    /// Returns a protocol error when the caller cannot read and post in the channel.
    pub async fn create_channel(&self, path: &str) -> Result<Value, ClientError> {
        self.write("/channels", &json!({"path":path})).await
    }

    /// Creates a channel and subtree readable only by its creator.
    ///
    /// Like Board's client, this uses grants before creating the channel. It refuses
    /// existing channels or grants that reach the subtree, and checks concurrent grant
    /// changes before publishing. Interruption may leave a closed, uncreated channel
    /// with its grants retained, but never intentionally opens a private conversation.
    ///
    /// # Errors
    /// Returns `channel_exists`, `channel_granted`, or `no_own_principal`, as well as
    /// transport and permission errors. Principals other than users and Vercel projects
    /// must create their own explicit grants instead.
    pub async fn create_private_channel(&self, path: &str) -> Result<Value, ClientError> {
        let me = self.me().await?;
        let principal = me["principal"].as_str().unwrap_or_default();
        let parts: Vec<_> = principal.split(':').collect();
        let creator = match parts.as_slice() {
            [source, "user", rest @ ..]
                if ["local", "vercel"].contains(source) && !rest.is_empty() =>
            {
                json!({"kind":"user","userId":rest.join(":")})
            }
            ["vercel", "project", project, environment] => {
                json!({"kind":"project","project":project,"environments":[environment]})
            }
            _ => {
                return Err(failure(
                    400,
                    "no_own_principal",
                    "a private channel requires a user or project principal; create explicit grants for this identity",
                ));
            }
        };
        if !self.reaching(path).await?.is_empty() {
            return Err(failure(
                409,
                "channel_granted",
                "a grant already reaches this channel or subtree",
            ));
        }
        let pattern = format!("{path}/");
        for prefix in [path, &pattern] {
            let page = self
                .channels(&[
                    ("prefix".into(), prefix.into()),
                    ("limit".into(), "1".into()),
                ])
                .await?;
            if page["channels"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|channel| {
                    channel["path"]
                        .as_str()
                        .is_some_and(|value| value == path || value.starts_with(&pattern))
                })
            {
                return Err(failure(
                    409,
                    "channel_exists",
                    "this channel or a child channel already exists",
                ));
            }
        }
        let own = self
            .write(
                "/grants",
                &json!({"principals":[creator],"can":{&pattern:"all"}}),
            )
            .await?["grant"]
            .clone();
        if others(&self.reaching(path).await?, std::slice::from_ref(&own)) {
            self.take_back(&own).await?;
            return Err(failure(
                409,
                "channel_granted",
                "a concurrent grant reaches this channel; nothing was created",
            ));
        }
        let closed = self
            .write(
                "/grants",
                &json!({"principals":[{"kind":"everyone"}],"can":{&pattern:[]}}),
            )
            .await?["grant"]
            .clone();
        if others(&self.reaching(path).await?, &[own.clone(), closed.clone()]) {
            self.take_back(&closed).await?;
            self.take_back(&own).await?;
            return Err(failure(
                409,
                "channel_granted",
                "a grant changed while making the channel private; nothing was created",
            ));
        }
        let mut result = self.create_channel(path).await?;
        result["grants"] = json!([own, closed]);
        Ok(result)
    }

    async fn reaching(&self, path: &str) -> Result<Vec<Value>, ClientError> {
        let grants = self.request("GET", "/grants", &[], None).await?;
        let pattern = format!("{path}/");
        Ok(grants["grants"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|grant| {
                grant["can"]
                    .as_object()
                    .is_some_and(|can| can.keys().any(|at| at == path || at.starts_with(&pattern)))
            })
            .cloned()
            .collect())
    }

    async fn take_back(&self, grant: &Value) -> Result<(), ClientError> {
        let id = grant["id"]
            .as_str()
            .ok_or_else(|| failure(502, "invalid_response", "grant response has no ID"))?;
        let path = format!("/grants/{}/revoke", Self::segment(id));
        match self.write(&path, &json!({})).await {
            Err(ClientError::Api { code, .. }) if code == "grant_lockout" => {
                self.write(&path, &json!({"lockMeOut":true})).await?;
            }
            result => {
                result?;
            }
        }
        Ok(())
    }
}

fn others(grants: &[Value], mine: &[Value]) -> bool {
    grants.iter().any(|grant| {
        !mine.iter().any(|own| {
            own["id"] == grant["id"]
                && own["principals"] == grant["principals"]
                && own["can"] == grant["can"]
        })
    })
}

fn failure(status: u16, code: &str, message: &str) -> ClientError {
    ClientError::Api {
        status,
        code: code.into(),
        message: message.into(),
        hint: None,
    }
}
