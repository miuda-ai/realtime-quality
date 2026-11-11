//! Shared recognition results and detection evidence; implementations own their state.
mod asr;
mod beep;
mod parameters;

pub(crate) use parameters::InspectionParameters;

pub(crate) use asr::{AsrResultHandler, KeywordMatcher, RecognitionWorkers, RecognitionHandle, normalize};
pub(crate) use beep::BeepDetector;
use serde::Serialize;

/// 100 ms of mono f32 audio at the recognition rate of 16 kHz.
pub(crate) const RECOGNITION_CHUNK_SAMPLES: usize = 1600;

pub(crate) struct RecognitionResult {
    pub language: String,
    pub text: String,
    pub endpoint: bool,
    pub start_ms: u64,
    pub end_ms: u64,
}
pub(crate) enum RecognitionEvent {
    Result(RecognitionResult),
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BeepDetection {
    pub frequency_hz: f64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub tone_ms: u32,
    pub energy_ratio: f64,
}

#[derive(Serialize)]
#[serde(tag = "method", rename_all = "lowercase")]
pub(crate) enum VoicemailEvidence {
    Beep { detection: BeepDetection },
    Asr { language: String, phrase: String, matched_text: String, end_ms: u64 },
}

