//! Board-compatible HTTP transport for Engage.
//!
//! [`Service`] embeds HTTP and durable workers with a shared lifecycle. [`serve`]
//! runs the same service on a standalone listener until shutdown.

mod auth;
mod blobs;
mod claims;
mod config;
mod feed;
mod http;
mod inspector;
mod limits;
mod platform;
mod service;
mod sync;
mod wake;

pub use config::{AuthConfig, BlobConfig, OidcConfig, ServerConfig};
pub use service::{Service, WorkerHandle};

use std::future::Future;
use std::sync::Arc;

use axum::Router;
use engage_core::BoardError;
use engage_store::Board;

/// Constructs an embeddable HTTP router with authentication and browser protections.
/// This convenience function starts no background work. Use [`Service`] and
/// [`Service::start_workers`] when embedding schedules, sync, or watcher delivery.
///
/// # Errors
/// Returns an error for invalid configuration or unavailable blob storage.
pub async fn router(board: Board, config: ServerConfig) -> Result<Router, BoardError> {
    Ok(Service::new(board, config).await?.router())
}

async fn initialize(board: Board, config: ServerConfig) -> Result<Arc<http::App>, BoardError> {
    config.validate()?;
    let auth = auth::Authenticator::new(&config).await?;
    let blobs = blobs::Blobs::new(&config.blobs)
        .await?
        .with_key(config.session_key.as_deref());
    let limits = limits::Limits::new(config.requests_per_minute, config.max_streams_per_principal);
    let sync_ready = std::sync::atomic::AtomicBool::new(config.sync.is_none());
    Ok(Arc::new(http::App {
        board,
        config,
        auth,
        blobs,
        limits,
        sync_ready,
        wake: tokio::sync::Notify::new(),
        drain: tokio_util::sync::CancellationToken::new(),
    }))
}

/// Runs the HTTP listener and the durable scheduler until `shutdown` completes.
///
/// # Errors
/// Returns configuration, initialization, bind, or listener errors.
pub async fn serve(
    board: Board,
    config: ServerConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), BoardError> {
    let address = config.bind;
    let service = Service::new(board, config).await?;
    let app = service.router();
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|cause| {
            error(
                500,
                "listen_failed",
                &format!("cannot bind listener: {cause}"),
            )
        })?;
    let workers = service.start_workers();
    tracing::info!(%address, "engage listener ready");
    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown.await;
        service.drain_requests();
    })
    .await
    .map_err(|cause| error(500, "serve_failed", &format!("listener failed: {cause}")));
    workers.shutdown().await?;
    result
}

pub(crate) fn error(status: u16, code: &str, message: &str) -> BoardError {
    BoardError {
        status,
        code: code.to_owned(),
        message: message.to_owned(),
    }
}
