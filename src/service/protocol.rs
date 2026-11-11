//! RustPBX quality WebSocket protocol v1.
use validator::Validate;
use serde::{Deserialize, Serialize};
use crate::recognition::{InspectionParameters, VoicemailEvidence};

#[derive(Deserialize, Validate)]
pub(crate) struct StartMessage {
    #[serde(rename = "type")]
    pub _kind: StartKind,
    #[validate(range(min = 1, max = 1, message = "unsupported protocol version"))]
    pub version: u32,
    pub sample_rate: u32,
    pub call_id: String,
    pub session_id: String,
    pub side: QualitySide,
    #[serde(rename = "direction")]
    pub _direction: CallDirection,
    #[serde(rename = "tenant_id")]
    pub _tenant_id: Option<i64>,
    #[serde(rename = "inbound_trunk_id")]
    pub _inbound_trunk_id: Option<i64>,
    #[serde(rename = "outbound_trunk_id")]
    pub _outbound_trunk_id: Option<i64>,
    #[serde(default)]
    #[validate(nested)]
    pub parameters: InspectionParameters,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum StartKind { Start }

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum QualitySide { Caller, Callee }
impl QualitySide {
    pub fn as_str(&self) -> &'static str {
        match self { Self::Caller => "caller", Self::Callee => "callee" }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CallDirection { Inbound, Outbound, Internal }

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum QualityEvent<'a> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn protocol_values_and_nested_settings_are_validated_at_the_boundary() {
        let base = json!({"type":"start","version":1,"sample_rate":16000,
            "call_id":"","session_id":"","side":"callee","direction":"outbound","tenant_id":0});
        serde_json::from_value::<StartMessage>(base.clone()).unwrap().validate().unwrap();
        for (field, value) in [("type", json!("stop")), ("side", json!("both")), ("direction", json!("unknown"))] {
            let mut request = base.clone(); request[field] = value;
            assert!(serde_json::from_value::<StartMessage>(request).is_err());
        }
        let mut request = base.clone(); request["version"] = json!(2);
        assert!(serde_json::from_value::<StartMessage>(request).unwrap().validate().unwrap_err().field_errors().contains_key("version"));
        let mut request = base; request["parameters"] = json!({"inspect_ms":0});
        assert!(serde_json::from_value::<StartMessage>(request).unwrap().validate().unwrap_err().errors().contains_key("parameters"));
    }

    #[test]
    fn start_parses_connection_parameters_without_creating_resources() {
        let base = json!({"type":"start","version":1,"sample_rate":16000,
            "call_id":"call","session_id":"session","side":"callee","direction":"outbound"});
        let default: StartMessage = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(default.parameters.inspect_ms, 15000);
        assert!(!default.parameters.asr_enabled);
        let mut request = base;
        request["parameters"] = json!({"asr_enabled":true,"inspect_ms":5000,
            "keywords":[{"language":"id","sequence":["rekam","setelah mendengar nada"],"max_gap_words":10}]});
        // Model availability belongs to task setup, not request deserialization.
        let start: StartMessage = serde_json::from_value(request.clone()).unwrap();
        start.parameters.validate().unwrap();
        assert_eq!(start.parameters.inspect_ms, 5000);
        assert_eq!(start.parameters.keywords[0].sequence.as_ref().unwrap().len(), 2);
        request["parameters"] = json!({});
        assert_eq!(serde_json::from_value::<StartMessage>(request.clone()).unwrap().parameters.inspect_ms, 15000);
        for invalid in [json!(null), json!(true), json!({"inspect_ms":"5000"}), json!({"unknown":true})] {
            request["parameters"] = invalid;
            assert!(serde_json::from_value::<StartMessage>(request.clone()).is_err());
        }
    }
}
