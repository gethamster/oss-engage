use std::sync::Arc;

use bytes::Bytes;
use engage_core::{BoardError, Principal};
use engage_store::Board;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use object_store::{ObjectStore, ObjectStoreExt, PutMode, path::Path};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{BlobConfig, error};

pub(crate) const UPLOAD_MAX: usize = 4 * 1024 * 1024;
pub(crate) const DIRECT_MAX: usize = 100 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Blobs {
    store: Arc<dyn ObjectStore>,
    key: Arc<str>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Ticket {
    id: String,
    name: String,
    #[serde(rename = "type")]
    media_type: String,
    size: usize,
    uploaded_by: String,
    exp: u64,
    sha256: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct Permit {
    sub: String,
    iss: String,
    aud: String,
    exp: u64,
}

impl Blobs {
    pub async fn new(config: &BlobConfig) -> Result<Self, BoardError> {
        let store: Arc<dyn ObjectStore> = match config {
            BlobConfig::Memory => Arc::new(object_store::memory::InMemory::new()),
            BlobConfig::Filesystem(directory) => {
                tokio::fs::create_dir_all(directory)
                    .await
                    .map_err(storage_error)?;
                Arc::new(
                    object_store::local::LocalFileSystem::new_with_prefix(directory)
                        .map_err(storage_error)?,
                )
            }
            BlobConfig::S3 {
                bucket,
                region,
                endpoint,
            } => {
                let mut builder = object_store::aws::AmazonS3Builder::from_env()
                    .with_bucket_name(bucket)
                    .with_region(region);
                if let Some(endpoint) = endpoint {
                    builder = builder
                        .with_endpoint(endpoint)
                        .with_virtual_hosted_style_request(false);
                    if endpoint.starts_with("http://") {
                        builder = builder.with_allow_http(true);
                    }
                }
                Arc::new(builder.build().map_err(storage_error)?)
            }
        };
        Ok(Self {
            store,
            key: Arc::from(format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4())),
        })
    }

    pub fn with_key(mut self, key: Option<&str>) -> Self {
        if let Some(key) = key {
            self.key = Arc::from(key);
        }
        self
    }

    pub async fn upload(
        &self,
        board: &Board,
        principal: &Principal,
        bytes: Bytes,
        name: Option<&str>,
        media_type: Option<&str>,
    ) -> Result<Value, BoardError> {
        if bytes.is_empty() {
            return Err(error(400, "bad_request", "the file is empty"));
        }
        if bytes.len() > UPLOAD_MAX {
            return Err(error(413, "too_large", "attachments are at most 4 MB"));
        }
        let id = format!("att_{}", uuid::Uuid::new_v4());
        let name = filename(name)?;
        let media_type = content_type(media_type);
        self.create_attachment(board, principal, &id, bytes, &name, &media_type)
            .await
    }

    async fn create_attachment(
        &self,
        board: &Board,
        principal: &Principal,
        id: &str,
        bytes: Bytes,
        name: &str,
        media_type: &str,
    ) -> Result<Value, BoardError> {
        let size = bytes.len();
        let path = data_path(id)?;
        self.store
            .put_opts(&path, bytes.into(), PutMode::Create.into())
            .await
            .map_err(storage_error)?;
        let metadata =
            json!({"id":id,"name":name,"type":media_type,"size":size,"object":path.as_ref()});
        match board
            .request(principal, "POST", "/attachments", &[], metadata)
            .await
        {
            Ok(value) => Ok(public_metadata(&value)),
            Err(cause) => {
                if let Err(cleanup) = self.store.delete(&path).await {
                    tracing::warn!(%cleanup, "cannot remove unregistered upload");
                }
                Err(cause)
            }
        }
    }

    pub async fn download(
        &self,
        board: &Board,
        principal: &Principal,
        id: &str,
    ) -> Result<(Value, Bytes), BoardError> {
        let metadata = board
            .request(
                principal,
                "GET",
                &format!("/attachments/{id}"),
                &[],
                Value::Null,
            )
            .await?;
        let path = data_path(id)?;
        let bytes = self
            .store
            .get(&path)
            .await
            .map_err(read_error)?
            .bytes()
            .await
            .map_err(storage_error)?;
        Ok((public_metadata(&metadata), bytes))
    }

    pub async fn start(
        &self,
        principal: &Principal,
        body: &Value,
        base_url: &str,
    ) -> Result<Value, BoardError> {
        let size = body
            .get("size")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0 && *n <= DIRECT_MAX as u64)
            .ok_or_else(|| {
                error(
                    400,
                    "bad_request",
                    "direct uploads need a size between 1 byte and 100 MB",
                )
            })? as usize;
        let sha256 = body
            .get("sha256")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(hash) = &sha256 {
            use base64::Engine;
            if base64::engine::general_purpose::STANDARD
                .decode(hash)
                .ok()
                .is_none_or(|bytes| bytes.len() != 32)
            {
                return Err(error(
                    400,
                    "bad_request",
                    "sha256 must be a base64 SHA-256 digest",
                ));
            }
        }
        let ticket = Ticket {
            id: format!("att_{}", uuid::Uuid::new_v4()),
            name: filename(body.get("name").and_then(Value::as_str))?,
            media_type: content_type(body.get("type").and_then(Value::as_str)),
            size,
            uploaded_by: principal.id.clone(),
            exp: (chrono::Utc::now().timestamp() + 1800).max(0) as u64,
            sha256,
        };
        let bytes = serde_json::to_vec(&ticket).map_err(storage_error)?;
        self.store
            .put_opts(
                &ticket_path(&ticket.id)?,
                bytes.into(),
                PutMode::Create.into(),
            )
            .await
            .map_err(storage_error)?;
        let permit = Permit {
            sub: ticket.id.clone(),
            iss: "engage-upload".into(),
            aud: "engage-upload".into(),
            exp: ticket.exp,
        };
        let token = encode(
            &Header::new(Algorithm::HS256),
            &permit,
            &EncodingKey::from_secret(self.key.as_bytes()),
        )
        .map_err(storage_error)?;
        Ok(json!({
            "id":ticket.id,
            "url":format!("{}/attachments/uploads/{}/data?ticket={token}", base_url.trim_end_matches('/'), ticket.id),
            "headers":{"content-type":ticket.media_type},
            "expiresAt":chrono::DateTime::from_timestamp(ticket.exp as i64,0).map(|time|time.to_rfc3339_opts(chrono::SecondsFormat::Millis,true))
        }))
    }

    pub async fn put_ticket(
        &self,
        id: &str,
        permit: &str,
        media_type: Option<&str>,
        bytes: Bytes,
    ) -> Result<(), BoardError> {
        self.authorize_ticket(id, permit)?;
        let ticket = self.ticket(id).await?;
        if ticket.size != bytes.len() || content_type(media_type) != ticket.media_type {
            return Err(error(
                400,
                "upload_mismatch",
                "uploaded bytes must match the ticket's size and content type",
            ));
        }
        if let Some(expected) = ticket.sha256 {
            use base64::Engine;
            let actual = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&bytes));
            if actual != expected {
                return Err(error(
                    400,
                    "upload_mismatch",
                    "uploaded bytes do not match sha256",
                ));
            }
        }
        self.store
            .put_opts(&data_path(id)?, bytes.into(), PutMode::Create.into())
            .await
            .map_err(|cause| match cause {
                object_store::Error::AlreadyExists { .. } => {
                    error(409, "upload_exists", "upload bytes have already landed")
                }
                _ => storage_error(cause),
            })?;
        Ok(())
    }

    fn authorize_ticket(&self, id: &str, permit: &str) -> Result<(), BoardError> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_audience(&["engage-upload"]);
        validation.set_issuer(&["engage-upload"]);
        validation.leeway = 0;
        let claim = decode::<Permit>(
            permit,
            &DecodingKey::from_secret(self.key.as_bytes()),
            &validation,
        )
        .map_err(|_| error(403, "invalid_upload", "upload ticket is invalid or expired"))?
        .claims;
        if claim.sub != id {
            return Err(error(
                403,
                "invalid_upload",
                "upload ticket does not name this upload",
            ));
        }
        Ok(())
    }

    pub async fn preflight_ticket(
        &self,
        id: &str,
        permit: &str,
        media_type: Option<&str>,
    ) -> Result<usize, BoardError> {
        self.authorize_ticket(id, permit)?;
        let ticket = self.ticket(id).await?;
        if content_type(media_type) != ticket.media_type {
            return Err(error(
                400,
                "upload_mismatch",
                "content type must match the upload ticket",
            ));
        }
        Ok(ticket.size)
    }

    pub async fn finish(
        &self,
        board: &Board,
        principal: &Principal,
        id: &str,
    ) -> Result<Value, BoardError> {
        let ticket = self.ticket(id).await?;
        if ticket.uploaded_by != principal.id {
            return Err(error(404, "attachment_not_found", "no upload"));
        }
        if ticket.exp <= chrono::Utc::now().timestamp().max(0) as u64 {
            return Err(error(400, "upload_expired", "upload ticket has expired"));
        }
        let path = data_path(id)?;
        let metadata = self.store.head(&path).await.map_err(read_error)?;
        if metadata.size as usize != ticket.size {
            return Err(error(
                400,
                "upload_mismatch",
                "uploaded size does not match the ticket",
            ));
        }
        let result = board.request(principal, "POST", "/attachments", &[], json!({
            "id":id,"name":ticket.name,"type":ticket.media_type,"size":ticket.size,"object":path.as_ref()
        })).await?;
        Ok(public_metadata(&result))
    }

    async fn ticket(&self, id: &str) -> Result<Ticket, BoardError> {
        let bytes = self
            .store
            .get(&ticket_path(id)?)
            .await
            .map_err(read_error)?
            .bytes()
            .await
            .map_err(storage_error)?;
        serde_json::from_slice(&bytes).map_err(storage_error)
    }
}

