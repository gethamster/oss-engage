use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use engage_core::BoardError;
use engage_store::Board;
use tokio_util::sync::CancellationToken;

use crate::{ServerConfig, error, http::App};

/// An embeddable service sharing its verifier, blob store, limits, and workers.
#[derive(Clone)]
pub struct Service {
    app: Arc<App>,
}

impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Service").finish_non_exhaustive()
    }
}

impl Service {
    /// Initializes one shared service state for both HTTP and background work.
    ///
    /// # Errors
    /// Returns configuration, authentication initialization, or storage errors.
    pub async fn new(board: Board, config: ServerConfig) -> Result<Self, BoardError> {
        Ok(Self {
            app: crate::initialize(board, config).await?,
        })
    }

    /// Builds a router sharing this service's authentication and attachment state.
    ///
    /// Serve with Axum's `into_make_service_with_connect_info::<SocketAddr>()` so
    /// loopback checks and Tailscale verification can use the actual network peer.
    pub fn router(&self) -> Router {
        crate::http::router(self.app.clone())
    }

    /// Ends active event streams and requests, and rejects subsequent HTTP work.
    ///
    /// This operation is terminal for this service and its cloned routers. An
    /// embedding host should call it when stopping its listener so long polls or
    /// incomplete request bodies cannot delay graceful shutdown. Stop background
    /// workers separately with [`WorkerHandle::shutdown`].
    pub fn drain_requests(&self) {
        self.app.drain.cancel();
    }

    /// Starts durable schedules, provider sync, and watcher notifications.
    ///
    /// Keep the returned handle alive for the embedding application's lifetime.
    /// Leases and idempotency make separate instances safe against one database.
    pub fn start_workers(&self) -> WorkerHandle {
        let cancel = CancellationToken::new();
        let mut tasks = vec![
            scheduler(self.app.board.clone(), cancel.clone()),
            crate::wake::spawn(self.app.clone(), cancel.clone()),
        ];
        if let Some(sync) = crate::sync::spawn(self.app.clone(), cancel.clone()) {
            tasks.push(sync);
        }
        WorkerHandle { cancel, tasks }
    }
}

/// Owns background task lifetimes. Dropping it requests cooperative cancellation.
pub struct WorkerHandle {
    cancel: CancellationToken,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl fmt::Debug for WorkerHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkerHandle")
            .field("tasks", &self.tasks.len())
            .finish_non_exhaustive()
    }
}

impl WorkerHandle {
    /// Stops and joins all owned workers without discarding durable pending work.
    ///
    /// # Errors
    /// Returns `worker_failed` if an owned task panicked or was externally aborted.
    pub async fn shutdown(mut self) -> Result<(), BoardError> {
        self.cancel.cancel();
        let mut failed = false;
        for task in self.tasks.drain(..) {
            if task.await.is_err() {
                failed = true;
            }
        }
        if failed {
            Err(error(
                500,
                "worker_failed",
                "a background worker exited unexpectedly",
            ))
        } else {
            Ok(())
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn scheduler(board: Board, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {()=cancel.cancelled()=>break,_=interval.tick()=>{}}
            tokio::select! {
                ()=cancel.cancelled()=>break,
                result=board.tick()=>{
                    if let Err(cause)=result {tracing::error!(code=cause.code,"durable scheduler tick failed");}
                }
            }
        }
    })
}
