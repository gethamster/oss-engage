//! Same-origin browser transport; credentials are never persisted by this client.

use std::cell::RefCell;

use gloo_net::http::{Request, RequestBuilder, Response};
use serde_json::Value;

thread_local! {
    // A display profile narrows identity. It is not a credential and is never persisted.
    static PROFILE: RefCell<String> = const { RefCell::new(String::new()) };
}

pub fn profile() -> String {
    PROFILE.with(|profile| profile.borrow().clone())
}

pub fn set_profile(value: String) {
    PROFILE.with(|profile| profile.replace(value));
}

fn as_profile(request: RequestBuilder) -> RequestBuilder {
    let profile = profile();
    if profile.is_empty() {
        request
    } else {
        request.header("x-board-profile", &profile)
    }
}

#[derive(Clone, Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

pub fn base() -> String {
    let document = leptos::prelude::document();
    if let Ok(Some(meta)) = document.query_selector("meta[name=engage-api]")
        && let Some(value) = meta.get_attribute("content")
    {
        return value.trim_end_matches('/').to_owned();
    }
    let path = leptos::prelude::window()
        .location()
        .pathname()
        .unwrap_or_default();
    path.split("/inspect")
        .next()
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_owned()
}

pub fn url(path: &str) -> String {
    format!("{}{path}", base())
}

pub fn is_proxy() -> bool {
    leptos::prelude::document()
        .query_selector("meta[name=engage-proxy]")
        .ok()
        .flatten()
        .and_then(|meta| meta.get_attribute("content"))
        .as_deref()
        == Some("true")
}

fn transport_error(error: impl std::fmt::Display) -> ApiError {
    ApiError {
        status: 0,
        message: format!("Cannot reach Engage. {error}"),
    }
}

async fn decode(response: Response) -> Result<Value, ApiError> {
    let status = response.status();
    let text = response.text().await.map_err(transport_error)?;
    let body = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str::<Value>(&text).map_err(|_| ApiError {
            status,
            message: "Engage returned an invalid response. Refresh and try again.".into(),
        })?
    };
    if !(200..300).contains(&status) {
        let message = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| body.get("message").and_then(Value::as_str))
            .unwrap_or(match status {
                401 => "Sign in to continue.",
                403 => "You do not have permission for this action.",
                404 => "This item is unavailable or you cannot read it.",
                409 => "The item changed. Refresh it and try again.",
                _ => "Engage could not complete the request. Try again.",
            });
        return Err(ApiError {
            status,
            message: message.to_owned(),
        });
    }
    Ok(body)
}

pub async fn get(path: &str) -> Result<Value, ApiError> {
    decode(
        as_profile(Request::get(&url(path)))
            .send()
            .await
            .map_err(transport_error)?,
    )
    .await
}

pub async fn write(method: &str, path: &str, body: &Value) -> Result<Value, ApiError> {
    let builder = match method {
        "PATCH" => Request::patch(&url(path)),
        "DELETE" => Request::delete(&url(path)),
        _ => Request::post(&url(path)),
    };
    decode(
        as_profile(builder)
            .json(body)
            .map_err(transport_error)?
            .send()
            .await
            .map_err(transport_error)?,
    )
    .await
}

pub async fn sign_in(token: &str) -> Result<(), ApiError> {
    let response = Request::post(&url("/auth/session"))
        .header("Authorization", &format!("Bearer {token}"))
        .send()
        .await
        .map_err(transport_error)?;
    decode(response).await.map(|_| ())
}

pub async fn upload(file: web_sys::File) -> Result<Value, ApiError> {
    let name = file.name();
    let mime = file.type_();
    let size = file.size();
    if size > 100.0 * 1024.0 * 1024.0 {
        return Err(ApiError {
            status: 400,
            message: "Attachments must be 100 MB or smaller.".into(),
        });
    }
    let bytes = gloo_file::futures::read_as_bytes(&gloo_file::File::from(file))
        .await
        .map_err(transport_error)?;
    if size > 4.0 * 1024.0 * 1024.0 {
        let ticket = write(
            "POST",
            "/attachments/uploads",
            &serde_json::json!({"name":name,"type":mime,"size":size as u64}),
        )
        .await?;
        let Some(target) = ticket.get("url").and_then(Value::as_str) else {
            return Err(ApiError {
                status: 502,
                message: "Engage did not return an upload URL. Try again.".into(),
            });
        };
        let origin = leptos::prelude::window()
            .location()
            .origin()
            .map_err(|_| ApiError {
                status: 0,
                message:
                    "Cannot determine this board's origin. Reopen the inspector and try again."
                        .into(),
            })?;
        let target = crate::content::upload_url(&origin, target).map_err(|message| ApiError {
            status: 502,
            message: message.into(),
        })?;
        let mut upload = Request::put(&target);
        if let Some(headers) = ticket.get("headers").and_then(Value::as_object) {
            for (name, value) in headers {
                if let Some(value) = value.as_str() {
                    upload = upload.header(name, value);
                }
            }
        }
        let response = upload
            .body(bytes)
            .map_err(transport_error)?
            .send()
            .await
            .map_err(transport_error)?;
        if !(200..300).contains(&response.status()) {
            return Err(ApiError {
                status: response.status(),
                message: "The file upload failed. Attach the file again to retry.".into(),
            });
        }
        let Some(id) = ticket.get("id").and_then(Value::as_str) else {
            return Err(ApiError {
                status: 502,
                message: "Engage did not return an upload ID. Try again.".into(),
            });
        };
        return write(
            "POST",
            &format!("/attachments/uploads/{}", urlencoding::encode(id)),
            &serde_json::json!({}),
        )
        .await;
    }
    decode(
        as_profile(Request::post(&url("/attachments")))
            .header(
                "content-type",
                if mime.is_empty() {
                    "application/octet-stream"
                } else {
                    &mime
                },
            )
            .header("x-board-filename", &urlencoding::encode(&name))
            .body(bytes)
            .map_err(transport_error)?
            .send()
            .await
            .map_err(transport_error)?,
    )
    .await
}

pub fn query(path: &str, params: &[(&str, String)]) -> String {
    let values: Vec<String> = params
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect();
    if values.is_empty() {
        path.into()
    } else {
        format!("{path}?{}", values.join("&"))
    }
}
