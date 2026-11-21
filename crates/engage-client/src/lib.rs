//! An asynchronous Rust client for Engage and Board-compatible HTTP services.
//!
//! ```no_run
//! # async fn example() -> Result<(), engage_client::ClientError> {
//! let board = engage_client::Client::new("http://127.0.0.1:4100")?;
//! let post = board.post("work", "Investigate the failing build").await?;
//! board.reply(post["message"]["id"].as_str().unwrap_or_default(), "Tests are running").await?;
//! # Ok(()) }
//! ```

mod channels;
mod coordination;
mod types;

use std::fmt;
use std::time::Duration;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Method;
pub use reqwest::Response;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use url::Url;

pub use types::{Event, EventsPage, Message, MessagePage, PostOptions, TaskClaim, Thread};

/// Browser metadata a trusted local proxy may forward to the board.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProxyHeaders<'a> {
    /// Body MIME type.
    pub content_type: Option<&'a str>,
    /// URI-encoded attachment filename.
    pub filename: Option<&'a str>,
    /// Response MIME types accepted by the browser.
    pub accept: Option<&'a str>,
}

/// An HTTP, protocol, URL, or response-decoding failure.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The provided URL or route is invalid.
    #[error("invalid board URL or route: {0}")]
    InvalidUrl(String),
    /// A credential or profile cannot be encoded as a header.
    #[error("invalid authentication or profile header")]
    InvalidHeader,
    /// The HTTP transport failed.
    #[error("board request failed: {0}")]
    Transport(#[source] reqwest::Error),
    /// The server returned a Board protocol error.
    #[error("{code}: {message}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Machine-readable Board error code.
        code: String,
        /// Human-readable error description.
        message: String,
        /// Optional corrective action supplied by the server.
        hint: Option<String>,
    },
    /// A successful response contained malformed JSON or an unexpected shape.
    #[error("invalid board response: {0}")]
    Decode(#[from] serde_json::Error),
}

impl From<reqwest::Error> for ClientError {
    fn from(error: reqwest::Error) -> Self {
        // Direct-upload URLs carry short-lived capabilities. Transport errors
        // must not copy those query strings into logs or user-visible diagnostics.
        Self::Transport(error.without_url())
    }
}

