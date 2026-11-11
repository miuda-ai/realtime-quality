//! Process configuration: listener, authentication and shared model resources.
use serde::Deserialize;
use validator::Validate;

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServiceConfig {
    pub listen: Option<String>,
    pub token: Option<String>,
    pub models: std::collections::BTreeMap<String, RecognitionModelConfig>,
}

#[derive(Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct RecognitionModelConfig {
    pub encoder: String,
    pub decoder: String,
    pub joiner: String,
    pub tokens: String,
    #[serde(default = "default_num_threads")]
    #[validate(range(min = 1, max = 32))]
    pub num_threads: i32,
    #[serde(default = "default_batch_size")]
    #[validate(range(min = 1, max = 64))]
    pub max_batch_size: usize,
    #[serde(default = "default_batch_wait_ms")]
    #[validate(range(max = 100))]
    pub max_wait_ms: u64,
}
fn default_num_threads() -> i32 { 1 }
fn default_batch_size() -> usize { 4 }
fn default_batch_wait_ms() -> u64 { 5 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_resource_limits_report_the_invalid_fields() {
        let model = RecognitionModelConfig { encoder:String::new(), decoder:String::new(), joiner:String::new(),
            tokens:String::new(), num_threads:0, max_batch_size:65, max_wait_ms:101 };
        let errors = model.validate().unwrap_err();
        for field in ["num_threads", "max_batch_size", "max_wait_ms"] {
            assert!(errors.field_errors().contains_key(field));
        }
    }
}
