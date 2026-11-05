//! BC10: block-companded 10-bit audio, the last-resort redundancy layer.
//!
//! The idea comes from NICAM: within each short block (1 ms), every sample in a
//! channel is shifted right by the same amount so the loudest one fits in 10
//! bits. Quiet passages keep their detail, and the codec adds no delay beyond
//! the block itself.
//!
//! Layout (spec, block type 0x03):
//! 1. One scale byte (the shift count, 0..=14) per block per channel, block-major:
//!    `[b0c0, b0c1, b1c0, b1c1, ...]`.
//! 2. Then every sample as a 10-bit two's-complement value, interleaved in frame
//!    order, packed MSB-first, with the last byte zero-padded.

use crate::Error;

/// Largest shift: 24-bit full scale (2^23 − 1) >> 14 = 511.
pub const MAX_SHIFT: u8 = 14;
const Q_MAX: i32 = 511;
const Q_MIN: i32 = -512;

/// Encoded size in bytes.
pub fn encoded_len(frames: usize, channels: usize, frames_per_block: usize) -> usize {
    let blocks = frames.div_ceil(frames_per_block);
    blocks * channels + (frames * channels * 10).div_ceil(8)
}

fn choose_shift(max: i32, min: i32) -> u8 {
    let mut s = 0u8;
    while s < MAX_SHIFT && ((max >> s) > Q_MAX || (min >> s) < Q_MIN) {
        s += 1;
    }
    s
}

#[inline]
fn quantise(x: i32, shift: u8) -> i32 {
    let half = if shift > 0 { 1i32 << (shift - 1) } else { 0 };
    ((x + half) >> shift).clamp(Q_MIN, Q_MAX)
}

/// Encodes interleaved 24-bit samples. Returns bytes written.
pub fn encode(
    samples: &[i32],
    channels: usize,
    frames_per_block: usize,
    out: &mut [u8],
) -> Result<usize, Error> {
    if channels == 0 || frames_per_block == 0 || samples.len() % channels != 0 {
        return Err(Error::FieldRange);
    }
    let frames = samples.len() / channels;
    let len = encoded_len(frames, channels, frames_per_block);
    if out.len() < len {
        return Err(Error::OutputTooSmall);
    }
    let blocks = frames.div_ceil(frames_per_block);
    let (scales, bits) = out[..len].split_at_mut(blocks * channels);

    for b in 0..blocks {
        let f0 = b * frames_per_block;
        let f1 = (f0 + frames_per_block).min(frames);
        for c in 0..channels {
            let (mut max, mut min) = (0i32, 0i32);
            for f in f0..f1 {
                let x = samples[f * channels + c];
                max = max.max(x);
                min = min.min(x);
            }
            scales[b * channels + c] = choose_shift(max, min);
        }
    }

    bits.fill(0);
    let mut bitpos = 0usize;
    for f in 0..frames {
        let b = f / frames_per_block;
        for c in 0..channels {
            let q = quantise(samples[f * channels + c], scales[b * channels + c]);
            write_bits(bits, bitpos, (q as u32) & 0x3FF);
            bitpos += 10;
        }
    }
    Ok(len)
}

/// Decodes into interleaved 24-bit samples. Returns samples written.
pub fn decode(
    data: &[u8],
    frames: usize,
    channels: usize,
    frames_per_block: usize,
    out: &mut [i32],
) -> Result<usize, Error> {
    if channels == 0 || frames_per_block == 0 {
        return Err(Error::FieldRange);
    }
    if data.len() < encoded_len(frames, channels, frames_per_block) {
        return Err(Error::TooShort);
    }
    let n = frames * channels;
    if out.len() < n {
        return Err(Error::OutputTooSmall);
    }
    let blocks = frames.div_ceil(frames_per_block);
    let (scales, bits) = data.split_at(blocks * channels);
    let mut bitpos = 0usize;
    for f in 0..frames {
        let b = f / frames_per_block;
        for c in 0..channels {
            let shift = scales[b * channels + c];
            if shift > MAX_SHIFT {
                return Err(Error::FieldRange);
            }
            let raw = read_bits(bits, bitpos) as i32;
            bitpos += 10;
            let q = (raw << 22) >> 22; // sign-extend 10 bits
            out[f * channels + c] = q << shift;
        }
    }
    Ok(n)
}

fn write_bits(buf: &mut [u8], bitpos: usize, value: u32) {
    for i in 0..10 {
        if value & (1 << (9 - i)) != 0 {
            let p = bitpos + i;
            buf[p / 8] |= 0x80 >> (p % 8);
        }
    }
}

fn read_bits(buf: &[u8], bitpos: usize) -> u32 {
    let mut v = 0u32;
    for i in 0..10 {
        let p = bitpos + i;
        v = (v << 1) | u32::from((buf[p / 8] >> (7 - p % 8)) & 1);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcm::{MAX_24, MIN_24};

    fn round_trip(samples: &[i32], ch: usize, fpb: usize) -> Vec<i32> {
        let frames = samples.len() / ch;
        let mut buf = vec![0u8; encoded_len(frames, ch, fpb)];
        encode(samples, ch, fpb, &mut buf).unwrap();
        let mut out = vec![0i32; samples.len()];
        decode(&buf, frames, ch, fpb, &mut out).unwrap();
        out
    }

    #[test]
    fn spec_size_budget_stereo_2ms() {
        // Spec size budget: 96 frames, 2 channels, 48-frame blocks = 244 bytes.
        assert_eq!(encoded_len(96, 2, 48), 244);
    }

    #[test]
    fn quiet_audio_is_exact() {
        let s: Vec<i32> = (0..96).map(|i| (i % 50) - 25).collect();
        assert_eq!(round_trip(&s, 2, 48), s);
    }

    #[test]
    fn error_is_within_half_a_step() {
        // A loud sine on the left, a quiet one on the right: each block and
        // channel gets its own shift, so the quiet channel keeps its detail.
        let mut s = Vec::new();
        for i in 0..96 {
            let t = i as f64 / 48_000.0 * 1_000.0 * core::f64::consts::TAU;
            s.push((t.sin() * 7_000_000.0) as i32);
            s.push((t.sin() * 3_000.0) as i32);
        }
        let back = round_trip(&s, 2, 48);
        for (i, (a, b)) in s.iter().zip(back.iter()).enumerate() {
            let shift = if i % 2 == 0 { 14 } else { 3 };
            assert!(
                (a - b).abs() <= 1 << shift,
                "sample {i}: {a} vs {b} differs by more than one step"
            );
        }
        // The quiet channel must be far more accurate than the loud one.
        let quiet_err = s
            .iter()
            .zip(&back)
            .skip(1)
            .step_by(2)
            .map(|(a, b)| (a - b).abs())
            .max()
            .unwrap();
        assert!(quiet_err <= 8, "quiet channel error {quiet_err}");
    }

    #[test]
    fn full_scale_does_not_wrap() {
        let s = vec![MAX_24, MIN_24, MAX_24, MIN_24];
        let back = round_trip(&s, 2, 48);
        assert!(back[0] > 8_000_000 && back[1] < -8_000_000, "{back:?}");
    }

    #[test]
    fn partial_last_block() {
        let s: Vec<i32> = (0..50).map(|i| i * 1000).collect();
        let back = round_trip(&s, 1, 48);
        for (a, b) in s.iter().zip(back.iter()) {
            assert!((a - b).abs() <= 128);
        }
    }
}
