use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use engage_core::{BoardError, Principal};
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, decode_header, encode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;

use crate::{AuthConfig, OidcConfig, ServerConfig, error};

const COOKIE: &str = "engage_session";
const SESSION_AUDIENCE: &str = "engage-inspector";
const SESSION_ISSUER: &str = "engage-session";

#[derive(Clone)]
pub(crate) struct Authenticator {
    config: AuthConfig,
    keys: Arc<RwLock<Option<CachedKeys>>>,
    client: reqwest::Client,
    session_key: Option<String>,
    tailscale_client: Option<reqwest::Client>,
    allow_local_wake: bool,
}

struct CachedKeys {
    value: JwkSet,
    fetched: Instant,
}

#[derive(Serialize, Deserialize)]
struct BrowserSession {
    sub: String,
    iss: String,
    aud: String,
    exp: u64,
    principal: Principal,
}

pub(crate) struct Identity {
    pub principal: Principal,
    pub expires_at: Option<u64>,
}

impl Authenticator {
    pub async fn new(config: &ServerConfig) -> Result<Self, BoardError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| error(500, "auth_failed", "cannot initialize identity verifier"))?;
        let session_key = config.session_key.clone().or_else(|| {
            matches!(config.auth, AuthConfig::Local)
                .then(|| format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4()))
        });
        let tailscale_client = if let AuthConfig::Tailscale { socket, .. } = &config.auth {
            #[cfg(unix)]
            {
                Some(
                    reqwest::Client::builder()
                        .unix_socket(socket.as_path())
                        .redirect(reqwest::redirect::Policy::none())
                        .timeout(Duration::from_secs(5))
                        .build()
                        .map_err(|_| {
                            error(500, "auth_failed", "cannot initialize tailscaled client")
                        })?,
                )
            }
            #[cfg(not(unix))]
            {
                let _ = socket;
                return Err(error(
                    400,
                    "invalid_config",
                    "Tailscale daemon socket authentication requires a Unix platform",
                ));
            }
        } else {
            None
        };
        let auth = Self {
            config: config.auth.clone(),
            keys: Arc::default(),
            client,
            session_key,
            tailscale_client,
            allow_local_wake: config.wake_allow_private,
        };
        if let AuthConfig::Oidc(oidc) = &auth.config {
            validate_endpoint(&oidc.issuer)?;
            validate_endpoint(&oidc.jwks_url)?;
            if oidc.audience.is_empty() {
                return Err(error(
                    400,
                    "invalid_config",
                    "OIDC audience must not be empty",
                ));
            }
            auth.jwks(oidc, false).await?;
        }
        Ok(auth)
    }

    pub async fn identify(
        &self,
        headers: &HeaderMap,
        peer: Option<std::net::SocketAddr>,
    ) -> Result<Identity, BoardError> {
        if headers.get_all("authorization").iter().count() > 1 {
            return Err(unauthorized());
        }
        let mut identity = if let Some(header) = headers.get("authorization") {
            let value = header.to_str().map_err(|_| unauthorized())?;
            let token = value
                .strip_prefix("Bearer ")
                .filter(|s| s.len() <= 16 * 1024)
                .ok_or_else(unauthorized)?;
            self.bearer(token).await?
        } else if let Some(cookie) = cookie(headers) {
            self.browser(cookie)?
        } else if matches!(self.config, AuthConfig::Local) {
            Identity {
                principal: Principal::default(),
                expires_at: None,
            }
        } else if let AuthConfig::Tailscale { owner_logins, .. } = &self.config {
            self.tailscale(peer, owner_logins).await?
        } else {
            return Err(unauthorized());
        };
        identity.principal.facts["allow_local_wake"] = self.allow_local_wake.into();
        if headers.contains_key("x-board-scope") {
            return Err(error(
                400,
                "invalid_scope",
                "x-board-scope is x-board-can now: send the same value as x-board-can",
            ));
        }
        if let Some(profile) = headers.get("x-board-profile") {
            let value = profile
                .to_str()
                .map_err(|_| error(400, "invalid_profile", "invalid profile header"))?;
            if let Some(profile) = crate::claims::profile(value)? {
                identity.principal.profile = Some(profile.name.to_ascii_lowercase());
                identity.principal.facts["_profile"] =
                    json!({"name":profile.name,"avatar":profile.avatar});
            }
        }
        if let Some(scope) = headers.get("x-board-can") {
            let scope = scope
                .to_str()
                .map_err(|_| error(400, "invalid_scope", "invalid scope header"))?;
            crate::claims::scope(scope)?;
            // The configured token's scope is retained. A request can only narrow it.
            if identity.principal.scope.is_some() {
                return Err(error(
                    400,
                    "invalid_scope",
                    "a scoped credential cannot replace its scope header",
                ));
            }
            identity.principal.scope = Some(scope.to_owned());
        }
        Ok(identity)
    }

    async fn bearer(&self, token: &str) -> Result<Identity, BoardError> {
        match &self.config {
            AuthConfig::Local => Err(unauthorized()),
            AuthConfig::Bearer {
                token: expected,
                principal,
            } => {
                if token.len() == expected.len()
                    && bool::from(token.as_bytes().ct_eq(expected.as_bytes()))
                {
                    Ok(Identity {
                        principal: principal.clone(),
                        expires_at: None,
                    })
                } else {
                    Err(unauthorized())
                }
            }
            AuthConfig::Oidc(config) => self.oidc(config, token).await,
            AuthConfig::Tailscale { .. } => Err(unauthorized()),
        }
    }

    async fn tailscale(
        &self,
        peer: Option<std::net::SocketAddr>,
        owners: &[String],
    ) -> Result<Identity, BoardError> {
        let peer = peer.ok_or_else(unauthorized)?;
        let client = self.tailscale_client.as_ref().ok_or_else(unauthorized)?;
        let response = client
            .get("http://local-tailscaled.sock/localapi/v0/whois")
            .query(&[("addr", peer.to_string())])
            .send()
            .await
            .map_err(|_| {
                error(
                    503,
                    "identity_unavailable",
                    "tailscaled identity service is unavailable",
                )
            })?;
        if !response.status().is_success() {
            return Err(unauthorized());
        }
        let bytes = limited_response(response).await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| unauthorized())?;
        let user = &value["UserProfile"];
        let login = user["LoginName"]
            .as_str()
            .filter(|login| !login.is_empty())
            .ok_or_else(unauthorized)?;
        let user_id = user["ID"]
            .as_u64()
            .map(|n| n.to_string())
            .or_else(|| user["ID"].as_str().map(str::to_owned))
            .ok_or_else(unauthorized)?;
        let groups = value["Node"]["Tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        Ok(Identity {
            principal: Principal {
                id: format!("tailscale:user:{user_id}"),
                handle: login.into(),
                name: user["DisplayName"].as_str().unwrap_or(login).into(),
                groups,
                owner: owners.iter().any(|owner| owner == login),
                facts: json!({"issuer":"tailscale","login":login,"node":value["Node"]["ID"]}),
                ..Principal::default()
            },
            expires_at: Some((chrono::Utc::now().timestamp() + 60).max(0) as u64),
        })
    }

    async fn oidc(&self, config: &OidcConfig, token: &str) -> Result<Identity, BoardError> {
        let header = decode_header(token).map_err(|_| unauthorized())?;
        if !matches!(
            header.alg,
            Algorithm::RS256
                | Algorithm::RS384
                | Algorithm::RS512
                | Algorithm::ES256
                | Algorithm::ES384
                | Algorithm::EdDSA
        ) {
            return Err(unauthorized());
        }
        let mut keys = self.jwks(config, false).await?;
        let select = |keys: &JwkSet| match &header.kid {
            Some(kid) => keys.find(kid).cloned(),
            None if keys.keys.len() == 1 => keys.keys.first().cloned(),
            None => None,
        };
        if select(&keys).is_none() {
            keys = self.jwks(config, true).await?;
        }
        let key = select(&keys).ok_or_else(unauthorized)?;
        if key
            .common
            .public_key_use
            .as_ref()
            .is_some_and(|usage| *usage != jsonwebtoken::jwk::PublicKeyUse::Signature)
            || key
                .common
                .key_operations
                .as_ref()
                .is_some_and(|operations| {
                    !operations.contains(&jsonwebtoken::jwk::KeyOperations::Verify)
                })
        {
            return Err(unauthorized());
        }
        let key = DecodingKey::from_jwk(&key).map_err(|_| unauthorized())?;
        let mut validation = Validation::new(header.alg);
        validation.set_audience(&[&config.audience]);
        validation.set_issuer(&[&config.issuer]);
        validation.set_required_spec_claims(&["exp", "sub", "iss", "aud"]);
        validation.validate_nbf = true;
        validation.leeway = 5;
        let claims = decode::<Value>(token, &key, &validation)
            .map_err(|_| unauthorized())?
            .claims;
        let sub = claims
            .get("sub")
            .and_then(Value::as_str)
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 512
                    && !s
                        .chars()
                        .any(|c| c.is_whitespace() || matches!(c, '+' | ']'))
            })
            .ok_or_else(unauthorized)?;
        let handle = claims
            .get("preferred_username")
            .or_else(|| claims.get("email"))
            .and_then(Value::as_str)
            .unwrap_or(sub)
            .to_owned();
        let name = claims
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&handle)
            .to_owned();
        let groups = claims
            .get("groups")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let expires_at = claims.get("exp").and_then(Value::as_u64);
        let issuer_kind = if config.issuer == "https://token.actions.githubusercontent.com" {
            "github"
        } else if url::Url::parse(&config.issuer).ok().is_some_and(|issuer| {
            issuer.scheme() == "https" && issuer.host_str() == Some("oidc.vercel.com")
        }) {
            "vercel"
        } else {
            "oidc"
        };
        let principal_id = match issuer_kind {
            "vercel" => {
                let project = identity_claim(&claims, "project_id")?;
                let environment = identity_claim(&claims, "environment")?;
                format!("vercel:project:{project}:{environment}")
            }
            "github" => format!("github:repo:{}", identity_claim(&claims, "repository_id")?),
            _ => format!("oidc:{}:{sub}", config.issuer),
        };
        let principal = Principal {
            id: principal_id,
            handle,
            name,
            groups,
            owner: config.owner_subjects.iter().any(|owner| owner == sub),
            facts: json!({"issuer":issuer_kind,"iss":config.issuer,"claims":claims}),
            ..Principal::default()
        };
        Ok(Identity {
            principal,
            expires_at,
        })
    }

    async fn jwks(&self, config: &OidcConfig, refresh: bool) -> Result<JwkSet, BoardError> {
        {
            let cache = self.keys.read().await;
            if let Some(keys) = cache.as_ref()
                && keys.fetched.elapsed()
                    < if refresh {
                        Duration::from_secs(5)
                    } else {
                        Duration::from_secs(300)
                    }
            {
                return Ok(keys.value.clone());
            }
        }
        let mut cache = self.keys.write().await;
        if let Some(keys) = cache.as_ref()
            && keys.fetched.elapsed()
                < if refresh {
                    Duration::from_secs(5)
                } else {
                    Duration::from_secs(300)
                }
        {
            return Ok(keys.value.clone());
        }
        let response = self
            .client
            .get(&config.jwks_url)
            .send()
            .await
            .map_err(|_| {
                error(
                    503,
                    "identity_unavailable",
                    "identity key service is unavailable",
                )
            })?;
        if !response.status().is_success()
            || response.content_length().is_some_and(|n| n > 1024 * 1024)
        {
            return Err(error(
                503,
                "identity_unavailable",
                "identity key service returned an invalid response",
            ));
        }
        let bytes = limited_response(response).await?;
        let value: JwkSet = serde_json::from_slice(&bytes).map_err(|_| {
            error(
                503,
                "identity_unavailable",
                "identity key service returned invalid keys",
            )
        })?;
        if value.keys.is_empty() || value.keys.len() > 100 {
            return Err(error(
                503,
                "identity_unavailable",
                "identity key service returned invalid keys",
            ));
        }
        *cache = Some(CachedKeys {
            value: value.clone(),
            fetched: Instant::now(),
        });
        Ok(value)
    }

    pub fn session_cookie(&self, identity: Identity, secure: bool) -> Result<String, BoardError> {
        let key = self.session_key.as_ref().ok_or_else(|| {
            error(
                503,
                "sessions_unavailable",
                "configure a shared session_key to enable browser sessions",
            )
        })?;
        let now = chrono::Utc::now().timestamp().max(0) as u64;
        let expires = identity.expires_at.unwrap_or(now + 3600).min(now + 3600);
        let claims = BrowserSession {
            sub: identity.principal.id.clone(),
            iss: SESSION_ISSUER.into(),
            aud: SESSION_AUDIENCE.into(),
            exp: expires,
            principal: identity.principal,
        };
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(key.as_bytes()),
        )
        .map_err(|_| error(500, "auth_failed", "cannot create browser session"))?;
        if token.len() > 3800 {
            return Err(error(
                400,
                "session_too_large",
                "identity claims are too large for a browser session",
            ));
        }
        Ok(format!(
            "{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}{}",
            expires.saturating_sub(now),
            if secure { "; Secure" } else { "" }
        ))
    }

    fn browser(&self, cookie: &str) -> Result<Identity, BoardError> {
        let key = self.session_key.as_ref().ok_or_else(unauthorized)?;
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_audience(&[SESSION_AUDIENCE]);
        validation.set_issuer(&[SESSION_ISSUER]);
        validation.leeway = 0;
        let session = decode::<BrowserSession>(
            cookie,
            &DecodingKey::from_secret(key.as_bytes()),
            &validation,
        )
        .map_err(|_| unauthorized())?
        .claims;
        Ok(Identity {
            principal: session.principal,
            expires_at: Some(session.exp),
        })
    }
}

pub(crate) fn clear_cookie() -> &'static str {
    "engage_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"
}

fn cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("cookie")?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|piece| piece.trim().split_once('='))
        .find_map(|(name, value)| (name == COOKIE).then_some(value))
}

fn unauthorized() -> BoardError {
    error(401, "unauthorized", "a valid board credential is required")
}

fn identity_claim<'a>(claims: &'a Value, name: &str) -> Result<&'a str, BoardError> {
    claims
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 200
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
        })
        .ok_or_else(unauthorized)
}

fn validate_endpoint(value: &str) -> Result<(), BoardError> {
    let parsed = url::Url::parse(value).map_err(|_| {
        error(
            400,
            "invalid_config",
            "OIDC endpoints must be absolute HTTPS URLs",
        )
    })?;
    let loopback = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && loopback)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(error(
            400,
            "invalid_config",
            "OIDC endpoints require HTTPS except on loopback",
        ));
    }
    Ok(())
}

async fn limited_response(mut response: reqwest::Response) -> Result<Vec<u8>, BoardError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| error(503, "identity_unavailable", "cannot read identity response"))?
    {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err(error(
                503,
                "identity_unavailable",
                "identity response is too large",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
