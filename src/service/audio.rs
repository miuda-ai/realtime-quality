//! Per-connection PCM16LE decoding and streaming conversion to 16 kHz mono f32.
use anyhow::{Result, ensure};
use dasp_sample::Sample;
use bytes::{Buf, Bytes, BytesMut};
use rubato::{FftFixedInOut, Resampler};

pub(crate) const PROCESSING_FRAME_SAMPLES: usize = 320;
use crate::recognition::RECOGNITION_CHUNK_SAMPLES;

pub(crate) struct AudioProcessor {
    pcm_buffer: Vec<i16>,
    buffered_pcm_bytes: usize,
    resample_input: Vec<f32>,
    input_frame_samples: usize,
    resampler: Option<FftFixedInOut<f32>>,
    output_chunk: BytesMut,
    output_samples: usize,
}
impl AudioProcessor {
    pub fn new(sample_rate: u32) -> Result<Self> {
        ensure!([8000, 12000, 16000, 24000, 32000, 44100, 48000].contains(&sample_rate), "unsupported PCM sample rate: {sample_rate}");
        let input_frame_samples = sample_rate as usize / 50;
        let resampler = if sample_rate == 16000 { None } else {
            let resampler = FftFixedInOut::new(sample_rate as usize, 16000, input_frame_samples, 1)?;
            ensure!(resampler.input_frames_next() == input_frame_samples && resampler.output_frames_next() == PROCESSING_FRAME_SAMPLES,
                "unexpected resampler block size");
            Some(resampler)
        };
        Ok(Self { pcm_buffer: vec![0; input_frame_samples], buffered_pcm_bytes: 0, resample_input: vec![0.0; input_frame_samples], input_frame_samples, resampler, output_chunk: BytesMut::new(), output_samples: 0 })
    }
    /// Consume bytes through the next 20 ms frame. The returned slice borrows the
    /// final output chunk: neither resampling nor beep needs an intermediate copy.
    pub fn next_frame(&mut self, data: &mut &[u8]) -> Result<Option<&[f32]>> {
        let chunk_bytes = RECOGNITION_CHUNK_SAMPLES * size_of::<f32>();
        if self.output_chunk.is_empty() {
            // u8 allocations do not promise f32 alignment. Reserve padding and align
            // the live view explicitly; all subsequent splits are multiples of four.
            if self.output_chunk.capacity() < chunk_bytes + align_of::<f32>() - 1 {
                self.output_chunk.reserve(4 * chunk_bytes + align_of::<f32>() - 1);
            }
            let padding = self.output_chunk.as_ptr().align_offset(align_of::<f32>());
            self.output_chunk.resize(padding, 0);
            self.output_chunk.advance(padding);
            self.output_chunk.resize(chunk_bytes, 0); // Typed views only cover initialized bytes.
        }
        ensure!(self.output_samples < RECOGNITION_CHUNK_SAMPLES, "completed audio chunk must be taken before writing more");
        let block_bytes = self.input_frame_samples * size_of::<i16>();
        // Network buffers need not be aligned, and a message may split an i16.
        // Borrow complete aligned blocks; only the fallback copies into reusable,
        // typed storage. No per-sample byte assembly or per-message allocation.
        let direct = if self.buffered_pcm_bytes == 0 && data.len() >= block_bytes {
            bytemuck::try_cast_slice::<u8, i16>(&data[..block_bytes]).ok()
        } else { None };
        let samples = if let Some(samples) = direct {
            *data = &data[block_bytes..];
            samples
        } else {
            let count = (block_bytes - self.buffered_pcm_bytes).min(data.len());
            bytemuck::cast_slice_mut::<i16, u8>(&mut self.pcm_buffer)[self.buffered_pcm_bytes..self.buffered_pcm_bytes + count]
                .copy_from_slice(&data[..count]);
            *data = &data[count..];
            self.buffered_pcm_bytes += count;
            if self.buffered_pcm_bytes < block_bytes { return Ok(None); }
            self.buffered_pcm_bytes = 0;
            self.pcm_buffer.as_slice()
        };
        let begin = self.output_samples;
        let end = begin + PROCESSING_FRAME_SAMPLES;
        let output_chunk = &mut bytemuck::cast_slice_mut::<u8, f32>(&mut self.output_chunk)[begin..end];
        let destination = if self.resampler.is_some() { self.resample_input.as_mut_slice() } else { &mut *output_chunk };
        for (dest, sample) in destination.iter_mut().zip(samples) {
            // from_le is an identity on both supported release architectures.
            *dest = i16::from_le(*sample).to_sample::<f32>();
        }
        if let Some(resampler) = &mut self.resampler {
            resampler.process_into_buffer(&[self.resample_input.as_slice()], &mut [output_chunk], None)?;
        }
        self.output_samples = end;
        Ok(Some(&bytemuck::cast_slice::<u8, f32>(&self.output_chunk)[begin..end]))
    }

