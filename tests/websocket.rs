use std::time::Duration;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use realtime_quality::{QualityService, config::{Config, ModelConfig, Parameters}};

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn idle_service_sends_ping_every_ten_seconds() {
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/quality", listener.local_addr().unwrap());
        let service = QualityService::prepare(Config::default()).unwrap();
        let server = tokio::spawn(service.serve(listener, std::future::pending()));
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        ws.send(WsMessage::Text(json!({"type":"start","version":1,"sample_rate":16000,"call_id":"c","session_id":"s","side":"caller","direction":"outbound"}).to_string().into())).await.unwrap();
        for _ in 0..2 {
            let started = tokio::time::Instant::now();
            let message = tokio::time::timeout(Duration::from_secs(12), ws.next()).await.unwrap().unwrap().unwrap();
            assert!(matches!(message, WsMessage::Ping(_)));
            assert!(started.elapsed() >= Duration::from_secs(9), "heartbeat must not fire immediately");
            ws.flush().await.unwrap();
        }
        ws.close(None).await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn websocket_auth_chunked_tone_and_hangup() {
        use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as WsMessage};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/quality", listener.local_addr().unwrap());
        let config = Config { token: Some("test-token".into()), ..Default::default() };
        let service = QualityService::prepare(config).unwrap();
        let server = tokio::spawn(service.serve(listener, std::future::pending()));
        assert!(tokio_tungstenite::connect_async(&url).await.is_err());
        for (side, rate) in [("caller", 8000), ("callee", 16000), ("callee", 48000)] {
        let mut request = url.as_str().into_client_request().unwrap();
        request.headers_mut().insert("authorization", "Bearer test-token".parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        ws.send(WsMessage::Text(json!({"type":"start","version":1,"sample_rate":rate,"call_id":"call","session_id":"session","side":side,"direction":"outbound","tenant_id":7,"inbound_trunk_id":6,"outbound_trunk_id":10,"parameters":{"hangup_on_beep":true}}).to_string().into())).await.unwrap();
        let pcm: Vec<u8> = (0..rate / 3).flat_map(|i| ((12000.0 * (std::f64::consts::TAU * 1309.0 * i as f64 / rate as f64).sin()) as i16).to_le_bytes()).collect();
        for chunk in pcm.chunks(137) { ws.send(WsMessage::Binary(chunk.to_vec().into())).await.unwrap(); }
        for expected in ["calltrace", "hangup"] {
            let message = tokio::time::timeout(Duration::from_secs(2), ws.next()).await.unwrap().unwrap().unwrap().into_text().unwrap();
            let message: Value = serde_json::from_str(&message).unwrap();
            assert_eq!(message["type"], expected);
            if expected == "calltrace" {
                assert_eq!(message["kind"], "quality");
                assert_eq!(message["code"], "quality.voicemail_detected");
                assert_eq!(message["severity"], "info");
                assert!(message.get("ts").is_none());
            }
            assert!(message.get("side").is_none());
            assert!(message["detail"].get("side").is_none());
            assert_eq!(message["detail"]["method"], "beep");
            assert!(message["detail"].get("decision").is_none());
            if expected == "hangup" { assert_eq!(message["reason"], "quality.voicemail_detected"); }
            assert_eq!(message["detail"]["detection"]["frequency_hz"], 1309.0);
        }
        ws.close(None).await.unwrap();
        }
        server.abort();
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    #[tokio::test]
    async fn configuration_modes_and_missing_model() {
        assert!(QualityService::prepare(Config::default()).is_ok());
        assert!(QualityService::prepare(Config { detector: Parameters { asr_enabled: true, ..Default::default() }, ..Default::default() }).is_err());
        let config: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        config.detector.validate(16000).unwrap();
    }

    /// Opt in with local model/recording paths; no downloads or uploads in tests.
    #[tokio::test]
    #[ignore = "requires QUALITY_TEST_MODEL_DIR and QUALITY_TEST_WAV"]
    async fn shared_model_streaming_websockets_are_isolated() {
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        let root = std::env::var("QUALITY_TEST_MODEL_DIR").unwrap();
        let language = std::env::var("QUALITY_TEST_LANGUAGE").unwrap_or("id".into());
        let phrase = if language == "en" { "forwarded to voice mail" } else { "pesan suara" };
        let suffix = if language == "en" { "-epoch-99-avg-1-chunk-16-left-128" } else { "-iter-100000-avg-15-chunk-32-left-256" };
        let model = ModelConfig {
            encoder: format!("{root}/encoder{suffix}.int8.onnx"),
            decoder: format!("{root}/decoder{suffix}.onnx"),
            joiner: format!("{root}/joiner{suffix}.int8.onnx"),
            tokens: format!("{root}/tokens.txt"), num_threads: 2, max_batch_size: 4, max_wait_ms: 5,
        };
        let mut config = Config { models: [(language.clone(), model)].into(), ..Default::default() };
        if let Ok(path) = std::env::var("QUALITY_TEST_CONFIG") {
            config = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        }
        let languages: Vec<_> = config.models.keys().cloned().collect();
        let mut keywords = config.detector.keywords.clone();
        keywords.push(realtime_quality::Keyword { language: language.clone(), phrase: phrase.into() });
        let service = QualityService::prepare(config).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/quality", listener.local_addr().unwrap());
        let server = tokio::spawn(service.serve(listener, std::future::pending()));
        let mut wav = hound::WavReader::open(std::env::var("QUALITY_TEST_WAV").unwrap()).unwrap();
        let rate = wav.spec().sample_rate;
        assert!(matches!(rate, 8000 | 16000));
        assert_eq!(wav.spec().channels, 1);
        // Send native-rate PCM; the service performs any necessary resampling.
        let speech: Vec<i16> = wav.samples::<i16>().map(Result::unwrap).collect();
        for (beep_enabled, tone) in [(false, false), (true, false), (true, true)] {
            let (mut active, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            let (mut silent, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            for ws in [&mut active, &mut silent] {
                ws.send(WsMessage::Text(json!({"type":"start","version":1,"sample_rate":rate,"call_id":"test","session_id":"test","side":"callee","direction":"outbound",
                    "parameters":{"asr_enabled":true,"beep_enabled":beep_enabled,"asr_languages":languages,"text_language":language,
                        "keywords":keywords,"hangup_on_asr":true,"hangup_on_beep":true,"inspect_ms":10000}
                }).to_string().into())).await.unwrap();
            }
            let (mut writer, mut reader) = active.split();
            let (mut silent_writer, mut silent_reader) = silent.split();
            let pcm = if tone {
                (0..rate).map(|i| (12000.0 * (std::f64::consts::TAU * 1309.0 * i as f64 / rate as f64).sin()) as i16).collect()
            } else { speech.clone() };
            let send = tokio::spawn(async move {
                for chunk in pcm.chunks(rate as usize / 50) {
                    let bytes: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
                    writer.send(WsMessage::Binary(bytes.into())).await.unwrap();
                    silent_writer.send(WsMessage::Binary(vec![0; chunk.len()*2].into())).await.unwrap();
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                (writer, silent_writer)
            });
            let mut texts = 0;
            let mut traces = 0;
            tokio::time::timeout(Duration::from_secs(9), async {
                while let Some(message) = reader.next().await {
                    if let WsMessage::Text(text) = message.unwrap() {
                        let value: Value = serde_json::from_str(&text).unwrap();
                        eprintln!("stream output: {value}");
                        match value["type"].as_str().unwrap() {
                            "text" => { assert_eq!(value["lang"], language); texts += 1; },
                            "calltrace" => {
                                assert_eq!(value["code"], "quality.voicemail_detected");
                                assert_eq!(value["detail"]["method"], if tone { "beep" } else { "asr" });
                                traces += 1;
                            },
                            "hangup" => { assert_eq!(value["reason"], "quality.voicemail_detected"); break; },
                            _ => panic!("unexpected event"),
                        }
                    }
                }
            }).await.unwrap();
            assert_eq!(traces, 1);
            if !tone { assert!(texts > 0); }
            assert!(tokio::time::timeout(Duration::from_millis(100), silent_reader.next()).await.is_err(), "another connection must not receive speech results");
            send.abort();
            let _ = send.await;
        }
        server.abort();
    }
}
