//! Correction packets (PRIVATE: patent candidate; specified in
//! `docs/ip/04a-private-protocol-extensions.md`, NOT in the published
//! `04-protocol-spec-v0.1.md`).
//!
//! A correction carries an additive difference for audio a flow has already
//! sent: `truth − what was sent`, per frame and channel, starting at the
//! network sample in the RTP timestamp. Receivers add it to audio they have
//! not played yet (see `rob-node` `patch.rs`).
//!
//! Corrections travel in their own RTP stream (own SSRC, payload type
//! [`CORRECTION_PT`]) so the audio flow's sequence numbers, loss statistics
//! and duplicate detection are untouched. Payload (big-endian):
//!
//! | Offset | Bits | Field |
//! | --- | --- | --- |
//! | 0 | 32 | target SSRC: the audio flow being corrected |
//! | 4 | 16 | frames |
//! | 6 | 8 | channels |
//! | 7 | 8 | flags: bit 0 = repeat copy; bits 4-7 encoding (0 = f32) |
//! | 8 | frames × channels × 32 | difference samples, IEEE-754 f32, interleaved |
//!
//! Decoding never allocates.

use crate::Error;

/// RTP payload type for correction packets (dynamic range).
pub const CORRECTION_PT: u8 = 125;
/// Bytes before the samples.
pub const CORRECTION_HEADER_LEN: usize = 8;
/// Largest payload, so that RTP + extension + payload fits a 1,500-byte MTU
/// with IP/UDP headers and room to spare. Longer corrections are split.
pub const MAX_CORRECTION_PAYLOAD: usize = 1_400;

const FLAG_REPEAT: u8 = 0x01;
const ENCODING_F32: u8 = 0x00;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorrectionHeader {
    pub target_ssrc: u32,
    pub frames: u16,
    pub channels: u8,
    /// A repeat of an earlier copy (sent once more for loss protection).
    pub repeat: bool,
}

impl CorrectionHeader {
    pub fn samples(&self) -> usize {
        usize::from(self.frames) * usize::from(self.channels)
    }

    /// Payload bytes this correction needs.
    pub fn payload_len(&self) -> usize {
        CORRECTION_HEADER_LEN + self.samples() * 4
    }
}

/// Most frames one packet can carry for `channels`.
pub fn max_frames(channels: u8) -> usize {
    (MAX_CORRECTION_PAYLOAD - CORRECTION_HEADER_LEN) / (4 * usize::from(channels.max(1)))
}

/// Writes a correction payload; `delta` is interleaved, `frames × channels`.
pub fn write_payload(out: &mut [u8], h: &CorrectionHeader, delta: &[f32]) -> Result<usize, Error> {
    if h.channels == 0 || h.frames == 0 || delta.len() != h.samples() {
        return Err(Error::FieldRange);
    }
    let len = h.payload_len();
    if len > MAX_CORRECTION_PAYLOAD {
        return Err(Error::FieldRange);
    }
    if out.len() < len {
        return Err(Error::OutputTooSmall);
    }
    out[0..4].copy_from_slice(&h.target_ssrc.to_be_bytes());
    out[4..6].copy_from_slice(&h.frames.to_be_bytes());
    out[6] = h.channels;
    out[7] = (ENCODING_F32 << 4) | if h.repeat { FLAG_REPEAT } else { 0 };
    for (d, v) in out[CORRECTION_HEADER_LEN..len]
        .chunks_exact_mut(4)
        .zip(delta)
    {
        d.copy_from_slice(&v.to_be_bytes());
    }
    Ok(len)
}

/// Parses a correction payload: the header and the raw sample bytes (read
/// them with [`read_delta`]).
pub fn parse_payload(p: &[u8]) -> Result<(CorrectionHeader, &[u8]), Error> {
    if p.len() < CORRECTION_HEADER_LEN {
        return Err(Error::TooShort);
    }
    let h = CorrectionHeader {
        target_ssrc: u32::from_be_bytes([p[0], p[1], p[2], p[3]]),
        frames: u16::from_be_bytes([p[4], p[5]]),
        channels: p[6],
        repeat: p[7] & FLAG_REPEAT != 0,
    };
    if p[7] >> 4 != ENCODING_F32 || h.channels == 0 || h.frames == 0 {
        return Err(Error::FieldRange);
    }
    let body = &p[CORRECTION_HEADER_LEN..];
    if body.len() < h.samples() * 4 {
        return Err(Error::TooShort);
    }
    Ok((h, &body[..h.samples() * 4]))
}

/// Decodes sample bytes from [`parse_payload`] into `out` (at least as long).
/// Non-finite values are refused. No allocation.
pub fn read_delta(bytes: &[u8], out: &mut [f32]) -> Result<usize, Error> {
    let n = bytes.len() / 4;
    if out.len() < n {
        return Err(Error::OutputTooSmall);
    }
    for (o, b) in out.iter_mut().zip(bytes.chunks_exact(4)) {
        let v = f32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        if !v.is_finite() {
            return Err(Error::FieldRange);
        }
        *o = v;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(frames: u16) -> CorrectionHeader {
        CorrectionHeader {
            target_ssrc: 0xA1B2_C3D4,
            frames,
            channels: 2,
            repeat: true,
        }
    }

    #[test]
    fn round_trips_exactly() {
        let delta: Vec<f32> = (0..256).map(|k| (k as f32 * 0.37).sin() * 1.7).collect();
        let h = hdr(128);
        let mut buf = [0u8; MAX_CORRECTION_PAYLOAD];
        let n = write_payload(&mut buf, &h, &delta).unwrap();
        assert_eq!(n, 8 + 1_024);
        let (got, body) = parse_payload(&buf[..n]).unwrap();
        assert_eq!(got, h);
        let mut out = [0f32; 256];
        assert_eq!(read_delta(body, &mut out).unwrap(), 256);
        assert_eq!(&out[..], &delta[..], "bit-exact");
    }

    #[test]
    fn a_stereo_repair_fits_one_packet_and_size_limits_hold() {
        // A 2 ms period plus the 32-frame fade back in, stereo.
        assert!(max_frames(2) >= 96 + 32);
        let too_long = hdr(max_frames(2) as u16 + 1);
        let delta = vec![0.0f32; too_long.samples()];
        let mut buf = [0u8; 2_000];
        assert_eq!(
            write_payload(&mut buf, &too_long, &delta),
            Err(Error::FieldRange)
        );
    }

    #[test]
    fn malformed_payloads_are_refused() {
        assert_eq!(parse_payload(&[0; 4]), Err(Error::TooShort));
        let h = hdr(4);
        let mut buf = [0u8; 64];
        let n = write_payload(&mut buf, &h, &[0.5; 8]).unwrap();
        assert_eq!(
            parse_payload(&buf[..n - 1]).map(|x| x.0),
            Err(Error::TooShort)
        );
        let mut bad = buf;
        bad[7] = 0x10; // unknown encoding
        assert_eq!(
            parse_payload(&bad[..n]).map(|x| x.0),
            Err(Error::FieldRange)
        );
        let mut nan = buf;
        nan[8..12].copy_from_slice(&f32::NAN.to_be_bytes());
        let (_, body) = parse_payload(&nan[..n]).unwrap();
        assert_eq!(read_delta(body, &mut [0.0; 8]), Err(Error::FieldRange));
        assert_eq!(
            write_payload(&mut buf, &h, &[0.5; 7]),
            Err(Error::FieldRange)
        );
    }
}
