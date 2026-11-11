//! Realtime quality service: shared ASR models, per-connection state and optional beep detection.
pub mod config;
mod service;
mod recognition;

pub use service::QualityService;
