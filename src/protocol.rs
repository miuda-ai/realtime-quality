//! RustPBX quality WebSocket protocol v1.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize)]
pub(crate) struct Start {
    #[serde(rename = "type")]
    pub kind: String,
    pub version: u32,
    pub sample_rate: u32,
    pub call_id: String,
    pub session_id: String,
    pub side: String,
    pub direction: String,
    pub tenant_id: Option<i64>,
    pub inbound_trunk_id: Option<i64>,
    pub outbound_trunk_id: Option<i64>,
    #[serde(default)]
    pub parameters: Value,
}

#[derive(Serialize)]
#[serde(tag = "method", rename_all = "lowercase")]
pub(crate) enum VoicemailEvidence {
    Beep { detection: crate::beep::Detection },
    Asr { language: String, phrase: String, matched_text: String, end_ms: u64 },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum QualityResponse<'a> {
    Text { text: &'a str, partial: bool, start_ms: u64, end_ms: u64, lang: &'a str },
    Calltrace {
        kind: &'static str,
        code: &'static str,
        severity: &'static str,
        message: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<&'a VoicemailEvidence>,
    },
    Hangup { reason: &'a str, detail: &'a VoicemailEvidence },
}

