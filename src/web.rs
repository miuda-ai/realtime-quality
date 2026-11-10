//! HTTP upgrade and a single async select loop per WebSocket connection.
use std::{sync::Arc, time::Duration};
use anyhow::Result;
use dasp_sample::Sample;
use axum::{Router, extract::{State, WebSocketUpgrade, ws::{Message, WebSocket}}, http::{HeaderMap, StatusCode}, response::IntoResponse, routing::get};
use futures::SinkExt;
use crate::{QualityService, asr, audio::PcmFrames, beep::Detector, config::Parameters,
    protocol::{Start, QualityResponse, VoicemailEvidence}};

pub(crate) fn router(service: Arc<QualityService>) -> Router {
    Router::new().route("/health", get(|| async { "ok" }))
        .route("/v1/quality", get(upgrade)).with_state(service)
}

async fn upgrade(State(service): State<Arc<QualityService>>, headers: HeaderMap, ws: WebSocketUpgrade) -> impl IntoResponse {
    if let Some(token) = &service.config.token {
        if headers.get("authorization").and_then(|h| h.to_str().ok()) != Some(format!("Bearer {token}").as_str()) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }
    let Ok(permit) = service.connections.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    ws.max_message_size(65536).max_frame_size(65536).on_upgrade(move |socket| async move {
        let _permit = permit;
        if let Err(error) = inspect(socket, service).await { eprintln!("inspection ended: {error}"); }
    }).into_response()
}
async fn inspect(mut socket: WebSocket, service: Arc<QualityService>) -> Result<()> {
    let Some(Ok(Message::Text(start))) = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await? else { anyhow::bail!("expected start"); };
    let start: Start = serde_json::from_str(&start)?;
    anyhow::ensure!(start.kind == "start" && start.version == 1, "unsupported protocol version");
    anyhow::ensure!([start.tenant_id, start.inbound_trunk_id, start.outbound_trunk_id]
        .into_iter().flatten().all(|id| id > 0), "routing IDs must be positive");
    anyhow::ensure!(matches!(start.direction.as_str(), "inbound" | "outbound" | "internal"), "invalid call direction");
    anyhow::ensure!(matches!(start.side.as_str(), "caller" | "callee") && !start.call_id.is_empty() && !start.session_id.is_empty(), "invalid stream identity");
    let mut audio = PcmFrames::new(start.sample_rate)?;
    let mut parameters = serde_json::to_value(&service.config.detector)?;
    if let Some(overrides) = start.parameters.as_object() {
        parameters.as_object_mut().unwrap().extend(overrides.clone());
    } else { anyhow::ensure!(start.parameters.is_null(), "parameters must be an object"); }
    let parameters: Parameters = serde_json::from_value(parameters)?;
    parameters.validate(16000)?;
    if let Some(worker) = &service.worker { worker.validate(&parameters)?; }
    anyhow::ensure!(parameters.asr_enabled || parameters.beep_enabled, "no detector enabled");
    let mut detector = if parameters.beep_enabled { Some(Detector::new(parameters.clone(), 16000)?) } else { None };
    let mut asr = if parameters.asr_enabled { Some(service.worker.as_ref().ok_or_else(|| anyhow::anyhow!("no ASR models loaded"))?.open(parameters.clone()).await?) } else { None };
    let mut observed = false;
    let mut hangup_sent = false;
    let deadline = tokio::time::sleep(Duration::from_millis(parameters.inspect_ms as u64));
    tokio::pin!(deadline);
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(10), Duration::from_secs(10),
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    'connection: loop {
        tokio::select! {
            _ = &mut deadline => break,
            _ = heartbeat.tick() => {
                tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Ping(Vec::new().into()))).await??;
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Binary(data))) => {
                    if tokio::time::Instant::now() >= deadline.deadline() { break; }
                    let mut data = data.as_ref();
                    while let Some(samples) = audio.next_frame(&mut data)? {
                        if tokio::time::Instant::now() >= deadline.deadline() { break 'connection; }
                        // Only the existing beep detector needs integer samples. ASR stays f32.
                        let beep_samples = if detector.is_some() {
                            std::array::from_fn(|i| samples[i].clamp(-1.0, 32767.0 / 32768.0).to_sample::<i16>())
                        } else { [0; 320] };
                        // The PBX selects which legs are sent; inspect each received stream.
                        for hit in detector.as_mut().map(|d| d.push(&beep_samples)).unwrap_or_default() {
                            let detail = VoicemailEvidence::Beep { detection: hit };
                            if !observed {
                                observed = true;
                                tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Text(serde_json::to_string(&QualityResponse::Calltrace { kind: "quality", code: "quality.voicemail_detected", severity: "info", message: "Voicemail detected from configured beep", detail: Some(&detail) })?.into()))).await??;
                            }
                            if parameters.hangup_on_beep && !hangup_sent {
                                hangup_sent = true;
                                tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Text(serde_json::to_string(&QualityResponse::Hangup { reason: "quality.voicemail_detected", detail: &detail })?.into()))).await??;
                            }
                        }
                        if let Some(chunk) = audio.take_chunk() {
                            if let Some(engine) = &asr {
                                if let Err(error) = engine.push(chunk) {
                                    eprintln!("ASR disabled for {}: {error}", start.session_id);
                                    asr = None;
                                    tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Text(serde_json::to_string(&QualityResponse::Calltrace {
                                        kind: "quality", code: "quality.asr_failed", severity: "warn", message: "ASR queue full or closed", detail: None,
                                    })?.into()))).await??;
                                    if detector.is_none() { break 'connection; }
                                }
                            }
                        }
                    }
                }
                Some(Ok(Message::Ping(data))) => tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Pong(data))).await??,
                Some(Ok(Message::Close(_))) => {
                    // Axum/tungstenite queues the Close reply; flush before dropping TCP.
                    let _ = tokio::time::timeout(Duration::from_secs(1), socket.flush()).await;
                    break;
                },
                None => break,
                Some(Err(error)) => return Err(error.into()),
                _ => {},
            },
            incoming = async { match asr.as_mut() { Some(engine) => engine.output.recv().await, None => std::future::pending().await } } => {
                if tokio::time::Instant::now() >= deadline.deadline() { break; }
                let incoming = if asr.as_ref().is_some_and(|engine| engine.failed()) {
                    Some(asr::Output::QueueFull)
                } else { incoming };
                match incoming {
                    Some(asr::Output::QueueFull) => {
                        asr = None;
                        tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Text(serde_json::to_string(&QualityResponse::Calltrace {
                            kind: "quality", code: "quality.asr_failed", severity: "warn", message: "ASR pending queue full", detail: None,
                        })?.into()))).await??;
                        if detector.is_none() { break; }
                    }
                    Some(asr::Output::Text { language, text, partial, start_ms, end_ms }) => {
                        tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Text(serde_json::to_string(&QualityResponse::Text {
                            text: &text, partial, start_ms, end_ms, lang: &language,
                        })?.into()))).await??;
                    }
                    Some(asr::Output::Match { language, phrase, text, end_ms }) => {
                        let detail = VoicemailEvidence::Asr { language, phrase, matched_text: text, end_ms };
                        if !observed {
                            observed = true;
                            tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Text(serde_json::to_string(&QualityResponse::Calltrace {
                                kind: "quality", code: "quality.voicemail_detected", severity: "info",
                                message: "Voicemail detected from configured ASR phrase", detail: Some(&detail),
                            })?.into()))).await??;
                        }
                        if parameters.hangup_on_asr && !hangup_sent {
                            hangup_sent = true;
                            tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Text(serde_json::to_string(&QualityResponse::Hangup {
                                reason: "quality.voicemail_detected", detail: &detail,
                            })?.into()))).await??;
                        }
                    }
                    None => {
                        asr = None;
                        if detector.is_none() { break; }
                    }
                }
            }
        }
    }
    drop(asr);
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        socket.close().await?;
        while let Some(message) = socket.recv().await { message?; }
        Ok::<_, axum::Error>(())
    }).await;
    Ok(())
}
