use crate::SyncError;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(super) fn keyed_id(key: &str, platform: &str, id: &str) -> String {
    if id.starts_with(&format!("{key}:")) {
        id.into()
    } else {
        format!(
            "{key}:{}",
            id.strip_prefix(&format!("{platform}:")).unwrap_or(id)
        )
    }
}
pub(super) fn required<'a>(value: &'a Value, name: &str) -> Result<&'a str, SyncError> {
    value[name]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SyncError::Payload(format!("missing {name}")))
}
pub(super) fn derived_id(key: &str, event: &str) -> String {
    format!(
        "sync-{key}-{}",
        hex::encode(Sha256::digest(event.as_bytes()))
    )
}
pub(super) fn operation_uuid(identity: &str) -> String {
    let mut bytes = Sha256::digest(identity.as_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(&bytes[..16]);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}
pub(super) fn segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}
pub(super) fn lifecycle_text(event: &Value) -> String {
    let text = [
        event["title"].as_str(),
        event["url"].as_str(),
        event["body"].as_str(),
    ]
    .into_iter()
    .flatten()
    .filter(|value| !value.trim().is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    clamp_text(&text)
}
pub(super) fn safe_error(error: &SyncError) -> String {
    match error {
        SyncError::Platform { code, .. } | SyncError::Board { code, .. } => code.clone(),
        SyncError::Unsupported(operation) => format!("unsupported {operation}"),
        _ => "sync operation failed".into(),
    }
}

pub(super) fn provider_text(key: &str, platform: &str, principal: &str, text: &str) -> String {
    text.replace(&format!("@[{principal}+{key}:"), &format!("@[{platform}:"))
        .replace(&format!("@[{key}:"), &format!("@[{platform}:"))
}

pub(super) fn clamp_text(text: &str) -> String {
    const MAX_BYTES: usize = 65_536;
    if text.len() <= MAX_BYTES {
        return text.to_owned();
    }
    let mut cut = MAX_BYTES - 3;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
}

/// Finishes each independent leased delivery before reporting the first failure.
#[derive(Default)]
pub(super) struct BatchResult {
    count: usize,
    error: Option<SyncError>,
}
impl BatchResult {
    pub(super) fn record(&mut self, result: Result<usize, SyncError>) {
        match result {
            Ok(count) => self.count += count,
            Err(error) => {
                self.error.get_or_insert(error);
            }
        }
    }
    pub(super) fn finish(self) -> Result<usize, SyncError> {
        self.error.map_or(Ok(self.count), Err)
    }
}
