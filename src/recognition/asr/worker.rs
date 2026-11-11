//! Each language worker owns one recognizer and batches ready streams from different connections.
use std::{collections::{BTreeMap, HashMap, VecDeque}, sync::{Arc, mpsc, atomic::{AtomicBool, AtomicU64, Ordering}}, time::{Duration, Instant}};
use bytes::Bytes;
use crossbeam_utils::sync::{Parker, Unparker};
use anyhow::{Context, Result, ensure};
use tokio::sync::mpsc as async_mpsc;
use rtrb::{RingBuffer, Producer, Consumer};
use crate::config::RecognitionModelConfig;
use crate::recognition::InspectionParameters;
use super::{load_recognizer, RecognitionStream};
use crate::recognition::{RecognitionEvent, RECOGNITION_CHUNK_SAMPLES};

const MAX_PENDING_CHUNKS: usize = 5;

struct Registration {
    stream_id: u64,
    inspect_ms: u32,
    audio: Consumer<Bytes>,
    events: async_mpsc::Sender<RecognitionEvent>,
}
struct AudioSender {
    audio: Producer<Bytes>,
    notify: Unparker,
}
struct StreamState {
    recognition: RecognitionStream,
    audio: Consumer<Bytes>,
}
struct RecognitionWorkerHandle {
    registrations: mpsc::SyncSender<Registration>,
    stopping: Arc<AtomicBool>,
    notify: Unparker,
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    #[cfg(test)]
    largest_batch: Arc<std::sync::atomic::AtomicUsize>,
}
struct RecognitionWorker {
    recognizer: sherpa_onnx::OnlineRecognizer,
    language: String,
    max_batch_size: usize,
    max_wait: Duration,
    registration_receiver: mpsc::Receiver<Registration>,
    parker: Parker,
    stopping: Arc<AtomicBool>,
    #[cfg(test)]
    batch_metric: Arc<std::sync::atomic::AtomicUsize>,
}
impl RecognitionWorker {
    fn run(self) {
        let Self { recognizer, language, max_batch_size, max_wait, registration_receiver, parker, stopping,
            #[cfg(test)] batch_metric,
        } = self;
        let mut streams: HashMap<u64, StreamState> = HashMap::new();
        let mut ready = VecDeque::new();
        let mut deadline = None;
        while !stopping.load(Ordering::Relaxed) {
            // Registration has its own bounded control queue. Audio never passes through it.
            for _ in 0..64 {
                match registration_receiver.try_recv() {
                    Ok(Registration { stream_id, inspect_ms, audio, events }) => {
                        if !events.is_closed() {
                            streams.insert(stream_id, StreamState {
                                recognition: RecognitionStream::new(&recognizer, &language, inspect_ms, events),
                                audio,
                            });
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            }
            // Receiver closure cancels queued work without relying on a Close command.
            streams.retain(|_, state| !state.recognition.is_finished(&recognizer) && !state.audio.is_abandoned());
            for state in streams.values_mut() {
                // Each stream gets a bounded turn even if its producer continuously refills.
                for _ in 0..MAX_PENDING_CHUNKS {
                    if recognizer.is_ready(&state.recognition.decoder_stream) { break; }
                    let Ok(audio) = state.audio.pop() else { break; };
                    state.recognition.accept_audio(bytemuck::cast_slice(&audio));
                }
            }
            ready.retain(|stream_id| streams.contains_key(stream_id));
            for (&stream_id, state) in &mut streams {
                let recognition = &mut state.recognition;
                if !recognition.decode_queued && recognizer.is_ready(&recognition.decoder_stream) {
                    recognition.decode_queued = true;
                    ready.push_back(stream_id);
                }
            }
            if ready.is_empty() { deadline = None; }
            else if deadline.is_none() { deadline = Some(Instant::now() + max_wait); }
            let decode = !ready.is_empty() && (ready.len() >= max_batch_size || deadline.is_some_and(|d| Instant::now() >= d));
            if decode {
                let ids: Vec<_> = ready.drain(..ready.len().min(max_batch_size)).collect();
                let batch_streams: Vec<_> = ids.iter().map(|stream_id| &streams[stream_id].recognition.decoder_stream).collect();
                #[cfg(test)]
                batch_metric.fetch_max(batch_streams.len(), Ordering::Relaxed);
                recognizer.decode_multiple_streams(&batch_streams);
                // No recognition stream is mutated while its batch is executing on this language thread.
                for stream_id in ids {
                    let recognition = &mut streams.get_mut(&stream_id).unwrap().recognition;
                    recognition.decode_queued = false;
                    match recognition.publish_result(&recognizer) {
                        Ok(true) => {},
                        Ok(false) => { streams.remove(&stream_id); },
                        Err(_) => { streams.remove(&stream_id); },
                    }
                }
                // Already waiting streams keep their original deadline; no starvation by requeueing.
                if ready.is_empty() { deadline = None; }
            }
            if decode { continue; }
            let wait = deadline.map(|d| d.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_millis(100));
            // unpark retains a token if audio/register/close arrives before this park.
            // Notifications coalesce; they carry no audio and cannot fill another queue.
            parker.park_timeout(wait);
        }
    }
}
pub(crate) struct RecognitionWorkers {
    workers_by_language: BTreeMap<String, RecognitionWorkerHandle>,
    next_stream_id: AtomicU64,
}
pub(crate) struct RecognitionHandle {
    audio_senders: Vec<AudioSender>,
    pub events: async_mpsc::Receiver<RecognitionEvent>,
}
impl RecognitionHandle {
    pub fn send_audio(&mut self, pcm: Bytes) -> Result<()> {
        if pcm.is_empty() { return Ok(()); }
        let samples = bytemuck::try_cast_slice::<u8, f32>(&pcm).map_err(|_| anyhow::anyhow!("invalid aligned f32 audio chunk"))?;
        ensure!(samples.len() == RECOGNITION_CHUNK_SAMPLES, "expected a 100 ms ASR audio chunk");
        // A full ring drops this new chunk only. Never overwrite queued audio or block.
        for sender in &mut self.audio_senders {
            if !sender.audio.is_abandoned() {
                let _ = sender.audio.push(pcm.clone());
                sender.notify.unpark();
            }
        }
        Ok(())
    }
}
impl Drop for RecognitionHandle {
    fn drop(&mut self) {
        self.events.close();
        for sender in &self.audio_senders { sender.notify.unpark(); }
    }
}
impl RecognitionWorkers {
    pub fn start(configs: &BTreeMap<String, RecognitionModelConfig>) -> Result<Self> {
        let mut workers_by_language = BTreeMap::new();
        for (language, config) in configs {
            let recognizer = load_recognizer(language, config)
                .with_context(|| format!("prepare ASR model for {language}"))?;
            tracing::info!(language, num_threads = config.num_threads,
                max_batch_size = config.max_batch_size, "ASR model loaded");
            let key = language.clone();
            let language = language.clone();
            let max_batch_size = config.max_batch_size;
            let max_wait = Duration::from_millis(config.max_wait_ms);
            let (registrations, registration_receiver) = mpsc::sync_channel(64);
            #[cfg(test)]
            let largest_batch = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            #[cfg(test)]
            let batch_metric = largest_batch.clone();
            let stopping = Arc::new(AtomicBool::new(false));
            let parker = Parker::new();
            let notify = parker.unparker().clone();
            let worker = RecognitionWorker { recognizer, language: language.clone(), max_batch_size, max_wait,
                registration_receiver, parker, stopping: stopping.clone(),
                #[cfg(test)] batch_metric,
            };
            let thread = std::thread::Builder::new().name(format!("quality-asr-{language}"))
                .spawn(move || worker.run())?;
            workers_by_language.insert(key, RecognitionWorkerHandle { registrations, stopping, notify, thread: std::sync::Mutex::new(Some(thread)),
                #[cfg(test)] largest_batch,
            });
        }
        Ok(Self { workers_by_language, next_stream_id: AtomicU64::new(1) })
    }
    pub async fn shutdown(&self) -> Result<()> {
        let mut threads = Vec::new();
        for worker in self.workers_by_language.values() {
            worker.stopping.store(true, Ordering::Relaxed);
            worker.notify.unpark();
            if let Some(thread) = worker.thread.lock().unwrap().take() { threads.push(thread); }
        }
        tokio::task::spawn_blocking(move || {
            let mut panicked = false;
            for thread in threads { panicked |= thread.join().is_err(); }
            ensure!(!panicked, "ASR worker panicked");
            Ok(())
        }).await?
    }
    pub fn validate_languages(&self, config: &InspectionParameters) -> Result<()> {
        if config.asr_enabled {
            ensure!(config.asr_languages.iter().all(|l| self.workers_by_language.contains_key(l)), "requested ASR language has no loaded model");
        }
        Ok(())
    }
    pub fn register(&self, config: InspectionParameters) -> Result<Option<RecognitionHandle>> {
        self.validate_languages(&config)?;
        let stream_id = self.next_stream_id.fetch_add(1, Ordering::Relaxed);
        let (events, event_receiver) = async_mpsc::channel(32);
        let mut handle = RecognitionHandle { audio_senders: Vec::new(), events: event_receiver };
        for language in &config.asr_languages {
            let worker = &self.workers_by_language[language];
            let (audio, audio_receiver) = RingBuffer::new(MAX_PENDING_CHUNKS);
            match worker.registrations.try_send(Registration { stream_id, inspect_ms: config.inspect_ms,
                audio: audio_receiver, events: events.clone() }) {
                Ok(()) => {},
                // Admission is best effort. Drop this registration and any earlier language registrations.
                Err(mpsc::TrySendError::Full(_)) => return Ok(None),
                Err(mpsc::TrySendError::Disconnected(_)) => anyhow::bail!("ASR worker stopped"),
            }
            worker.notify.unpark();
            handle.audio_senders.push(AudioSender { audio, notify: worker.notify.clone() });
        }
        drop(events);
        Ok(Some(handle))
    }
}

impl Drop for RecognitionWorkerHandle {
    fn drop(&mut self) {
        // Also stop already-started workers when later model initialization fails.
        self.stopping.store(true, Ordering::Relaxed);
        self.notify.unpark();
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::recognition::RecognitionResult;

    #[test]
    fn full_ring_drops_new_audio_and_resumes_without_affecting_other_languages() {
        let (audio_a, mut receiver_a) = RingBuffer::new(MAX_PENDING_CHUNKS);
        let (audio_b, mut receiver_b) = RingBuffer::new(MAX_PENDING_CHUNKS);
        let (_events, results) = async_mpsc::channel(1);
        let mut connection = RecognitionHandle { audio_senders: vec![
            AudioSender { audio: audio_a, notify: Parker::new().unparker().clone() },
            AudioSender { audio: audio_b, notify: Parker::new().unparker().clone() },
        ], events: results };
        let mut frames = crate::service::audio::AudioProcessor::new(16000).unwrap();
        let mut input = &[0u8; 3200][..];
        while !input.is_empty() { frames.next_frame(&mut input).unwrap().unwrap(); }
        let chunk = frames.take_recognition_chunk().unwrap();
        assert!(connection.send_audio(chunk.slice(..chunk.len() - 4)).is_err());
        assert!(receiver_a.pop().is_err());
        for _ in 0..MAX_PENDING_CHUNKS {
            connection.send_audio(chunk.clone()).unwrap();
            receiver_b.pop().unwrap();
        }
        let newest = Bytes::copy_from_slice(&chunk);
        connection.send_audio(newest.clone()).unwrap();
        assert_eq!(receiver_b.pop().unwrap().as_ptr(), newest.as_ptr(), "another language still receives audio");
        for _ in 0..MAX_PENDING_CHUNKS {
            assert_eq!(receiver_a.pop().unwrap().as_ptr(), chunk.as_ptr(), "queued audio is not overwritten or copied");
        }
        assert!(receiver_a.pop().is_err(), "newest chunk was discarded when full");
        connection.send_audio(newest.clone()).unwrap();
        assert_eq!(receiver_a.pop().unwrap().as_ptr(), newest.as_ptr(), "audio resumes after capacity is available");
        drop(receiver_a);
        connection.send_audio(chunk).unwrap();
        drop(connection);
        assert!(_events.is_closed());
        assert!(receiver_b.is_abandoned());
    }

    #[test]
    fn registration_is_immediate_and_full_admission_rolls_back_other_languages() {
        let mut workers_by_language = BTreeMap::new();
        let mut receivers = BTreeMap::new();
        for language in ["en", "id"] {
            let (registrations, receiver) = mpsc::sync_channel(1);
            workers_by_language.insert(language.into(), RecognitionWorkerHandle {
                registrations, stopping: Arc::new(AtomicBool::new(false)), notify: Parker::new().unparker().clone(),
                thread: std::sync::Mutex::new(None), largest_batch: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            });
            receivers.insert(language, receiver);
        }
        let workers = RecognitionWorkers { workers_by_language, next_stream_id: AtomicU64::new(1) };
        let first = workers.register(InspectionParameters { asr_enabled:true, asr_languages:vec!["id".into()], ..Default::default() }).unwrap().unwrap();
        // No worker runs here: successful registration cannot depend on an acknowledgment.
        let second = workers.register(InspectionParameters { asr_enabled:true, asr_languages:vec!["en".into(), "id".into()], ..Default::default() }).unwrap();
        assert!(second.is_none(), "full registration queue skips this connection without error");
        let partial = receivers["en"].try_recv().unwrap();
        assert!(partial.events.is_closed(), "already-queued languages are cancelled on partial admission failure");
        assert!(partial.audio.is_abandoned());
        let registration = receivers["id"].try_recv().unwrap();
        assert!(!registration.events.is_closed());
        drop(first);
        assert!(registration.events.is_closed(), "abandoned registration is visible before stream creation");
        assert!(registration.audio.is_abandoned());
    }

    #[tokio::test]
    #[ignore = "requires QUALITY_TEST_CONFIG with local model assets"]
    async fn batches_distinct_connections_and_cancels_independently() {
        let path = std::env::var("QUALITY_TEST_CONFIG").unwrap();
        let config: crate::config::ServiceConfig = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let worker = RecognitionWorkers::start(&config.models).unwrap();
        let parameters = InspectionParameters { asr_enabled:true,
            asr_languages:config.models.keys().cloned().collect(), ..Default::default() };
        let mut a = worker.register(parameters.clone()).unwrap().unwrap();
        let mut b = worker.register(parameters.clone()).unwrap().unwrap();
        let pcm = Bytes::from(vec![0u8; 1600 * 4]);
        for _ in 0..20 {
            a.send_audio(pcm.clone()).unwrap();
            b.send_audio(pcm.clone()).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        for (language, handle) in &worker.workers_by_language {
            assert!(handle.largest_batch.load(Ordering::Relaxed) >= 2, "{language} never batched different streams");
        }
        for events in [&mut a.events, &mut b.events] {
            while let Ok(update) = events.try_recv() {
                assert!(matches!(update, RecognitionEvent::Result(RecognitionResult { ref text, .. }) if text.is_empty()));
            }
        }
        a.events.close();
        for sender in &a.audio_senders { sender.notify.unpark(); }
        tokio::time::timeout(Duration::from_secs(2), async {
            while a.audio_senders.iter().any(|sender| !sender.audio.is_abandoned()) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("closed result receiver releases every language stream and its audio buffer");
        drop(a);
        for _ in 0..8 {
            b.send_audio(pcm.clone()).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(b.audio_senders.iter().all(|sender| !sender.audio.is_abandoned()));
        while let Ok(update) = b.events.try_recv() {
            assert!(matches!(update, RecognitionEvent::Result(RecognitionResult { ref text, .. }) if text.is_empty()));
        }
        b.events.close();
        tokio::time::timeout(Duration::from_secs(2), async {
            while b.audio_senders.iter().any(|sender| !sender.audio.is_abandoned()) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("idle workers reclaim closed streams without more audio or explicit wakeup");
        drop(b);
        let mut remaining = worker.register(parameters).unwrap().unwrap();
        worker.shutdown().await.unwrap();
        assert!(remaining.events.recv().await.is_none());
        assert!(remaining.audio_senders.iter().all(|sender| sender.audio.is_abandoned()));
        assert!(worker.workers_by_language.values().all(|handle| handle.thread.lock().unwrap().is_none()));
    }
}
