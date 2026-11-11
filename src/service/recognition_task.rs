//! One connection task owns transport, audio processing and detection policy.
use validator::Validate;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use anyhow::Result;
use axum::extract::ws::{Message, WebSocket};
use dasp_sample::Sample;
use futures::SinkExt;
use tokio::{sync::watch, time::{Instant, Interval}};
use crate::recognition::{InspectionParameters, AsrResultHandler, KeywordMatcher, RecognitionEvent, RecognitionResult, BeepDetector, RecognitionHandle, VoicemailEvidence};
use super::{QualityService, audio::AudioProcessor, StartMessage, QualityEvent};

pub(crate) struct RecognitionTask {
    socket: WebSocket,
    call_id: String,
    session_id: String,
    side: &'static str,
    audio: AudioProcessor,
    beep_detector: Option<BeepDetector>,
    recognition: Option<RecognitionHandle>,
    asr_result_handler: Box<dyn AsrResultHandler>,
    last_subtitles: BTreeMap<String, String>,
    parameters: InspectionParameters,
    hangup_sent: bool,
    inspection_deadline: Instant,
    heartbeat: Interval,
    shutdown: watch::Receiver<bool>,
}
impl RecognitionTask {
    pub async fn initialize(mut socket: WebSocket, service: &Arc<QualityService>) -> Result<Option<Self>> {
        let mut shutdown = service.shutdown.subscribe();
        let mut call_id = String::new();
        let mut session_id = String::new();
        let mut side = "unknown";
        let setup = async {
            let Some(Ok(Message::Text(start_message))) = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await? else {
                anyhow::bail!("expected start");
            };
            let start_message: StartMessage = serde_json::from_str(&start_message)?;
            call_id = start_message.call_id.clone();
            session_id = start_message.session_id.clone();
            side = start_message.side.as_str();
            start_message.validate()?;
            let parameters = start_message.parameters;
            if !parameters.asr_enabled && !parameters.beep_enabled { return Ok(None); }
            let audio = AudioProcessor::new(start_message.sample_rate)?;
            let inspection_deadline = Instant::now() + Duration::from_millis(parameters.inspect_ms as u64);
            let beep_detector = if parameters.beep_enabled { Some(BeepDetector::new(parameters.clone(), 16000)?) } else { None };
            let recognition = if parameters.asr_enabled {
                let handle = service.recognition_workers.as_ref()
                    .ok_or_else(|| anyhow::anyhow!("no ASR models loaded"))?.register(parameters.clone())?;
                let Some(handle) = handle else { return Ok(None); };
                Some(handle)
            } else { None };
            tracing::info!(%call_id, %session_id, side, sample_rate = start_message.sample_rate, asr_enabled = parameters.asr_enabled,
                beep_enabled = parameters.beep_enabled, "quality inspection started");
            Ok::<_, anyhow::Error>(Some((audio, parameters, beep_detector, recognition, inspection_deadline)))
        };
        let result = tokio::select! {
            biased;
            _ = shutdown.wait_for(|stopping| *stopping) => Ok(None),
            result = setup => result,
        };
        let (audio, parameters, beep_detector, recognition, inspection_deadline) = match result {
            Ok(Some(state)) => state,
            Ok(None) => { close(&mut socket).await; return Ok(None); }
            Err(error) => {
                tracing::warn!(%call_id, %session_id, side, error = %format!("{error:#}"), "quality connection setup failed");
                close(&mut socket).await;
                return Err(error);
            }
        };
        let mut heartbeat = tokio::time::interval_at(Instant::now() + Duration::from_secs(10), Duration::from_secs(10));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Ok(Some(Self { socket, call_id, session_id, side, audio, beep_detector, recognition, asr_result_handler: Box::new(KeywordMatcher::new(&parameters)), last_subtitles: BTreeMap::new(), parameters,
            hangup_sent: false, inspection_deadline, heartbeat, shutdown }))
    }
    pub async fn run(mut self) -> Result<()> {
        let deadline = self.inspection_deadline;
        let result = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => Ok(()),
            result = self.run_loop() => result,
        };
        // Dropping the receiver releases all language streams, even if their command queues are full.
        self.recognition.take();
        close(&mut self.socket).await;
        if let Err(error) = &result {
            tracing::warn!(call_id = %self.call_id, session_id = %self.session_id, side = self.side,
                error = %format!("{error:#}"), "quality connection ended with error");
        } else {
            tracing::debug!(call_id = %self.call_id, session_id = %self.session_id, side = self.side, "quality connection closed");
        }
        result
    }
    async fn run_loop(&mut self) -> Result<()> {
        loop {
            tokio::select! {
                _ = async { let _ = self.shutdown.wait_for(|stopping| *stopping).await; } => break,
                _ = self.heartbeat.tick() => {
                    tokio::time::timeout(Duration::from_secs(5), self.socket.send(Message::Ping(Vec::new().into()))).await??;
                },
                incoming = self.socket.recv() => match incoming {
                    Some(Ok(Message::Binary(data))) => self.receive_audio(&data).await?,
                    Some(Ok(Message::Ping(data))) => {
                        tokio::time::timeout(Duration::from_secs(5), self.socket.send(Message::Pong(data))).await??;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(error)) => return Err(error.into()),
                    _ => {},
                },
                incoming = async { match &mut self.recognition {
                    Some(handle) => handle.events.recv().await,
                    None => std::future::pending().await,
                }} => {
                    match incoming {
                        Some(RecognitionEvent::Result(result)) => self.receive_recognition_result(result).await?,
                        None => { self.recognition.take(); }
                    }
                }
            }
            if self.hangup_sent || (self.recognition.is_none() && self.beep_detector.is_none()) { break; }
        }
        Ok(())
    }
    async fn receive_audio(&mut self, mut data: &[u8]) -> Result<()> {
        while Instant::now() < self.inspection_deadline && !*self.shutdown.borrow() {
            let Some(samples) = self.audio.next_frame(&mut data)? else { break; };
            if let Some(beep_detector) = &mut self.beep_detector {
                let samples: [i16; 320] = std::array::from_fn(|i| samples[i].clamp(-1.0, 32767.0 / 32768.0).to_sample::<i16>());
                for detection in beep_detector.push(&samples) {
                    self.report_detection(VoicemailEvidence::Beep { detection }).await?;
                    if self.hangup_sent { return Ok(()); }
                }
            }
            if let Some(chunk) = self.audio.take_recognition_chunk() {
                if let Some(handle) = &mut self.recognition {
                    if let Err(error) = handle.send_audio(chunk) { self.disable_asr(&error.to_string()).await?; }
                }
            }
        }
        Ok(())
    }
    async fn receive_recognition_result(&mut self, result: RecognitionResult) -> Result<()> {
        let decision = self.asr_result_handler.handle(&result);
        if let Some(detection) = decision.detection {
            self.report_detection(detection).await?;
            if self.hangup_sent { return Ok(()); }
        }
        let previous = self.last_subtitles.entry(result.language.clone()).or_default();
        let emit_text = !result.text.is_empty() && (result.endpoint || result.text != *previous);
        if result.endpoint { previous.clear(); } else { previous.clone_from(&result.text); }
        if emit_text {
            self.send_event(&QualityEvent::Text { text: &result.text, partial: !result.endpoint,
                start_ms: result.start_ms, end_ms: result.end_ms, lang: &result.language }).await?;
        }
        Ok(())
    }
    async fn report_detection(&mut self, detail: VoicemailEvidence) -> Result<()> {
        let message = match &detail {
            VoicemailEvidence::Beep { .. } => "Voicemail detected from configured beep",
            VoicemailEvidence::Asr { .. } => "Voicemail detected from configured ASR phrase",
        };
        self.send_event(&QualityEvent::Calltrace { kind: "quality", code: "quality.voicemail_detected",
            severity: "info", message, detail: Some(&detail) }).await?;
        if self.parameters.hangup_on_detection && !self.hangup_sent {
            self.send_event(&QualityEvent::Hangup { reason: "quality.voicemail_detected", detail: &detail }).await?;
            self.hangup_sent = true;
        }
        Ok(())
    }
    async fn disable_asr(&mut self, message: &str) -> Result<()> {
        self.recognition.take();
        tracing::warn!(call_id = %self.call_id, session_id = %self.session_id, side = self.side, message, "ASR disabled for connection");
        self.send_event(&QualityEvent::Calltrace { kind: "quality", code: "quality.asr_failed",
            severity: "warn", message, detail: None }).await
    }
    async fn send_event(&mut self, response: &QualityEvent<'_>) -> Result<()> {
        let message = Message::Text(serde_json::to_string(response)?.into());
        tokio::time::timeout(Duration::from_secs(5), self.socket.send(message)).await??;
        Ok(())
    }
}

async fn close(socket: &mut WebSocket) {
    // Independent of shutdown: allow either peer's Close handshake to finish.
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        socket.close().await?;
        while let Some(message) = socket.recv().await { message?; }
        Ok::<_, axum::Error>(())
    }).await;
}
