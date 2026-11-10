//! Per-connection PCM16LE decoding and streaming conversion to 16 kHz mono f32.
use anyhow::{Result, ensure};
use dasp_sample::Sample;
use bytes::{Buf, Bytes, BytesMut};
use rubato::{FftFixedInOut, Resampler};

pub(crate) const FRAME_SAMPLES: usize = 320;
pub(crate) const ASR_CHUNK_SAMPLES: usize = 1600;

pub(crate) struct PcmFrames {
    pcm: Vec<i16>,
    filled_bytes: usize,
    input: Vec<f32>,
    input_samples: usize,
    resampler: Option<FftFixedInOut<f32>>,
    output: BytesMut,
    written: usize,
}
impl PcmFrames {
    pub fn new(rate: u32) -> Result<Self> {
        ensure!([8000, 12000, 16000, 24000, 32000, 44100, 48000].contains(&rate), "unsupported PCM sample rate: {rate}");
        let input_samples = rate as usize / 50;
        let resampler = if rate == 16000 { None } else {
            let resampler = FftFixedInOut::new(rate as usize, 16000, input_samples, 1)?;
            ensure!(resampler.input_frames_next() == input_samples && resampler.output_frames_next() == FRAME_SAMPLES,
                "unexpected resampler block size");
            Some(resampler)
        };
        Ok(Self { pcm: vec![0; input_samples], filled_bytes: 0, input: vec![0.0; input_samples], input_samples, resampler, output: BytesMut::new(), written: 0 })
    }
    /// Consume bytes through the next 20 ms frame. The returned slice borrows the
    /// final output chunk: neither resampling nor beep needs an intermediate copy.
    pub fn next_frame(&mut self, data: &mut &[u8]) -> Result<Option<&[f32]>> {
        let chunk_bytes = ASR_CHUNK_SAMPLES * size_of::<f32>();
        if self.output.is_empty() {
            // u8 allocations do not promise f32 alignment. Reserve padding and align
            // the live view explicitly; all subsequent splits are multiples of four.
            if self.output.capacity() < chunk_bytes + align_of::<f32>() - 1 {
                self.output.reserve(4 * chunk_bytes + align_of::<f32>() - 1);
            }
            let padding = self.output.as_ptr().align_offset(align_of::<f32>());
            self.output.resize(padding, 0);
            self.output.advance(padding);
            self.output.resize(chunk_bytes, 0); // Typed views only cover initialized bytes.
        }
        ensure!(self.written < ASR_CHUNK_SAMPLES, "completed audio chunk must be taken before writing more");
        let block_bytes = self.input_samples * size_of::<i16>();
        // Network buffers need not be aligned, and a message may split an i16.
        // Borrow complete aligned blocks; only the fallback copies into reusable,
        // typed storage. No per-sample byte assembly or per-message allocation.
        let direct = if self.filled_bytes == 0 && data.len() >= block_bytes {
            bytemuck::try_cast_slice::<u8, i16>(&data[..block_bytes]).ok()
        } else { None };
        let samples = if let Some(samples) = direct {
            *data = &data[block_bytes..];
            samples
        } else {
            let count = (block_bytes - self.filled_bytes).min(data.len());
            bytemuck::cast_slice_mut::<i16, u8>(&mut self.pcm)[self.filled_bytes..self.filled_bytes + count]
                .copy_from_slice(&data[..count]);
            *data = &data[count..];
            self.filled_bytes += count;
            if self.filled_bytes < block_bytes { return Ok(None); }
            self.filled_bytes = 0;
            self.pcm.as_slice()
        };
        let begin = self.written;
        let end = begin + FRAME_SAMPLES;
        let output = &mut bytemuck::cast_slice_mut::<u8, f32>(&mut self.output)[begin..end];
        let destination = if self.resampler.is_some() { self.input.as_mut_slice() } else { &mut *output };
        for (dest, sample) in destination.iter_mut().zip(samples) {
            // from_le is an identity on both supported release architectures.
            *dest = i16::from_le(*sample).to_sample::<f32>();
        }
        if let Some(resampler) = &mut self.resampler {
            resampler.process_into_buffer(&[self.input.as_slice()], &mut [output], None)?;
        }
        self.written = end;
        Ok(Some(&bytemuck::cast_slice::<u8, f32>(&self.output)[begin..end]))
    }

