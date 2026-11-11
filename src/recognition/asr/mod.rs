//! Shared ASR models and per-connection decoder state.
use validator::Validate;
mod worker;
mod keywords;

pub(crate) use worker::{RecognitionWorkers, RecognitionHandle};
pub(crate) use keywords::{KeywordMatcher, normalize};
use anyhow::{Context, Result, ensure};
use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, OnlineStream};
use tokio::sync::mpsc;
use super::{RecognitionResult, RecognitionEvent, VoicemailEvidence};
use crate::config::RecognitionModelConfig;

/// One handler per connection; receives every decoder update, including repeated text.
pub(crate) trait AsrResultHandler: Send {
    fn handle(&mut self, result: &RecognitionResult) -> AsrResultOutput;
}

pub(crate) struct AsrResultOutput {
    pub detection: Option<VoicemailEvidence>,
}

pub(crate) struct RecognitionStream {
    pub decoder_stream: OnlineStream,
    pub decode_queued: bool,
    language: String,
    start_ms: u64,
    events: mpsc::Sender<RecognitionEvent>,
    max_audio_samples: u64,
    accepted_samples: u64,
}

pub(crate) fn load_recognizer(language: &str, model_config: &RecognitionModelConfig) -> Result<OnlineRecognizer> {
    model_config.validate()?;
    for path in [&model_config.encoder, &model_config.decoder, &model_config.joiner, &model_config.tokens] {
        ensure!(std::path::Path::new(path).is_file(), "ASR model asset missing: {path}");
    }
    let mut recognizer_config = OnlineRecognizerConfig::default();
    recognizer_config.model_config.transducer.encoder = Some(model_config.encoder.clone());
    recognizer_config.model_config.transducer.decoder = Some(model_config.decoder.clone());
    recognizer_config.model_config.transducer.joiner = Some(model_config.joiner.clone());
    recognizer_config.model_config.tokens = Some(model_config.tokens.clone());
    recognizer_config.model_config.provider = Some("cpu".into());
    recognizer_config.model_config.num_threads = model_config.num_threads;
    recognizer_config.decoding_method = Some("greedy_search".into());
    recognizer_config.enable_endpoint = true;
    OnlineRecognizer::create(&recognizer_config).with_context(|| format!("cannot load ASR model for {language}"))
}
impl RecognitionStream {
    pub fn new(recognizer: &OnlineRecognizer, language: &str, inspect_ms: u32, events: mpsc::Sender<RecognitionEvent>) -> Self {
        Self { decoder_stream: recognizer.create_stream(), decode_queued: false, language: language.into(),
            start_ms: 0, events, max_audio_samples: inspect_ms as u64 * 16, accepted_samples: 0 }
    }
    pub fn is_closed(&self) -> bool { self.events.is_closed() }
    pub fn is_finished(&self, recognizer: &OnlineRecognizer) -> bool {
        self.is_closed() || (self.accepted_samples >= self.max_audio_samples && !recognizer.is_ready(&self.decoder_stream))
    }
    pub fn accept_audio(&mut self, pcm: &[f32]) {
        let remaining = self.max_audio_samples.saturating_sub(self.accepted_samples) as usize;
        let accepted_samples = &pcm[..pcm.len().min(remaining)];
        self.accepted_samples += accepted_samples.len() as u64;
        self.decoder_stream.accept_waveform(16000, accepted_samples);
    }
    /// Preserve every decoder update: repeated text also counts toward policy stability.
    pub fn publish_result(&mut self, recognizer: &OnlineRecognizer) -> Result<bool, mpsc::error::TrySendError<RecognitionEvent>> {
        if self.is_closed() { return Ok(false); }
        if let Some(result) = recognizer.get_result(&self.decoder_stream) {
            let endpoint = recognizer.is_endpoint(&self.decoder_stream);
            let end_ms = self.accepted_samples / 16;
            self.events.try_send(RecognitionEvent::Result(RecognitionResult {
                language: self.language.clone(), text: result.text, endpoint,
                start_ms: self.start_ms, end_ms,
            }))?;
            if endpoint {
                recognizer.reset(&self.decoder_stream);
                self.start_ms = end_ms;
            }
        }
        Ok(!self.is_finished(recognizer))
    }
}

