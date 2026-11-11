//! Lightweight incremental frequency detection. No tasks, sockets or model state.
use validator::Validate;
use super::BeepDetection;
use crate::recognition::InspectionParameters;

pub(crate) struct BeepDetector {
    config: InspectionParameters,
    sample_rate: u32,
    frame_buffer: Vec<i16>,
    processed_samples: u64,
    active_frequency_index: Option<usize>,
    tone_ms: u32,
    gap_ms: u32,
    start_ms: u64,
    detection_reported: bool,
}
impl BeepDetector {
    pub fn new(config: InspectionParameters, sample_rate: u32) -> anyhow::Result<Self> {
        config.validate()?;
        anyhow::ensure!((8000..=48000).contains(&sample_rate), "unsupported sample rate");
        anyhow::ensure!(config.frequencies_hz.iter().all(|f| *f + config.tolerance_hz < sample_rate as f64 / 2.0), "frequency outside detector Nyquist range");
        Ok(Self { config, sample_rate, frame_buffer: Vec::new(), processed_samples: 0, active_frequency_index: None,
            tone_ms: 0, gap_ms: 0, start_ms: 0, detection_reported: false })
    }
    pub fn push(&mut self, pcm: &[i16]) -> Vec<BeepDetection> {
        let mut results = Vec::new();
        let window = self.sample_rate as usize / 50;
        for &sample in pcm {
            if self.processed_samples * 1000 / self.sample_rate as u64 >= self.config.inspect_ms as u64 { break; }
            self.frame_buffer.push(sample);
            self.processed_samples += 1;
            if self.frame_buffer.len() < window { continue; }
            let end_ms = self.processed_samples * 1000 / self.sample_rate as u64;
            let mean = self.frame_buffer.iter().map(|s| *s as f64).sum::<f64>() / window as f64;
            let energy = self.frame_buffer.iter().map(|s| (*s as f64 - mean).powi(2)).sum::<f64>();
            let dbfs = 10.0 * (energy / window as f64 / 32768.0_f64.powi(2)).log10();
            let mut best = None;
            let mut ratio = 0.0;
            if dbfs >= self.config.min_dbfs && energy > 0.0 {
                for (index, &frequency) in self.config.frequencies_hz.iter().enumerate() {
                    let steps = (self.config.tolerance_hz * 2.0 / 5.0).ceil() as usize;
                    for step in 0..=steps {
                        let hz = if steps == 0 { frequency } else {
                            frequency - self.config.tolerance_hz + step as f64 * 2.0 * self.config.tolerance_hz / steps as f64
                        };
                        let coeff = 2.0 * (std::f64::consts::TAU * hz / self.sample_rate as f64).cos();
                        let (mut a, mut b) = (0.0, 0.0);
                        for sample in &self.frame_buffer {
                            let next = *sample as f64 - mean + coeff * a - b;
                            b = a; a = next;
                        }
                        let power = a*a + b*b - coeff*a*b;
                        let candidate = (2.0 * power / (window as f64 * energy)).min(1.0);
                        if candidate >= self.config.min_energy_ratio && candidate > ratio {
                            ratio = candidate; best = Some(index);
                        }
                    }
                }
            }
            self.frame_buffer.clear();
            if let Some(index) = best {
                if self.active_frequency_index != Some(index) {
                    self.active_frequency_index = Some(index); self.tone_ms = 0;
                    self.start_ms = end_ms - 20; self.detection_reported = false;
                }
                self.gap_ms = 0;
                self.tone_ms += 20;
                if !self.detection_reported && self.tone_ms >= self.config.min_tone_ms {
                    results.push(BeepDetection { frequency_hz: self.config.frequencies_hz[index],
                        start_ms: self.start_ms, end_ms, tone_ms: self.tone_ms, energy_ratio: ratio });
                    self.detection_reported = true;
                }
            } else {
                self.gap_ms += 20;
                if self.gap_ms > self.config.max_gap_ms {
                    self.active_frequency_index = None; self.tone_ms = 0; self.detection_reported = false;
                    self.gap_ms = 0;
                }
            }
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tone(hz: f64, ms: usize) -> Vec<i16> {
        (0..ms*16).map(|i| (12000.0 * (std::f64::consts::TAU * hz * i as f64 / 16000.0).sin()) as i16).collect()
    }
    #[test]
    fn chunk_boundaries_and_zero_gaps() {
        for hz in [1309.0, 660.0] {
            let mut audio = tone(hz, 100);
            audio.extend(vec![0; 320]); audio.extend(tone(hz, 100));
            let mut detector = BeepDetector::new(InspectionParameters::default(), 16000).unwrap();
            let hits: Vec<_> = audio.chunks(137).flat_map(|chunk| detector.push(chunk)).collect();
            assert_eq!(hits.len(), 1); assert_eq!(hits[0].end_ms, 180);
            let mut strict = BeepDetector::new(InspectionParameters { max_gap_ms: 0, ..Default::default() }, 16000).unwrap();
            assert!(strict.push(&audio).is_empty());
        }
    }
    #[test]
    fn rejects_wrong_frequency_short_tones_and_silence() {
        for audio in [tone(425.0, 500), tone(1309.0, 100), vec![0; 16000]] {
            assert!(BeepDetector::new(InspectionParameters::default(), 16000).unwrap().push(&audio).is_empty());
        }
    }
    #[test]
    fn bounded_inspection_and_validation() {
        let mut detector = BeepDetector::new(InspectionParameters { inspect_ms: 100, ..Default::default() }, 16000).unwrap();
        assert!(detector.push(&tone(1309.0, 500)).is_empty());
        assert!(BeepDetector::new(InspectionParameters { frequencies_hz: vec![9000.0], ..Default::default() }, 16000).is_err());
    }
}

#[cfg(test)]
mod interference_tests {
    use super::*;
    #[test]
    fn mixed_tones_do_not_pass_single_tone_purity() {
        let processed_samples: Vec<i16> = (0..16000).map(|i| {
            let t = std::f64::consts::TAU * i as f64 / 16000.0;
            (7000.0 * ((1309.0 * t).sin() + (1000.0 * t).sin())) as i16
        }).collect();
        assert!(BeepDetector::new(InspectionParameters::default(), 16000).unwrap().push(&processed_samples).is_empty());
    }
}