    /// Freeze the completed 100 ms chunk without copying. Bytes owns that region
    /// until the last worker releases it; subsequent writes use disjoint storage.
    pub fn take_chunk(&mut self) -> Option<Bytes> {
        if self.written != ASR_CHUNK_SAMPLES { return None; }
        self.written = 0;
        Some(self.output.split_to(ASR_CHUNK_SAMPLES * size_of::<f32>()).freeze())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_resampling_is_independent_of_message_boundaries() {
        for rate in [8000, 12000, 16000, 24000, 32000, 44100, 48000] {
            let input: Vec<i16> = (0..rate / 5).map(|i| {
                (12000.0 * (std::f64::consts::TAU * 1309.0 * i as f64 / rate as f64).sin()) as i16
            }).collect();
            let bytes: Vec<u8> = input.iter().flat_map(|s| s.to_le_bytes()).collect();
            let mut expected = Vec::new();
            for size in [bytes.len(), 1, 137, 640, 1901] {
                let mut frames = PcmFrames::new(rate).unwrap();
                let mut actual = Vec::new();
                let mut frozen = Vec::new();
                for mut data in bytes.chunks(size) {
                    while let Some(frame) = frames.next_frame(&mut data).unwrap() {
                        actual.extend_from_slice(frame);
                        if let Some(chunk) = frames.take_chunk() { frozen.push(chunk); }
                    }
                }
                // Hold earlier chunks throughout later writes: their samples must not change.
                let shared: Vec<f32> = frozen.iter().flat_map(|b| bytemuck::cast_slice::<u8, f32>(b).iter().copied()).collect();
                assert_eq!(shared, actual);
                if expected.is_empty() { expected = actual; }
                else { assert_eq!(actual, expected, "rate {rate}, chunk {size}"); }
            }
            assert_eq!(expected.len(), 3200);
            assert!(expected.iter().all(|s| s.is_finite()));
            let rms = (expected[640..].iter().map(|s| s*s).sum::<f32>() / 2560.0).sqrt();
            assert!((rms - 12000.0 / 32768.0 / 2.0_f32.sqrt()).abs() < 0.01, "rate {rate}: rms {rms}");
            if rate == 16000 {
                assert_eq!(expected, input.iter().map(|s| s.to_sample::<f32>()).collect::<Vec<_>>());
            }
        }
        assert!(PcmFrames::new(0).is_err());
        assert!(PcmFrames::new(96000).is_err());
    }
    #[test]
    fn freeze_shares_output_storage_and_preserves_previous_chunks() {
        let mut frames = PcmFrames::new(8000).unwrap();
        let input_ptr = frames.input.as_ptr();
        let data = vec![0; 800 * 2];
        let mut remaining = data.as_slice();
        let output_ptr = frames.next_frame(&mut remaining).unwrap().unwrap().as_ptr();
        while !remaining.is_empty() { frames.next_frame(&mut remaining).unwrap().unwrap(); }
        let chunk = frames.take_chunk().unwrap();
        assert_eq!(chunk.as_ptr(), output_ptr.cast::<u8>());
        let shared = chunk.clone();
        assert_eq!(chunk.as_ptr(), shared.as_ptr());
        for _ in 0..12 {
            let mut remaining = data.as_slice();
            while let Some(_) = frames.next_frame(&mut remaining).unwrap() {
                let _ = frames.take_chunk();
            }
        }
        assert_eq!(input_ptr, frames.input.as_ptr());
        assert!(bytemuck::cast_slice::<u8, f32>(&shared).iter().all(|s| *s == 0.0));
    }
    #[test]
    fn silence_and_incomplete_tail_are_preserved_without_padding() {
        let mut frames = PcmFrames::new(8000).unwrap();
        let mut data = &[0u8; 321][..];
        assert!(frames.next_frame(&mut data).unwrap().unwrap().iter().all(|s| *s == 0.0));
        assert!(frames.next_frame(&mut data).unwrap().is_none());
        assert_eq!(frames.filled_bytes, 1);
        assert!(frames.next_frame(&mut &[][..]).unwrap().is_none());
        assert!(frames.next_frame(&mut &[0][..]).unwrap().is_none());
        assert_eq!(frames.filled_bytes, 2);
        assert!(frames.take_chunk().is_none());
    }
}
