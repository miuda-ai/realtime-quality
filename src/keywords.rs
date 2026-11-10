//! Per-stream phrase confirmation over successive ASR decoder updates.
use crate::config::Parameters;

pub fn normalize(text: &str) -> String {
    text.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Consecutive decoder updates, not PCM packets. Word boundaries prevent substring matches.
pub struct KeywordMatcher {
    phrases: Vec<(String, String, u32)>,
    stable_updates: u32,
}
impl KeywordMatcher {
    pub fn new(config: &Parameters, language: &str) -> Self {
        Self { phrases: config.keywords.iter().filter(|k| k.language == language)
            .map(|k| (k.phrase.clone(), format!(" {} ", normalize(&k.phrase)), 0)).collect(),
            stable_updates: config.stable_updates }
    }
    pub fn update(&mut self, text: &str) -> Option<String> {
        let text = format!(" {} ", normalize(text));
        let mut matched = None;
        for (phrase, normalized, count) in &mut self.phrases {
            *count = if text.contains(normalized.as_str()) { count.saturating_add(1) } else { 0 };
            if *count >= self.stable_updates && matched.is_none() { matched = Some(phrase.clone()); }
        }
        matched
    }
    pub fn reset(&mut self) { for (_, _, count) in &mut self.phrases { *count = 0; } }
}

#[cfg(test)]
mod keyword_tests {
    use super::*;
    #[test]
    fn stable_matches_are_per_stream_and_reset_on_revision() {
        let config = Parameters { keywords: vec![crate::config::Keyword { language: "id".into(), phrase: "layanan pesan suara".into() }], ..Default::default() };
        let mut a = KeywordMatcher::new(&config, "id");
        let mut b = KeywordMatcher::new(&config, "id");
        assert!(a.update("LAYANAN PESAN SUARA.").is_none());
        assert!(b.update("layanan pesan suara").is_none());
        assert!(a.update("layanan pesan lain").is_none());
        assert!(a.update("layanan pesan suara").is_none());
        assert!(a.update("layanan pesan suara").is_some());
        b.reset();
        assert!(b.update("layanan pesan suara").is_none());
        assert!(b.update("layanan pesan suaranya").is_none());
    }
}
