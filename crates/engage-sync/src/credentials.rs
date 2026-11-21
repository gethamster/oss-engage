//! Resolve credentials at use time so rotation and external brokers need no restart.
use crate::SyncError;
use async_trait::async_trait;
use std::{fmt, sync::Arc};
use zeroize::Zeroize;

/// A credential whose Debug representation is redacted and memory erased on drop.
#[derive(Clone)]
pub struct Secret(String);
impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone, Debug)]
pub struct Credentials {
    pub access_token: Secret,
    pub webhook_secret: Secret,
    /// The bot's platform user ID, used to suppress its own inbound messages.
    pub bot_user_id: Option<String>,
}

/// An application can implement this trait using its existing connection vault.
#[async_trait]
pub trait CredentialResolver: Send + Sync + fmt::Debug {
    async fn resolve(&self) -> Result<Credentials, SyncError>;
}

#[derive(Clone, Debug)]
pub struct StaticCredentials(pub Credentials);
#[async_trait]
impl CredentialResolver for StaticCredentials {
    async fn resolve(&self) -> Result<Credentials, SyncError> {
        Ok(self.0.clone())
    }
}

/// A resolver that reads the environment again for every operation.
#[derive(Clone, Debug)]
pub struct EnvironmentCredentials {
    pub token_variable: String,
    pub signing_secret_variable: String,
    pub bot_id_variable: Option<String>,
}
#[async_trait]
impl CredentialResolver for EnvironmentCredentials {
    async fn resolve(&self) -> Result<Credentials, SyncError> {
        let read = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    SyncError::Credentials(format!(
                        "missing credential environment variable {name}"
                    ))
                })
        };
        Ok(Credentials {
            access_token: Secret::new(read(&self.token_variable)?),
            webhook_secret: Secret::new(read(&self.signing_secret_variable)?),
            bot_user_id: self
                .bot_id_variable
                .as_ref()
                .and_then(|key| std::env::var(key).ok()),
        })
    }
}

pub(crate) type Resolver = Arc<dyn CredentialResolver>;
