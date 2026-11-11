//! Per-connection recognition settings, defaults and validation.
use serde::{Deserialize, Serialize};
use validator::{Validate, ValidationError};
use super::normalize;

#[derive(Debug, Clone, Default, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
#[validate(schema(function = "validate_keyword_shape"))]
pub struct KeywordRule {
    #[validate(custom(function = "validate_language"))]
    pub language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(length(min = 1, max = 512), custom(function = "validate_phrase"))]
    pub phrase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(length(min = 2, max = 8), custom(function = "validate_sequence"))]
    pub sequence: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(range(max = 100))]
    pub max_gap_words: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(default, deny_unknown_fields)]
#[validate(schema(function = "validate_detection_settings"))]
pub struct InspectionParameters {
    pub beep_enabled: bool,
    pub asr_enabled: bool,
    #[validate(length(max = 2), custom(function = "validate_languages"))]
    pub asr_languages: Vec<String>,
    #[validate(length(max = 256), nested)]
    pub keywords: Vec<KeywordRule>,
    #[validate(range(min = 1, max = 10))]
    pub stable_updates: u32,
    pub hangup_on_detection: bool,
    #[validate(length(max = 16), custom(function = "validate_frequencies"))]
    pub frequencies_hz: Vec<f64>,
    #[validate(range(min = 0.0, max = 100.0), custom(function = "validate_finite"))]
    pub tolerance_hz: f64,
    #[validate(range(min = 20, max = 5000))]
    pub min_tone_ms: u32,
    #[validate(range(max = 500))]
    pub max_gap_ms: u32,
    #[validate(range(min = -90.0, max = 0.0), custom(function = "validate_finite"))]
    pub min_dbfs: f64,
    #[validate(range(min = 0.5, max = 1.0), custom(function = "validate_finite"))]
    pub min_energy_ratio: f64,
    #[validate(range(min = 20, max = 300000))]
    pub inspect_ms: u32,
}
impl Default for InspectionParameters {
    fn default() -> Self {
        Self { beep_enabled: true, asr_enabled: false, asr_languages: vec!["id".into()],
            keywords: Vec::new(),
            stable_updates: 2, hangup_on_detection: false, frequencies_hz: vec![1309.0, 660.0], tolerance_hz: 20.0,
            min_tone_ms: 160, max_gap_ms: 40, min_dbfs: -35.0,
            min_energy_ratio: 0.8, inspect_ms: 15000 }
    }
}
fn validate_language(language: &str) -> Result<(), ValidationError> {
    if matches!(language, "en" | "id") { Ok(()) } else { Err(ValidationError::new("unsupported_language")) }
}

fn validate_languages(languages: &[String]) -> Result<(), ValidationError> {
    for language in languages { validate_language(language)?; }
    if languages.iter().collect::<std::collections::HashSet<_>>().len() != languages.len() {
        return Err(ValidationError::new("duplicate_language"));
    }
    Ok(())
}

fn validate_phrase(phrase: &str) -> Result<(), ValidationError> {
    if normalize(phrase).is_empty() { Err(ValidationError::new("empty_normalized_phrase")) } else { Ok(()) }
}

fn validate_sequence(sequence: &[String]) -> Result<(), ValidationError> {
    for phrase in sequence {
        validate_phrase(phrase)?;
        if phrase.chars().count() > 512 { return Err(ValidationError::new("sequence_phrase_too_long")); }
    }
    Ok(())
}

fn validate_keyword_shape(rule: &KeywordRule) -> Result<(), ValidationError> {
    match (&rule.phrase, &rule.sequence, rule.max_gap_words) {
        (Some(_), None, None) | (None, Some(_), Some(_)) => Ok(()),
        _ => Err(ValidationError::new("expected_phrase_or_sequence_with_gap")),
    }
}

fn validate_finite(value: f64) -> Result<(), ValidationError> {
    if value.is_finite() { Ok(()) } else { Err(ValidationError::new("not_finite")) }
}