    /// Freeze the completed 100 ms chunk without copying. Bytes owns that region
    /// until the last worker releases it; subsequent writes use disjoint storage.
    pub fn take_recognition_chunk(&mut self) -> Option<Bytes> {
        if self.output_samples != RECOGNITION_CHUNK_SAMPLES { return None; }
        self.output_samples = 0;
        Some(self.output_chunk.split_to(RECOGNITION_CHUNK_SAMPLES * size_of::<f32>()).freeze())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_resampling_is_independent_of_message_boundaries() {
        for sample_rate in [8000, 12000, 16000, 24000, 32000, 44100, 48000] {
            let resample_input: Vec<i16> = (0..sample_rate / 5).map(|i| {
                (12000.0 * (std::f64::consts::TAU * 1309.0 * i as f64 / sample_rate as f64).sin()) as i16
            }).collect();
            let bytes: Vec<u8> = resample_input.iter().flat_map(|s| s.to_le_bytes()).collect();
            let mut expected = Vec::new();
            for size in [bytes.len(), 1, 137, 640, 1901] {
                let mut frames = AudioProcessor::new(sample_rate).unwrap();
                let mut actual = Vec::new();
                let mut frozen = Vec::new();
                for mut data in bytes.chunks(size) {
                    while let Some(frame) = frames.next_frame(&mut data).unwrap() {
                        actual.extend_from_slice(frame);
                        if let Some(chunk) = frames.take_recognition_chunk() { frozen.push(chunk); }
                    }
                }
                // Hold earlier chunks throughout later writes: their samples must not change.
                let shared: Vec<f32> = frozen.iter().flat_map(|b| bytemuck::cast_slice::<u8, f32>(b).iter().copied()).collect();
                assert_eq!(shared, actual);
                if expected.is_empty() { expected = actual; }
                else { assert_eq!(actual, expected, "sample_rate {sample_rate}, chunk {size}"); }
            }
            assert_eq!(expected.len(), 3200);
            assert!(expected.iter().all(|s| s.is_finite()));
            let rms = (expected[640..].iter().map(|s| s*s).sum::<f32>() / 2560.0).sqrt();
            assert!((rms - 12000.0 / 32768.0 / 2.0_f32.sqrt()).abs() < 0.01, "sample_rate {sample_rate}: rms {rms}");
            if sample_rate == 16000 {
                assert_eq!(expected, resample_input.iter().map(|s| s.to_sample::<f32>()).collect::<Vec<_>>());
            }
        }
        assert!(AudioProcessor::new(0).is_err());
        assert!(AudioProcessor::new(96000).is_err());
    }
    #[test]
    fn freeze_shares_output_storage_and_preserves_previous_chunks() {
        let mut frames = AudioProcessor::new(8000).unwrap();
        let input_ptr = frames.resample_input.as_ptr();
        let data = vec![0; 800 * 2];
        let mut remaining = data.as_slice();
        let output_ptr = frames.next_frame(&mut remaining).unwrap().unwrap().as_ptr();
        while !remaining.is_empty() { frames.next_frame(&mut remaining).unwrap().unwrap(); }
        let chunk = frames.take_recognition_chunk().unwrap();
        assert_eq!(chunk.as_ptr(), output_ptr.cast::<u8>());
        let shared = chunk.clone();
        assert_eq!(chunk.as_ptr(), shared.as_ptr());
        for _ in 0..12 {
            let mut remaining = data.as_slice();
            while let Some(_) = frames.next_frame(&mut remaining).unwrap() {
                let _ = frames.take_recognition_chunk();
            }
        }
        assert_eq!(input_ptr, frames.resample_input.as_ptr());
        assert!(bytemuck::cast_slice::<u8, f32>(&shared).iter().all(|s| *s == 0.0));
    }
    #[test]
    fn silence_and_incomplete_tail_are_preserved_without_padding() {
        let mut frames = AudioProcessor::new(8000).unwrap();
        let mut data = &[0u8; 321][..];
        assert!(frames.next_frame(&mut data).unwrap().unwrap().iter().all(|s| *s == 0.0));
        assert!(frames.next_frame(&mut data).unwrap().is_none());
        assert_eq!(frames.buffered_pcm_bytes, 1);
        assert!(frames.next_frame(&mut &[][..]).unwrap().is_none());
        assert!(frames.next_frame(&mut &[0][..]).unwrap().is_none());
        assert_eq!(frames.buffered_pcm_bytes, 2);
        assert!(frames.take_recognition_chunk().is_none());
    }
}
