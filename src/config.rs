//! Service model assets and per-connection detection parameters.
use serde::{Deserialize, Serialize};
use crate::keywords::normalize;

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub listen: Option<String>,
    pub token: Option<String>,
    pub detector: Parameters,
    pub models: std::collections::BTreeMap<String, ModelConfig>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    pub encoder: String,
    pub decoder: String,
    pub joiner: String,
    pub tokens: String,
    #[serde(default = "one")]
    pub num_threads: i32,
    #[serde(default = "default_batch_size")]
    pub max_batch_size: usize,
    #[serde(default = "default_batch_wait")]
    pub max_wait_ms: u64,
}
fn one() -> i32 { 1 }
fn default_batch_size() -> usize { 4 }
fn default_batch_wait() -> u64 { 5 }


#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Keyword {
    pub language: String,
    pub phrase: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Parameters {
    pub beep_enabled: bool,
    pub asr_enabled: bool,
    pub asr_languages: Vec<String>,
    pub keywords: Vec<Keyword>,
    pub stable_updates: u32,
    pub hangup_on_asr: bool,
    /// Optional single language used for subtitles; all selected languages detect keywords.
    pub text_language: Option<String>,
    pub frequencies_hz: Vec<f64>,
    pub tolerance_hz: f64,
    pub min_tone_ms: u32,
    pub max_gap_ms: u32,
    pub min_dbfs: f64,
    pub min_energy_ratio: f64,
    pub inspect_ms: u32,
    pub hangup_on_beep: bool,
}
impl Default for Parameters {
    fn default() -> Self {
        Self { beep_enabled: true, asr_enabled: false, asr_languages: vec!["id".into()],
            keywords: Vec::new(),
            stable_updates: 2, hangup_on_asr: false, text_language: None, frequencies_hz: vec![1309.0, 660.0], tolerance_hz: 20.0,
            min_tone_ms: 160, max_gap_ms: 40, min_dbfs: -35.0,
            min_energy_ratio: 0.8, inspect_ms: 30000, hangup_on_beep: false }
    }
}
impl Parameters {
    pub fn validate(&self, rate: u32) -> anyhow::Result<()> {
        anyhow::ensure!((1..=10).contains(&self.stable_updates), "stable_updates must be 1..10");
        anyhow::ensure!(self.asr_languages.iter().all(|lang| matches!(lang.as_str(), "en" | "id")), "only en and id ASR languages are supported");
        anyhow::ensure!(self.keywords.iter().all(|keyword| matches!(keyword.language.as_str(), "en" | "id")), "keyword language must be en or id");
        if self.asr_enabled {
            anyhow::ensure!(!self.asr_languages.is_empty() && self.asr_languages.len() <= 2, "select English (en), Indonesian (id), or both");
            let unique: std::collections::HashSet<_> = self.asr_languages.iter().collect();
            anyhow::ensure!(unique.len() == self.asr_languages.len(), "duplicate ASR language");
            anyhow::ensure!(self.text_language.as_ref().is_none_or(|lang| self.asr_languages.contains(lang)), "text_language must be selected");
            anyhow::ensure!(self.keywords.len() <= 256, "too many keywords");
            anyhow::ensure!(self.keywords.iter().all(|k| !normalize(&k.phrase).is_empty() && k.phrase.len() <= 512), "invalid keyword");
            anyhow::ensure!(self.asr_languages.iter().all(|lang| self.keywords.iter().any(|k| &k.language == lang)), "each ASR language needs keywords");
        }
        anyhow::ensure!((8000..=48000).contains(&rate), "unsupported sample rate");
        if self.beep_enabled {
        anyhow::ensure!(!self.frequencies_hz.is_empty() && self.frequencies_hz.len() <= 16, "expected 1..16 frequencies");
        anyhow::ensure!(self.tolerance_hz.is_finite() && (0.0..=100.0).contains(&self.tolerance_hz), "invalid frequency tolerance");
        anyhow::ensure!(self.frequencies_hz.iter().all(|f| f.is_finite() && *f > self.tolerance_hz && *f + self.tolerance_hz < rate as f64 / 2.0), "frequency outside Nyquist range");
        anyhow::ensure!((20..=5000).contains(&self.min_tone_ms) && self.max_gap_ms <= 500, "invalid tone duration/gap");
        anyhow::ensure!(self.min_dbfs.is_finite() && (-90.0..=0.0).contains(&self.min_dbfs), "invalid dBFS threshold");
        anyhow::ensure!(self.min_energy_ratio.is_finite() && (0.5..=1.0).contains(&self.min_energy_ratio), "invalid energy ratio");
        }
        anyhow::ensure!((20..=300000).contains(&self.inspect_ms), "inspection duration must be 20..300000 ms");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn asr_requires_configured_keywords_and_supported_languages() {
        assert!(Parameters::default().keywords.is_empty());
        let mut parameters = Parameters { asr_enabled: true, asr_languages: vec!["en".into(), "id".into()], beep_enabled: false, frequencies_hz: Vec::new(), ..Default::default() };
        assert!(parameters.validate(16000).is_err());
        parameters.keywords = vec![Keyword { language: "en".into(), phrase: "leave a message".into() }, Keyword { language: "id".into(), phrase: "pesan suara".into() }];
        assert!(parameters.validate(16000).is_ok());
        parameters.asr_languages.push("fr".into());
        assert!(parameters.validate(16000).is_err());
        let example: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        example.detector.validate(16000).unwrap();
    }
}