fn validate_frequencies(frequencies: &[f64]) -> Result<(), ValidationError> {
    if frequencies.iter().all(|f| f.is_finite() && *f > 0.0 && *f < 8000.0) { Ok(()) }
    else { Err(ValidationError::new("frequency_outside_16k_nyquist_range")) }
}

fn validate_detection_settings(parameters: &InspectionParameters) -> Result<(), ValidationError> {
    if parameters.asr_enabled {
        if parameters.asr_languages.is_empty() { return Err(ValidationError::new("asr_requires_languages")); }
        if parameters.asr_languages.iter().any(|lang| !parameters.keywords.iter().any(|k| &k.language == lang)) {
            return Err(ValidationError::new("each_asr_language_requires_keywords"));
        }
    }
    if parameters.beep_enabled {
        if parameters.frequencies_hz.is_empty() { return Err(ValidationError::new("beep_requires_frequencies")); }
        if parameters.frequencies_hz.iter().any(|f| *f <= parameters.tolerance_hz || *f + parameters.tolerance_hz >= 8000.0) {
            return Err(ValidationError::new("frequency_tolerance_outside_16k_nyquist_range"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validation_reports_fields_and_nested_keyword_indices() {
        use serde_json::json;
        for (field, value) in [("stable_updates", json!(0)), ("inspect_ms", json!(300001)),
            ("min_tone_ms", json!(19)), ("max_gap_ms", json!(501)), ("tolerance_hz", json!(101)),
            ("min_dbfs", json!(1)), ("min_energy_ratio", json!(0.4))] {
            let parameters: InspectionParameters = serde_json::from_value(json!({field:value})).unwrap();
            assert!(parameters.validate().unwrap_err().field_errors().contains_key(field), "{field}");
        }
        let parameters: InspectionParameters = serde_json::from_value(json!({"asr_enabled":true,"keywords":[
            {"language":"id","phrase":"!!!"}]})).unwrap();
        let errors = parameters.validate().unwrap_err();
        let validator::ValidationErrorsKind::List(rules) = &errors.errors()["keywords"] else { panic!("expected indexed keyword errors") };
        assert!(rules[&0].field_errors().contains_key("phrase"));
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let parameters = InspectionParameters { tolerance_hz:value, ..Default::default() };
            assert!(parameters.validate().is_err());
        }
        let parameters = InspectionParameters { frequencies_hz:vec![7990.0], tolerance_hz:20.0, ..Default::default() };
        assert!(parameters.validate().is_err());
    }

    #[test]
    fn keyword_rule_shapes_are_validated() {
        use serde_json::json;
        for rule in [json!({"phrase":"hello", "sequence":["hello","world"], "max_gap_words":1}),
            json!({"sequence":["hello","world"]}), json!({"sequence":["hello"],"max_gap_words":0}),
            json!({"sequence":["hello","!!!"],"max_gap_words":0}),
            json!({"sequence":["hello","world"],"max_gap_words":101}),
            json!({"phrase":"hello","max_gap_words":1})] {
            let mut rule = rule;
            rule["language"] = json!("id");
            let parameters: InspectionParameters = serde_json::from_value(json!({"asr_enabled":true,"keywords":[rule]})).unwrap();
            assert!(parameters.validate().is_err());
        }
        assert_eq!(InspectionParameters::default().inspect_ms, 15000);
        assert!(serde_json::from_value::<InspectionParameters>(json!({"hangup_on_asr":true})).is_err());
    }

    #[test]
    fn asr_requires_configured_keywords_and_supported_languages() {
        assert!(InspectionParameters::default().keywords.is_empty());
        let mut parameters = InspectionParameters { asr_enabled: true, asr_languages: vec!["en".into(), "id".into()], beep_enabled: false, frequencies_hz: Vec::new(), ..Default::default() };
        assert!(parameters.validate().is_err());
        parameters.keywords = vec![KeywordRule { language: "en".into(), phrase: Some("leave a message".into()), ..Default::default() }, KeywordRule { language: "id".into(), phrase: Some("pesan suara".into()), ..Default::default() }];
        assert!(parameters.validate().is_ok());
        parameters.asr_languages.push("fr".into());
        assert!(parameters.validate().is_err());
    }
}
