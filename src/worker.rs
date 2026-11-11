//! Each language worker owns one recognizer and batches ready streams from different connections.
use std::{collections::{BTreeMap, HashMap, VecDeque}, sync::{Arc, mpsc, atomic::{AtomicBool, AtomicU64, Ordering}}, time::{Duration, Instant}};
use bytes::Bytes;
use anyhow::{Context, Result, ensure};
use tokio::sync::{mpsc as async_mpsc, oneshot};
use crate::{asr::{self, Session, Output}, config::{ModelConfig, Parameters}};

const MAX_PENDING_CHUNKS: usize = 2;

enum Command {
    Open { id: u64, config: Parameters, output: async_mpsc::Sender<Output>, reply: oneshot::Sender<()>, failed: Arc<AtomicBool> },
    Audio { id: u64, pcm: Bytes },
}
struct ConnectionState {
    session: Session,
    pending: VecDeque<Bytes>,
    failed: Arc<AtomicBool>,
}
struct LanguageWorker {
    commands: mpsc::SyncSender<Command>,
    #[cfg(test)]
    largest_batch: Arc<std::sync::atomic::AtomicUsize>,
}
pub(crate) struct RecognitionWorkers {
    languages: BTreeMap<String, LanguageWorker>,
    next_id: AtomicU64,
}
pub(crate) struct RecognitionConnection {
    id: u64,
    languages: Vec<mpsc::SyncSender<Command>>,
    failed: Arc<AtomicBool>,
    pub output: async_mpsc::Receiver<Output>,
}
impl RecognitionConnection {
    pub fn failed(&self) -> bool { self.failed.load(Ordering::Relaxed) }
    pub fn push(&self, pcm: Bytes) -> Result<()> {
        if pcm.is_empty() { return Ok(()); }
        let samples = bytemuck::try_cast_slice::<u8, f32>(&pcm).map_err(|_| anyhow::anyhow!("invalid aligned f32 audio chunk"))?;
        ensure!(samples.len() == crate::audio::ASR_CHUNK_SAMPLES, "expected a 100 ms ASR audio chunk");
        ensure!(!self.failed(), "ASR pending queue full");
        for commands in &self.languages {
            commands.try_send(Command::Audio { id: self.id, pcm: pcm.clone() })
                .map_err(|_| anyhow::anyhow!("ASR worker queue full or closed"))?;
        }
        Ok(())
    }
}
impl RecognitionWorkers {
    pub fn load(configs: &BTreeMap<String, ModelConfig>) -> Result<Self> {
        let mut languages = BTreeMap::new();
        for (language, config) in configs {
            let model = asr::load(language, config)
                .with_context(|| format!("prepare ASR model for {language}"))?;
            tracing::info!(language, num_threads = config.num_threads,
                max_batch_size = config.max_batch_size, "ASR model loaded");
            let key = language.clone();
            let language = language.clone();
            let max_batch_size = config.max_batch_size;
            let max_wait = Duration::from_millis(config.max_wait_ms);
            let (commands, rx) = mpsc::sync_channel(64);
            #[cfg(test)]
            let largest_batch = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            #[cfg(test)]
            let batch_metric = largest_batch.clone();
            std::thread::Builder::new().name(format!("quality-asr-{language}")).spawn(move || {
                let mut sessions: HashMap<u64, ConnectionState> = HashMap::new();
                let mut ready = VecDeque::new();
                let mut deadline = None;
                loop {
                    // Receiver closure cancels queued work without relying on a Close command.
                    sessions.retain(|_, state| !state.failed.load(Ordering::Relaxed) && !state.session.finished(&model));
                    for state in sessions.values_mut() {
                        // Feed only until the model is ready; the remaining queue has a fixed chunk limit.
                        while !model.is_ready(&state.session.stream) {
                            let Some(audio) = state.pending.pop_front() else { break; };
                            state.session.accept(bytemuck::cast_slice(&audio));
                        }
                    }
                    ready.retain(|id| sessions.contains_key(id));
                    for (&id, state) in &mut sessions {
                        let session = &mut state.session;
                        if !session.queued && model.is_ready(&session.stream) {
                            session.queued = true;
                            ready.push_back(id);
                        }
                    }
                    if ready.is_empty() { deadline = None; }
                    else if deadline.is_none() { deadline = Some(Instant::now() + max_wait); }
                    let decode = !ready.is_empty() && (ready.len() >= max_batch_size || deadline.is_some_and(|d| Instant::now() >= d));
                    if decode {
                        let ids: Vec<_> = ready.drain(..ready.len().min(max_batch_size)).collect();
                        let streams: Vec<_> = ids.iter().map(|id| &sessions[id].session.stream).collect();
                        #[cfg(test)]
                        batch_metric.fetch_max(streams.len(), Ordering::Relaxed);
                        model.decode_multiple_streams(&streams);
                        // No stream is mutated while its batch is executing on this language thread.
                        for id in ids {
                            let session = &mut sessions.get_mut(&id).unwrap().session;
                            session.queued = false;
                            if !session.publish(&model) { sessions.remove(&id); }
                        }
                        // Already waiting streams keep their original deadline; no starvation by requeueing.
                        if ready.is_empty() { deadline = None; }
                    }
                    let wait = if decode { Duration::ZERO } else { deadline.map(|d| d.saturating_duration_since(Instant::now()))
                        .unwrap_or(Duration::from_millis(100)) };
                    let first = match rx.recv_timeout(wait) {
                        Ok(command) => command,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    // Bound ingress draining so continuous audio cannot starve batch execution.
                    let mut command = first;
                    for index in 0..64 {
                        match command {
                            Command::Open { id, config, output, reply, failed } => {
                                if !output.is_closed() && !reply.is_closed() {
                                    sessions.insert(id, ConnectionState { session: Session::new(&model, &language, config, output), pending: VecDeque::with_capacity(MAX_PENDING_CHUNKS), failed });
                                    let _ = reply.send(());
                                }
                            }
                            Command::Audio { id, pcm } => {
                                if let Some(state) = sessions.get_mut(&id).filter(|s| !s.session.is_closed()) {
                                    if state.pending.len() == MAX_PENDING_CHUNKS {
                                        state.failed.store(true, Ordering::Relaxed);
                                        state.session.report_queue_full();
                                        sessions.remove(&id);
                                    } else {
                                        state.pending.push_back(pcm);
                                    }
                                }
                            }
                        }
                        if index == 63 { break; }
                        match rx.try_recv() { Ok(next) => command = next, Err(_) => break }
                    }
                }
            })?;
            languages.insert(key, LanguageWorker { commands,
                #[cfg(test)] largest_batch,
            });
        }
        Ok(Self { languages, next_id: AtomicU64::new(1) })
    }
    pub fn validate(&self, config: &Parameters) -> Result<()> {
        if config.asr_enabled {
            ensure!(config.asr_languages.iter().all(|l| self.languages.contains_key(l)), "requested ASR language has no loaded model");
        }
        Ok(())
    }
    pub async fn open(&self, config: Parameters) -> Result<RecognitionConnection> {
        self.validate(&config)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (output, receiver) = async_mpsc::channel(32);
        let mut languages = Vec::new();
        let failed = Arc::new(AtomicBool::new(false));
        let mut replies = Vec::new();
        for language in &config.asr_languages {
            let commands = self.languages[language].commands.clone();
            let (reply, ready) = oneshot::channel();
            commands.try_send(Command::Open { id, config: config.clone(), output: output.clone(), reply, failed: failed.clone() })
                .map_err(|_| anyhow::anyhow!("ASR worker queue full or closed"))?;
            languages.push(commands);
            replies.push(ready);
        }
        drop(output);
        tokio::time::timeout(Duration::from_secs(3), futures::future::try_join_all(replies)).await??;
        Ok(RecognitionConnection { id, languages, failed, output: receiver })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ingress_queue_is_bounded_and_rejects_incomplete_chunks() {
        let (commands, receiver) = mpsc::sync_channel(1);
        let (_output, results) = async_mpsc::channel(1);
        let connection = RecognitionConnection { id: 1, languages: vec![commands],
            failed: Arc::new(AtomicBool::new(false)), output: results };
        let mut frames = crate::audio::PcmFrames::new(16000).unwrap();
        let mut input = &[0u8; 3200][..];
        while !input.is_empty() { frames.next_frame(&mut input).unwrap().unwrap(); }
        let chunk = frames.take_chunk().unwrap();
        assert!(connection.push(chunk.slice(..chunk.len() - 4)).is_err());
        assert!(receiver.try_recv().is_err());
        connection.push(chunk.clone()).unwrap();
        assert!(connection.push(chunk.clone()).is_err());
        assert!(matches!(receiver.try_recv().unwrap(), Command::Audio { id: 1, .. }));
        connection.push(chunk.clone()).unwrap();
        receiver.try_recv().unwrap();
        connection.failed.store(true, Ordering::Relaxed);
        assert!(connection.push(chunk).is_err());
        assert!(receiver.try_recv().is_err());
    }
    #[tokio::test]
    #[ignore = "requires QUALITY_TEST_CONFIG with local model assets"]
    async fn batches_distinct_connections_and_cancels_independently() {
        let path = std::env::var("QUALITY_TEST_CONFIG").unwrap();
        let config: crate::config::Config = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let worker = RecognitionWorkers::load(&config.models).unwrap();
        let parameters = Parameters { asr_enabled: true,
            asr_languages: config.models.keys().cloned().collect(), ..Default::default() };
        let mut a = worker.open(parameters.clone()).await.unwrap();
        let mut b = worker.open(parameters).await.unwrap();
        let pcm = Bytes::from(vec![0u8; 1600 * 4]);
        for _ in 0..20 {
            a.push(pcm.clone()).unwrap();
            b.push(pcm.clone()).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        for (language, model) in &worker.languages {
            assert!(model.largest_batch.load(Ordering::Relaxed) >= 2, "{language} never batched different streams");
        }
        assert!(a.output.try_recv().is_err());
        assert!(b.output.try_recv().is_err());
        // Fixed chunks reject malformed input before dispatch.
        assert!(a.push(Bytes::from(vec![0u8; 3201 * 4])).is_err());
        drop(a);
        for _ in 0..8 {
            b.push(pcm.clone()).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(b.output.try_recv().is_err());
        drop(b);
    }
}
