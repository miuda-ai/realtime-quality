//! Realtime quality service: shared ASR models, per-connection state and optional beep detection.
pub mod config;
pub mod beep;
mod asr;
mod audio;
mod keywords;
mod protocol;
mod web;
mod worker;

use std::{future::Future, sync::Arc};
use anyhow::Result;
use tokio::{net::TcpListener, sync::Semaphore};
use config::Config;
use worker::RecognitionWorkers;

pub use beep::{Detection, Detector};
pub use config::{Keyword, Parameters};

/// Prepared service resources. Models are loaded once, before accepting connections.
pub struct QualityService {
    config: Config,
    worker: Option<RecognitionWorkers>,
    connections: Arc<Semaphore>,
}
impl QualityService {
    pub fn prepare(config: Config) -> Result<Self> {
        anyhow::ensure!(config.models.keys().all(|lang| matches!(lang.as_str(), "en" | "id")), "only en and id models are supported");
        config.detector.validate(16000)?;
        let worker = if config.models.is_empty() {
            anyhow::ensure!(!config.detector.asr_enabled, "no ASR models configured");
            None
        } else {
            let worker = RecognitionWorkers::load(&config.models)?;
            worker.validate(&config.detector)?;
            Some(worker)
        };
        Ok(Self { config, worker, connections: Arc::new(Semaphore::new(256)) })
    }

    /// Serve an already-bound listener. The embedding application owns shutdown signaling.
    pub async fn serve(self, listener: TcpListener, shutdown: impl Future<Output = ()> + Send + 'static) -> Result<()> {
        axum::serve(listener, web::router(Arc::new(self)))
            .with_graceful_shutdown(shutdown).await?;
        Ok(())
    }
}
