use crate::{CredentialResolver, SyncError, credentials::Resolver};
use hmac::{Hmac, Mac};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use sha2::Sha256;
use std::time::Duration;
use unicode_normalization::UnicodeNormalization;

pub(crate) fn client() -> Client {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .user_agent("oss-engage/0.1")
        .build()
        .expect("static HTTP client configuration is valid")
}

pub(crate) fn verify(secret: &str, body: &[u8], signature: &str) -> Result<(), SyncError> {
    if secret.is_empty() {
        return Err(SyncError::Signature);
    }
    let signature = hex::decode(signature).map_err(|_| SyncError::Signature)?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| SyncError::Signature)?;
    mac.update(body);
    mac.verify_slice(&signature)
        .map_err(|_| SyncError::Signature)
}

pub(crate) fn payload(bytes: &[u8]) -> Result<Value, SyncError> {
    serde_json::from_slice(bytes).map_err(|_| SyncError::Payload("expected JSON".into()))
}

pub(crate) fn required<'a>(value: &'a Value, field: &str) -> Result<&'a str, SyncError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| SyncError::Payload(format!("missing {field}")))
}
pub(crate) fn string(value: &Value, field: &str) -> String {
    match value.get(field) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

pub(crate) async fn json_response(response: reqwest::Response) -> Result<Value, SyncError> {
    let status = response.status().as_u16();
    let bytes = bounded_bytes(response, 8 * 1024 * 1024).await?;
    if !(200..300).contains(&status) {
        return Err(SyncError::Platform {
            status,
            code: format!("http_{status}"),
        });
    }
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    let result: Value = serde_json::from_slice(&bytes)
        .map_err(|_| SyncError::Payload("platform returned invalid JSON".into()))?;
    if result.get("ok") == Some(&Value::Bool(false)) {
        let code = result
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("request_rejected");
        // Only retain symbolic platform codes, never response diagnostics or content.
        let code =
            if code.len() <= 80 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                code.to_owned()
            } else {
                "request_rejected".into()
            };
        return Err(SyncError::Platform { status, code });
    }
    Ok(result)
}

/// Bound response allocation even when the peer omits Content-Length.
pub(crate) async fn bounded_bytes(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, SyncError> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(SyncError::Payload("platform response is too large".into()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(SyncError::Payload("platform response is too large".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[derive(Clone, Debug)]
pub(crate) struct Api {
    pub client: Client,
    pub credentials: Resolver,
    pub base: String,
}
impl Api {
    pub fn new(credentials: impl CredentialResolver + 'static, base: &str) -> Self {
        Self {
            client: client(),
            credentials: std::sync::Arc::new(credentials),
            base: base.into(),
        }
    }
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        query: &[(&str, &str)],
    ) -> Result<Value, SyncError> {
        let credentials = self.credentials.resolve().await?;
        let url = format!(
            "{}/{}",
            self.base.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let mut request = self
            .client
            .request(method, url)
            .bearer_auth(credentials.access_token.expose())
            .header("Accept", "application/json")
            .query(query);
        if let Some(body) = body {
            request = request.json(body);
        }
        json_response(request.send().await?).await
    }
}

pub(crate) fn validate_component(value: &str) -> Result<(), SyncError> {
    if value.is_empty()
        || value.len() > 300
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(SyncError::Payload("invalid platform identifier".into()));
    }
    Ok(())
}

pub(crate) fn slug(value: &str) -> String {
    let mut result = String::new();
    for c in value
        .nfkd()
        .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
        .flat_map(char::to_lowercase)
    {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
            result.push(c);
        } else if !result.ends_with('-') {
            result.push('-');
        }
    }
    result
        .trim_start_matches(['-', '_', '.'])
        .trim_end_matches('-')
        .to_owned()
}

pub(crate) fn tag(prefix: &str, name: &str) -> Option<(String, Value)> {
    let key = slug(name);
    if key.is_empty() {
        return None;
    }
    Some((
        format!("{prefix}.{key}").chars().take(64).collect(),
        json!(
            name.chars()
                .filter(|c| !c.is_control())
                .take(256)
                .collect::<String>()
        ),
    ))
}
