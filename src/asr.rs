//! One shared recognizer per language, with independent stream and keyword state per connection.
use anyhow::{Context, Result, ensure};
use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, OnlineStream};
use tokio::sync::mpsc;
use crate::{config::{ModelConfig, Parameters}, keywords::KeywordMatcher};

pub(crate) enum Output {
    QueueFull,
    Text { language: String, text: String, partial: bool, start_ms: u64, end_ms: u64 },
    Match { language: String, phrase: String, text: String, end_ms: u64 },
}
pub(crate) struct Session {
    pub stream: OnlineStream,
    pub queued: bool,
    language: String,
    matcher: KeywordMatcher,
    last_text: String,
    start_ms: u64,
    output: mpsc::Sender<Output>,
    config: Parameters,
    samples: u64,
    matched: bool,
}

pub(crate) fn load(language: &str, model: &ModelConfig) -> Result<OnlineRecognizer> {
    ensure!((1..=32).contains(&model.num_threads), "invalid ASR num_threads");
    ensure!((1..=64).contains(&model.max_batch_size), "max_batch_size must be 1..64");
    ensure!(model.max_wait_ms <= 100, "max_wait_ms must be 0..100");
    for path in [&model.encoder, &model.decoder, &model.joiner, &model.tokens] {
        ensure!(std::path::Path::new(path).is_file(), "ASR model asset missing: {path}");
    }
    let mut config = OnlineRecognizerConfig::default();
    config.model_config.transducer.encoder = Some(model.encoder.clone());
    config.model_config.transducer.decoder = Some(model.decoder.clone());
    config.model_config.transducer.joiner = Some(model.joiner.clone());
    config.model_config.tokens = Some(model.tokens.clone());
    config.model_config.provider = Some("cpu".into());
    config.model_config.num_threads = model.num_threads;
    config.decoding_method = Some("greedy_search".into());
    config.enable_endpoint = true;
    OnlineRecognizer::create(&config).with_context(|| format!("cannot load ASR model for {language}"))
}
impl Session {
    pub fn new(model: &OnlineRecognizer, language: &str, config: Parameters, output: mpsc::Sender<Output>) -> Self {
        Self { stream: model.create_stream(), queued: false, language: language.into(),
            matcher: KeywordMatcher::new(&config, language), last_text: String::new(), start_ms: 0,
            output, config, samples: 0, matched: false }
    }
    pub fn report_queue_full(&self) {
        eprintln!("ASR pending queue full for language {}; ending connection recognition", self.language);
        // The shared failure flag also covers a full output queue: the next queued
        // result or input submission observes failure even if this wakeup cannot fit.
        let _ = self.output.try_send(Output::QueueFull);
    }
    pub fn is_closed(&self) -> bool { self.output.is_closed() }
    pub fn finished(&self, model: &OnlineRecognizer) -> bool {
        self.is_closed() || (self.samples >= self.config.inspect_ms as u64 * 16 && !model.is_ready(&self.stream))
    }
    pub fn accept(&mut self, pcm: &[f32]) {
        let remaining = (self.config.inspect_ms as u64 * 16).saturating_sub(self.samples) as usize;
        let samples = &pcm[..pcm.len().min(remaining)];
        self.samples += samples.len() as u64;
        self.stream.accept_waveform(16000, samples);
    }
    /// Called exactly once after this stream's batch decode, before another decode or reset.
    pub fn publish(&mut self, model: &OnlineRecognizer) -> bool {
        if self.is_closed() { return false; }
        let end_ms = self.samples / 16;
        let mut failed = false;
        if let Some(result) = model.get_result(&self.stream) {
            if !self.matched {
                if let Some(phrase) = self.matcher.update(&result.text) {
                    self.matched = true;
                    failed |= self.output.try_send(Output::Match {
                        language: self.language.clone(), phrase, text: result.text.clone(), end_ms,
                    }).is_err();
                }
            }
            let endpoint = model.is_endpoint(&self.stream);
            if self.config.text_language.as_ref() == Some(&self.language)
                && !result.text.is_empty() && (endpoint || result.text != self.last_text) {
                failed |= self.output.try_send(Output::Text { language: self.language.clone(),
                    text: result.text.clone(), partial: !endpoint, start_ms: self.start_ms, end_ms }).is_err();
            }
            self.last_text = result.text;
            if endpoint {
                model.reset(&self.stream);
                self.matcher.reset();
                self.last_text.clear();
                self.start_ms = end_ms;
            }
        }
        if failed && !self.is_closed() { eprintln!("ASR result queue full; ending recognition"); }
        !failed && !self.finished(model)
    }
}