fn data_path(id: &str) -> Result<Path, BoardError> {
    checked_path("attachments", id)
}
fn ticket_path(id: &str) -> Result<Path, BoardError> {
    checked_path("uploads", id)
}

fn checked_path(prefix: &str, id: &str) -> Result<Path, BoardError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(error(404, "attachment_not_found", "no attachment"));
    }
    Ok(Path::from(format!("board/{prefix}/{id}")))
}

fn public_metadata(value: &Value) -> Value {
    let value = value.get("attachment").unwrap_or(value);
    json!({"id":value.get("id"),"name":value.get("name"),"type":value.get("type"),"size":value.get("size")})
}

fn filename(value: Option<&str>) -> Result<String, BoardError> {
    let name = value
        .unwrap_or("file")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("file")
        .chars()
        .filter(|c| !c.is_control())
        .take(255)
        .collect::<String>();
    let name = name.trim();
    let name = if name.is_empty() { "file" } else { name };
    if name.starts_with('.') || name.chars().any(|c| matches!(c, '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}')) {
        return Err(error(400, "invalid_attachment", "attachment names cannot start with a dot or contain invisible formatting"));
    }
    Ok(name.to_owned())
}

fn content_type(value: Option<&str>) -> String {
    let value = value
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if value.len() <= 255
        && value
            .split_once('/')
            .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"/!#$&^_.+-".contains(&c))
    {
        value
    } else {
        "application/octet-stream".into()
    }
}

fn storage_error(cause: impl std::fmt::Display) -> BoardError {
    tracing::error!(%cause, "blob operation failed");
    error(
        503,
        "storage_unavailable",
        "attachment storage is unavailable",
    )
}

fn read_error(cause: object_store::Error) -> BoardError {
    match cause {
        object_store::Error::NotFound { .. } => error(404, "attachment_not_found", "no attachment"),
        _ => storage_error(cause),
    }
}
