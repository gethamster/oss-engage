use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use engage_core::{BoardError, Principal};
use engage_sync::{BoardApi, SyncError, SyncWorker};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::http::App;

struct LocalApi(Arc<App>);

impl LocalApi {
    fn principal(&self, id: &str) -> Principal {
        Principal {
            id: id.into(),
            handle: id.into(),
            name: id.into(),
            ..self.0.config.sync_principal.clone()
        }
    }
}

#[async_trait]
impl BoardApi for LocalApi {
    async fn request(
        &self,
        principal: &str,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Value,
    ) -> Result<Value, SyncError> {
        self.0
            .board
            .request(&self.principal(principal), method, path, query, body)
            .await
            .map_err(board_error)
    }

    async fn upload(
        &self,
        principal: &str,
        name: &str,
        mime_type: &str,
        bytes: Vec<u8>,
    ) -> Result<Value, SyncError> {
        self.0
            .blobs
            .upload(
                &self.0.board,
                &self.principal(principal),
                bytes.into(),
                Some(name),
                Some(mime_type),
            )
            .await
            .map_err(board_error)
    }

    async fn download(&self, principal: &str, id: &str) -> Result<Vec<u8>, SyncError> {
        self.0
            .blobs
            .download(&self.0.board, &self.principal(principal), id)
            .await
            .map(|(_, bytes)| bytes.to_vec())
            .map_err(board_error)
    }
}

pub(crate) fn spawn(
    app: Arc<App>,
    cancel: CancellationToken,
) -> Option<tokio::task::JoinHandle<()>> {
    let registry = app.config.sync.clone()?;
    let worker = SyncWorker::new(registry);
    Some(tokio::spawn(async move {
        let api = LocalApi(app.clone());
        let mut registered = false;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                ()=cancel.cancelled()=>break,
                _=interval.tick()=>{}
                _=app.wake.notified()=>{}
            }
            let operation = async {
                if !registered {
                    worker.register(&api).await?;
                    registered = true;
                    app.sync_ready.store(true, Ordering::Relaxed);
                }
                worker.run_once(&api, 50).await
            };
            tokio::select! {
                ()=cancel.cancelled()=>break,
                result=operation=>{
                    if let Err(cause)=result {
                        // Provider bodies and credentials are deliberately absent from logs.
                        let code=match cause {SyncError::Board{ref code,..}=>code.as_str(),_=>"sync_operation_failed"};
                        tracing::warn!(code,"sync worker will retry durable work");
                    }
                }
            }
        }
    }))
}

fn board_error(error: BoardError) -> SyncError {
    SyncError::Board {
        status: error.status,
        code: error.code,
    }
}
