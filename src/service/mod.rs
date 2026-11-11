//! HTTP service, per-connection processing and PBX protocol.
mod recognition_task;
pub(crate) mod audio;
mod protocol;

use protocol::{StartMessage, QualityEvent};

use std::{future::Future, sync::Arc};
use anyhow::Result;
use axum::{Router, extract::{State, WebSocketUpgrade}, http::{HeaderMap, StatusCode}, response::IntoResponse, routing::get};
use self::recognition_task::RecognitionTask;
use tokio::{net::TcpListener, sync::Semaphore};
use crate::config::ServiceConfig;
use crate::recognition::RecognitionWorkers;


/// Prepared service resources. Models are loaded once, before accepting connections.
pub struct QualityService {
    config: ServiceConfig,
    recognition_workers: Option<RecognitionWorkers>,
    connections: Arc<Semaphore>,
    shutdown: tokio::sync::watch::Sender<bool>,
}
impl QualityService {
    pub fn prepare(config: ServiceConfig) -> Result<Self> {
        anyhow::ensure!(config.models.keys().all(|lang| matches!(lang.as_str(), "en" | "id")), "only en and id models are supported");
        let recognition_workers = if config.models.is_empty() {
            None
        } else {
            let worker = RecognitionWorkers::start(&config.models)?;
            Some(worker)
        };
        Ok(Self { config, recognition_workers, connections: Arc::new(Semaphore::new(256)),
            shutdown: tokio::sync::watch::channel(false).0 })
    }

    /// Serve an already-bound listener. The embedding application owns shutdown signaling.
    pub async fn serve(self, listener: TcpListener, shutdown: impl Future<Output = ()> + Send + 'static) -> Result<()> {
        let service = Arc::new(self);
        let stopping = service.clone();
        let result = axum::serve(listener, service.clone().router())
            .with_graceful_shutdown(async move {
                shutdown.await;
                stopping.shutdown.send_replace(true);
            }).await;
        service.shutdown.send_replace(true);
        // Upgrade tasks own permits until their connection and Close handshake end.
        let _connections = service.connections.acquire_many(256).await?;
        if let Some(worker) = &service.recognition_workers { worker.shutdown().await?; }
        result?;
        Ok(())
    }

    fn router(self: Arc<Self>) -> Router {
        Router::new().route("/health", get(|| async { "ok" }))
            .route("/v1/quality", get(Self::upgrade)).with_state(self)
    }

    async fn upgrade(State(service): State<Arc<Self>>, headers: HeaderMap, ws: WebSocketUpgrade) -> impl IntoResponse {
        if *service.shutdown.borrow() { return StatusCode::SERVICE_UNAVAILABLE.into_response(); }
        if let Some(token) = &service.config.token {
            if headers.get("authorization").and_then(|h| h.to_str().ok()) != Some(format!("Bearer {token}").as_str()) {
                return StatusCode::UNAUTHORIZED.into_response();
            }
        }
        let Ok(permit) = service.connections.clone().try_acquire_owned() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        ws.max_message_size(65536).max_frame_size(65536).on_upgrade(move |socket| async move {
            let _permit = permit;
            if let Ok(Some(task)) = RecognitionTask::initialize(socket, &service).await {
                let _ = task.run().await;
            }
        }).into_response()
    }
}