/// A reusable client. Clones share the underlying HTTP connection pool.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    headers: HeaderMap,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Connects to a Board protocol endpoint, preserving any mounted URL prefix.
    ///
    /// # Errors
    /// Returns an error for a non-HTTP URL, embedded credentials, or invalid URL.
    pub fn new(base: &str) -> Result<Self, ClientError> {
        let mut base = Url::parse(base).map_err(|e| ClientError::InvalidUrl(e.to_string()))?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(ClientError::InvalidUrl(
                "use an http(s) URL without credentials, query, or fragment".into(),
            ));
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(320))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            base,
            headers: HeaderMap::new(),
        })
    }

    /// Adds a bearer credential. Debug output never contains authentication headers.
    ///
    /// # Errors
    /// Returns an error when the token contains forbidden header characters.
    pub fn with_token(mut self, token: &str) -> Result<Self, ClientError> {
        let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| ClientError::InvalidHeader)?;
        header.set_sensitive(true);
        self.headers.insert(reqwest::header::AUTHORIZATION, header);
        Ok(self)
    }

    /// Writes through the caller's named agent profile, without widening its permissions.
    ///
    /// # Errors
    /// Returns an error when the profile contains forbidden header characters.
    pub fn with_profile(mut self, profile: &str) -> Result<Self, ClientError> {
        let value = HeaderValue::from_str(profile).map_err(|_| ClientError::InvalidHeader)?;
        self.headers.insert("x-board-profile", value);
        Ok(self)
    }

    /// The normalized endpoint, including its mount prefix.
    pub fn base_url(&self) -> &str {
        self.base.as_str()
    }

    /// Encodes an opaque identifier as a single URL path segment.
    pub fn segment(id: &str) -> String {
        utf8_percent_encode(id, NON_ALPHANUMERIC).to_string()
    }

    fn endpoint(&self, path: &str) -> Result<Url, ClientError> {
        if !path.starts_with('/')
            || path.contains("://")
            || path.contains('?')
            || path.contains('#')
            || path.split('/').any(|part| part == ".." || part == ".")
        {
            return Err(ClientError::InvalidUrl(
                "a route starts with / and contains no query or traversal".into(),
            ));
        }
        let endpoint = self
            .base
            .join(path.trim_start_matches('/'))
            .map_err(|e| ClientError::InvalidUrl(e.to_string()))?;
        if !endpoint.path().starts_with(self.base.path()) {
            return Err(ClientError::InvalidUrl(
                "a route cannot escape the board mount".into(),
            ));
        }
        Ok(endpoint)
    }

    /// Sends a protocol request and decodes its JSON response.
    ///
    /// Query pairs preserve repeated filters. Use `None` for requests with no JSON body.
    ///
    /// # Errors
    /// Returns transport, protocol, or response-decoding errors.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Value, ClientError> {
        self.request_as(method, path, query, body).await
    }

    /// Sends a protocol request and decodes a caller-selected response type.
    ///
    /// # Errors
    /// Returns transport, protocol, or response-decoding errors.
    pub async fn request_as<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<T, ClientError> {
        let response = self.send(method, path, query, body).await?;
        let bytes = response.bytes().await?;
        let value = if bytes.is_empty() {
            b"null".as_slice()
        } else {
            bytes.as_ref()
        };
        Ok(serde_json::from_slice(value)?)
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Response, ClientError> {
        let method = Method::from_bytes(method.as_bytes())
            .map_err(|e| ClientError::InvalidUrl(e.to_string()))?;
        let mut request = self
            .http
            .request(method, self.endpoint(path)?)
            .headers(self.headers.clone())
            .query(query);
        if let Some(body) = body {
            request = request.json(body);
        }
        check(request.send().await?).await
    }

    /// Reads endpoint discovery.
    pub async fn discovery(&self) -> Result<Value, ClientError> {
        self.request("GET", "/", &[], None).await
    }
    /// Reads the authenticated principal and effective permissions.
    pub async fn me(&self) -> Result<Value, ClientError> {
        self.request("GET", "/me", &[], None).await
    }
    /// Lists channels.
    pub async fn channels(&self, query: &[(String, String)]) -> Result<Value, ClientError> {
        self.request("GET", "/channels", query, None).await
    }
    /// Lists matching thread summaries.
    pub async fn threads(&self, query: &[(String, String)]) -> Result<Value, ClientError> {
        self.request("GET", "/threads", query, None).await
    }
    /// Reads a root and its replies, optionally paging replies.
    pub async fn thread(
        &self,
        id: &str,
        query: &[(String, String)],
    ) -> Result<Thread, ClientError> {
        self.request_as(
            "GET",
            &format!("/messages/{}", Self::segment(id)),
            query,
            None,
        )
        .await
    }
    /// Posts a root message.
    pub async fn post(&self, channel: &str, text: &str) -> Result<Value, ClientError> {
        self.post_with(&PostOptions {
            channel: Some(channel.into()),
            text: text.into(),
            ..PostOptions::default()
        })
        .await
    }
    /// Replies to an existing message's thread.
    pub async fn reply(&self, id: &str, text: &str) -> Result<Value, ClientError> {
        self.post_with(&PostOptions {
            re: Some(id.into()),
            text: text.into(),
            ..PostOptions::default()
        })
        .await
    }
    /// Posts or replies with conditional-write and idempotency options.
    pub async fn post_with(&self, options: &PostOptions) -> Result<Value, ClientError> {
        self.request(
            "POST",
            "/messages",
            &[],
            Some(&serde_json::to_value(options)?),
        )
        .await
    }
    /// Conditionally edits text at a known revision.
    pub async fn edit(
        &self,
        id: &str,
        text: &str,
        revision: Option<u64>,
    ) -> Result<Value, ClientError> {
        let mut body = json!({"text":text});
        if let Some(revision) = revision {
            body["revision"] = revision.into();
        }
        self.message_action(id, "edit", &body).await
    }
    /// Applies a mutation to a message.
    pub async fn message_action(
        &self,
        id: &str,
        action: &str,
        body: &Value,
    ) -> Result<Value, ClientError> {
        self.request(
            "POST",
            &format!("/messages/{}/{action}", Self::segment(id)),
            &[],
            Some(body),
        )
        .await
    }
    /// Lists tasks with filters and a paging cursor.
    pub async fn tasks(&self, query: &[(String, String)]) -> Result<Value, ClientError> {
        self.request("GET", "/tasks", query, None).await
    }
    /// Creates a task, or promotes an existing thread into a task.
    pub async fn create_task(&self, body: &Value) -> Result<Value, ClientError> {
        self.request("POST", "/tasks", &[], Some(body)).await
    }
    /// Applies a task lifecycle operation with its claim credentials where required.
    pub async fn task_action(
        &self,
        id: &str,
        action: &str,
        body: &Value,
    ) -> Result<Value, ClientError> {
        self.request(
            "POST",
            &format!("/tasks/{}/{action}", Self::segment(id)),
            &[],
            Some(body),
        )
        .await
    }
    /// Reads events after a cursor, optionally long-polling up to 300 seconds.
    pub async fn events(&self, query: &[(String, String)]) -> Result<EventsPage, ClientError> {
        self.request_as("GET", "/events", query, None).await
    }
    /// Opens a replayable SSE response. The caller owns cancellation and reconnection.
    pub async fn event_stream(&self, query: &[(String, String)]) -> Result<Response, ClientError> {
        check(
            self.http
                .get(self.endpoint("/events")?)
                .headers(self.headers.clone())
                .header("accept", "text/event-stream")
                .query(query)
                .send()
                .await?,
        )
        .await
    }
    /// Forwards a browser request through a trusted loopback inspector proxy.
    ///
    /// Only content negotiation and attachment metadata are forwarded; caller-supplied
    /// authentication, cookies, proxy headers, and host headers cannot override this client.
    /// The response preserves error statuses and streaming bodies.
    ///
    /// # Errors
    /// Returns a transport error or rejects a route outside this client's mounted endpoint.
    pub async fn proxy_request(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        headers: ProxyHeaders<'_>,
        body: Vec<u8>,
    ) -> Result<Response, ClientError> {
        let method = Method::from_bytes(method.as_bytes())
            .map_err(|e| ClientError::InvalidUrl(e.to_string()))?;
        let mut request = self
            .http
            .request(method, self.endpoint(path)?)
            .headers(self.headers.clone())
            .query(query);
        if let Some(value) = headers.content_type {
            request = request.header("content-type", value);
        }
        if let Some(value) = headers.filename {
            request = request.header("x-board-filename", value);
        }
        if let Some(value) = headers.accept {
            request = request.header("accept", value);
        }
        request.body(body).send().await.map_err(ClientError::from)
    }
    /// Uploads an attachment, returning its protocol metadata.
    pub async fn upload(
        &self,
        name: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> Result<Value, ClientError> {
        let response = check(
            self.http
                .post(self.endpoint("/attachments")?)
                .headers(self.headers.clone())
                .header("content-type", content_type)
                .header(
                    "x-board-filename",
                    utf8_percent_encode(name, NON_ALPHANUMERIC).to_string(),
                )
                .body(bytes)
                .send()
                .await?,
        )
        .await?;
        Ok(serde_json::from_slice(&response.bytes().await?)?)
    }
    /// Downloads a readable attachment. File access is checked by the service.
    pub async fn download(&self, id: &str) -> Result<Vec<u8>, ClientError> {
        let response = self
            .send(
                "GET",
                &format!("/attachments/{}", Self::segment(id)),
                &[],
                None,
            )
            .await?;
        Ok(response.bytes().await?.to_vec())
    }
    /// Sends any serializable protocol request body.
    pub async fn write<B: Serialize>(&self, path: &str, body: &B) -> Result<Value, ClientError> {
        self.request("POST", path, &[], Some(&serde_json::to_value(body)?))
            .await
    }
}

async fn check(response: Response) -> Result<Response, ClientError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let value: Value = response.json().await.unwrap_or(Value::Null);
    let error = value.get("error").unwrap_or(&value);
    Err(ClientError::Api {
        status,
        code: error["code"].as_str().unwrap_or("http_error").into(),
        message: error["message"]
            .as_str()
            .unwrap_or("the board refused the request")
            .into(),
        hint: error["hint"].as_str().map(str::to_owned),
    })
}
