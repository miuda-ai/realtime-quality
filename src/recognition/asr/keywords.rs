//! Per-stream phrase confirmation over successive ASR decoder updates.
use std::collections::BTreeMap;
use crate::recognition::InspectionParameters;
use crate::recognition::{RecognitionResult, VoicemailEvidence};
use super::{AsrResultHandler, AsrResultOutput};

pub fn normalize(text: &str) -> String {
    text.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Rule {
    label: String,
    parts: Vec<Vec<String>>,
    max_gap_words: usize,
    confirmations: u32,
}

struct LanguageState {
    rules: Vec<Rule>,
    matched: bool,
}

/// Configurable phrase/sequence interpretation of ASR results, with per-language state.
pub(crate) struct KeywordMatcher {
    languages: BTreeMap<String, LanguageState>,
    stable_updates: u32,
}
impl KeywordMatcher {
    pub fn new(config: &InspectionParameters) -> Self {
        let languages = config.asr_languages.iter().map(|language| {
            let rules = config.keywords.iter().filter(|k| &k.language == language).map(|k| {
                let phrases = k.sequence.clone().unwrap_or_else(|| k.phrase.iter().cloned().collect());
                Rule { label: phrases.join(" … "),
                    parts: phrases.iter().map(|p| normalize(p).split_whitespace().map(str::to_owned).collect()).collect(),
                    max_gap_words: k.max_gap_words.unwrap_or(0), confirmations: 0 }
            }).collect();
            (language.clone(), LanguageState { rules, matched: false })
        }).collect();
        Self { languages, stable_updates: config.stable_updates }
    }
}
impl AsrResultHandler for KeywordMatcher {
    fn handle(&mut self, result: &RecognitionResult) -> AsrResultOutput {
        let Some(state) = self.languages.get_mut(&result.language) else {
            return AsrResultOutput { detection: None };
        };
        let text = normalize(&result.text);
        let words: Vec<_> = text.split_whitespace().collect();
        let mut matched = None;
        for rule in &mut state.rules {
            if state.matched { break; }
            // Keep every reachable end position: a later occurrence may satisfy the gap
            // even when the first occurrence does not. Parts cannot overlap.
            let mut ends = Vec::new();
            for (index, part) in rule.parts.iter().enumerate() {
                let mut next = Vec::new();
                if !part.is_empty() && part.len() <= words.len() {
                    for start in 0..=words.len() - part.len() {
                        if words[start..start + part.len()].iter().zip(part).all(|(a, b)| a == b)
                            && (index == 0 || ends.iter().any(|end| start >= *end && start - end <= rule.max_gap_words)) {
                            next.push(start + part.len());
                        }
                    }
                }
                ends = next;
                if ends.is_empty() { break; }
            }
            rule.confirmations = if ends.is_empty() { 0 } else { rule.confirmations.saturating_add(1) };
            if rule.confirmations >= self.stable_updates && matched.is_none() { matched = Some(rule.label.clone()); }
        }
        state.matched |= matched.is_some();
        if result.endpoint {
            for rule in &mut state.rules { rule.confirmations = 0; }
            state.matched = false;
        }
        AsrResultOutput { detection: matched.map(|phrase| VoicemailEvidence::Asr {
            language: result.language.clone(), phrase, matched_text: result.text.clone(), end_ms: result.end_ms,
        }) }
    }
}

#[cfg(test)]
mod tests {
    use validator::Validate;
    use super::*;

    #[test]
    fn sequence_respects_order_gaps_revisions_and_alternate_occurrences() {
        let config: InspectionParameters = serde_json::from_value(serde_json::json!({
            "asr_enabled": true, "keywords": [{"language":"id",
                "sequence":["rekam", "setelah mendengar nada"], "max_gap_words":2}]
        })).unwrap();
        config.validate().unwrap();
        let mut matcher: Box<dyn AsrResultHandler> = Box::new(KeywordMatcher::new(&config));
        for (text, endpoint, detected) in [
            ("rekam pesan anda setelah mendengar nada", false, false),
            ("rekam pesan anda setelah mendengar nada", true, true),
            ("setelah mendengar nada rekam", false, false),
            ("rekam satu dua tiga setelah mendengar nada", false, false),
            ("rekam satu dua tiga rekam pesan setelah mendengar nada", false, false),
            ("rekam satu dua tiga rekam pesan setelah mendengar nada", true, true),
            ("rekam", true, false),
            ("setelah mendengar nada", false, false),
        ] {
            let result = RecognitionResult { language:"id".into(), text:text.into(), endpoint, start_ms:0, end_ms:100 };
            assert_eq!(matcher.handle(&result).detection.is_some(), detected, "{text}");
        }
    }

    #[test]
    fn handlers_isolate_languages_connections_and_reset_on_revisions_and_endpoints() {
        let config: InspectionParameters = serde_json::from_value(serde_json::json!({
            "asr_enabled":true, "asr_languages":["en","id"], "keywords":[
                {"language":"en","phrase":"voice mail"}, {"language":"id","phrase":"pesan suara"}]
        })).unwrap();
        let mut a: Box<dyn AsrResultHandler> = Box::new(KeywordMatcher::new(&config));
        let mut b: Box<dyn AsrResultHandler> = Box::new(KeywordMatcher::new(&config));
        let mut result = RecognitionResult { language:"en".into(), text:"VOICE MAIL.".into(), endpoint:false, start_ms:0, end_ms:100 };
        assert!(a.handle(&result).detection.is_none());
        assert!(b.handle(&result).detection.is_none());
        result.text = "voice man".into();
        assert!(a.handle(&result).detection.is_none());
        result.text = "voice mail".into();
        assert!(a.handle(&result).detection.is_none());
        assert!(matches!(a.handle(&result).detection, Some(VoicemailEvidence::Asr { phrase, .. }) if phrase == "voice mail"));
        result.endpoint = true;
        assert!(a.handle(&result).detection.is_none());
        result.endpoint = false;
        assert!(a.handle(&result).detection.is_none(), "new utterance starts confirmation again");
        assert!(a.handle(&result).detection.is_some(), "new utterance can report another detection");
        assert!(a.handle(&result).detection.is_none(), "repeated partial does not repeat the same detection");
        result.language = "id".into();
        result.text = "pesan suara".into();
        result.endpoint = true;
        assert!(a.handle(&result).detection.is_none());
        result.endpoint = false;
        assert!(a.handle(&result).detection.is_none(), "endpoint resets confirmations");
        result.text = "pesan suaranya".into();
        assert!(a.handle(&result).detection.is_none(), "respect word boundaries");
        result.text = "pesan suara".into();
        assert!(a.handle(&result).detection.is_none());
        assert!(matches!(a.handle(&result).detection, Some(VoicemailEvidence::Asr { phrase, .. }) if phrase == "pesan suara"));
    }
}
