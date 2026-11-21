//! Configuration references credential environment variables, never credential values.
use crate::{EnvironmentCredentials, GitHub, Linear, Registry, Slack, SyncError};
use serde::Deserialize;
use std::collections::BTreeMap;

/// A standalone service's named synchronization providers.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncConfig {
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub key: String,
    pub platform: String,
    pub principal: String,
    pub token_env: String,
    pub signing_secret_env: String,
    pub bot_id_env: Option<String>,
    #[serde(default)]
    pub channels: BTreeMap<String, String>,
    /// GitHub outbound writes are disabled unless explicitly enabled.
    #[serde(default)]
    pub outbound: bool,
    #[serde(default)]
    pub allow_join: bool,
    /// Linear personal API keys use raw Authorization rather than OAuth Bearer.
    #[serde(default)]
    pub api_key: bool,
}

impl Registry {
    /// Builds providers without resolving secrets until their first operation.
    ///
    /// # Errors
    /// Rejects malformed keys, duplicate registrations, unknown platforms and routes.
    pub fn from_config(config: &SyncConfig) -> Result<Self, SyncError> {
        let mut registry = Self::new();
        for provider in &config.providers {
            for name in [&provider.token_env, &provider.signing_secret_env]
                .into_iter()
                .chain(provider.bot_id_env.iter())
            {
                if name.is_empty()
                    || !name.chars().enumerate().all(|(i, c)| {
                        c == '_' || c.is_ascii_alphabetic() || i > 0 && c.is_ascii_digit()
                    })
                {
                    return Err(SyncError::Payload(
                        "invalid credential environment variable name".into(),
                    ));
                }
            }
            if provider.principal.is_empty() || registry.entries.contains_key(&provider.key) {
                return Err(SyncError::Payload(
                    "a sync registration needs a unique key and principal".into(),
                ));
            }
            let credentials = EnvironmentCredentials {
                token_variable: provider.token_env.clone(),
                signing_secret_variable: provider.signing_secret_env.clone(),
                bot_id_variable: provider.bot_id_env.clone(),
            };
            match provider.platform.as_str() {
                "slack" => registry.register(
                    &provider.key,
                    &provider.principal,
                    Slack::new(credentials).with_join(provider.allow_join),
                )?,
                "github" => registry.register(
                    &provider.key,
                    &provider.principal,
                    GitHub::new(credentials).with_outbound(provider.outbound),
                )?,
                "linear" => {
                    let linear = Linear::new(credentials);
                    registry.register(
                        &provider.key,
                        &provider.principal,
                        if provider.api_key {
                            linear.with_api_key()
                        } else {
                            linear
                        },
                    )?;
                }
                _ => return Err(SyncError::Payload("unknown sync platform".into())),
            }
            for (external, board) in &provider.channels {
                registry.map_channel(&provider.key, external, board)?;
            }
        }
        Ok(registry)
    }

    /// Maps an external container (without its sync-key prefix) into a board channel.
    ///
    /// # Errors
    /// Rejects an unknown provider or an invalid board channel.
    pub fn map_channel(
        &mut self,
        key: &str,
        external: impl Into<String>,
        board: impl Into<String>,
    ) -> Result<(), SyncError> {
        let external = external.into();
        let board = board.into();
        if external.is_empty()
            || external.len() > 300
            || board.is_empty()
            || board.len() > 500
            || !board.split('/').all(|part| {
                !part.is_empty()
                    && part.chars().enumerate().all(|(i, c)| {
                        c.is_ascii_lowercase()
                            || c.is_ascii_digit()
                            || i > 0 && matches!(c, '_' | '-')
                    })
            })
        {
            return Err(SyncError::Payload(
                "invalid synchronization channel mapping".into(),
            ));
        }
        self.entries
            .get_mut(key)
            .ok_or_else(|| SyncError::UnknownProvider(key.into()))?
            .channels
            .insert(external, board);
        Ok(())
    }
}
