use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use engage_core::{BoardError, Principal};

/// Verification settings for an external OpenID Connect issuer.
#[derive(Clone, Debug)]
pub struct OidcConfig {
    pub issuer: String,
    pub audience: String,
    pub jwks_url: String,
    /// Exact subjects that may administer this board. JWT groups never grant ownership.
    pub owner_subjects: Vec<String>,
}

/// Credentials accepted by the service.
#[derive(Clone)]
pub enum AuthConfig {
    /// A local board. The listener must bind to a loopback interface.
    Local,
    /// A statically configured credential and its trusted identity.
    Bearer { token: String, principal: Principal },
    /// Signed tokens verified against an issuer's JSON Web Key Set.
    Oidc(OidcConfig),
    /// A direct tailnet listener. Identity comes from the provisioned local daemon.
    Tailscale {
        socket: PathBuf,
        owner_logins: Vec<String>,
    },
}

impl fmt::Debug for AuthConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => formatter.write_str("Local"),
            Self::Bearer { principal, .. } => formatter
                .debug_struct("Bearer")
                .field("token", &"[redacted]")
                .field("principal", principal)
                .finish(),
            Self::Oidc(config) => formatter.debug_tuple("Oidc").field(config).finish(),
            Self::Tailscale {
                socket,
                owner_logins,
            } => formatter
                .debug_struct("Tailscale")
                .field("socket", socket)
                .field("owner_logins", owner_logins)
                .finish(),
        }
    }
}

/// Where immutable uploaded bytes are stored.
#[derive(Clone, Debug)]
pub enum BlobConfig {
    Memory,
    Filesystem(PathBuf),
    /// S3 credentials use the standard environment/workload credential chain.
    S3 {
        bucket: String,
        region: String,
        endpoint: Option<String>,
    },
}

/// Listener and transport configuration. Secrets are omitted from `Debug`.
#[derive(Clone)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub public_url: Option<String>,
    pub auth: AuthConfig,
    pub blobs: BlobConfig,
    pub inspector_dir: Option<PathBuf>,
    pub max_body_bytes: usize,
    /// At least 32 bytes. Set the same value on every instance serving browser sessions.
    pub session_key: Option<String>,
    pub sync: Option<engage_sync::Registry>,
    pub sync_principal: Principal,
    /// A bounded per-peer request allowance. Applies to authenticated and public routes.
    pub requests_per_minute: u32,
    /// Concurrent SSE streams for one authenticated principal on this instance.
    pub max_streams_per_principal: usize,
    /// Allow explicitly configured watcher callbacks on private networks. Disabled by default.
    pub wake_allow_private: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 4100)),
            public_url: None,
            auth: AuthConfig::Local,
            blobs: BlobConfig::Filesystem(PathBuf::from(".engage/blobs")),
            inspector_dir: Some(PathBuf::from("crates/engage-inspector/dist")),
            max_body_bytes: 4 * 1024 * 1024,
            session_key: None,
            sync: None,
            sync_principal: Principal {
                owner: false,
                ..Principal::default()
            },
            requests_per_minute: 6000,
            max_streams_per_principal: 8,
            wake_allow_private: false,
        }
    }
}

impl fmt::Debug for ServerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerConfig")
            .field("bind", &self.bind)
            .field("public_url", &self.public_url)
            .field("auth", &self.auth)
            .field("blobs", &self.blobs)
            .field("inspector_dir", &self.inspector_dir)
            .field("max_body_bytes", &self.max_body_bytes)
            .field(
                "session_key",
                &self.session_key.as_ref().map(|_| "[redacted]"),
            )
            .field("sync_configured", &self.sync.is_some())
            .field("requests_per_minute", &self.requests_per_minute)
            .field("max_streams_per_principal", &self.max_streams_per_principal)
            .field("wake_allow_private", &self.wake_allow_private)
            .finish_non_exhaustive()
    }
}

impl ServerConfig {
    pub(crate) fn validate(&self) -> Result<(), BoardError> {
        if self.requests_per_minute == 0
            || self.max_streams_per_principal == 0
            || self.max_streams_per_principal > 128
        {
            return Err(crate::error(
                400,
                "invalid_config",
                "request and stream limits must be positive; at most 128 streams per principal",
            ));
        }
        if matches!(self.auth, AuthConfig::Local) && !self.bind.ip().is_loopback() {
            return Err(crate::error(
                400,
                "invalid_config",
                "local authentication requires a loopback listener",
            ));
        }
        if self.max_body_bytes == 0 || self.max_body_bytes > 100 * 1024 * 1024 {
            return Err(crate::error(
                400,
                "invalid_config",
                "max_body_bytes must be between 1 byte and 100 MiB",
            ));
        }
        if let Some(key) = &self.session_key
            && key.len() < 32
        {
            return Err(crate::error(
                400,
                "invalid_config",
                "session_key must contain at least 32 bytes",
            ));
        }
        if let AuthConfig::Bearer { token, .. } = &self.auth
            && token.len() < 24
        {
            return Err(crate::error(
                400,
                "invalid_config",
                "bearer credentials must contain at least 24 characters",
            ));
        }
        if let Some(address) = &self.public_url {
            let parsed = url::Url::parse(address).map_err(|_| {
                crate::error(
                    400,
                    "invalid_config",
                    "public_url must be an absolute HTTP URL",
                )
            })?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err(crate::error(
                    400,
                    "invalid_config",
                    "public_url must be an absolute HTTP URL without credentials, query, or fragment",
                ));
            }
        }
        Ok(())
    }
}
