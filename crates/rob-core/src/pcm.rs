//! PCM24 and PCM16 block codecs.
//!
//! Samples are `i32` holding signed 24-bit values (range −2^23 ..= 2^23−1),
//! interleaved by channel. PCM24 is AES67 L24: 3 bytes big-endian per sample.
//! PCM16 carries the top 16 bits of each 24-bit sample; the receiver adds
//! dither when it plays one (see `layer`).

use crate::Error;

/// Largest 24-bit sample value.
pub const MAX_24: i32 = (1 << 23) - 1;
/// Smallest 24-bit sample value.
pub const MIN_24: i32 = -(1 << 23);

/// Clamps an `i32` to the 24-bit range.
#[inline]
pub fn clamp24(s: i32) -> i32 {
    s.clamp(MIN_24, MAX_24)
}

/// Encodes samples as PCM24 into `out`. Returns bytes written.
pub fn encode_pcm24(samples: &[i32], out: &mut [u8]) -> Result<usize, Error> {
    let n = samples.len() * 3;
    if out.len() < n {
        return Err(Error::OutputTooSmall);
    }
    for (s, o) in samples.iter().zip(out.chunks_exact_mut(3)) {
        let v = clamp24(*s);
        o[0] = (v >> 16) as u8;
        o[1] = (v >> 8) as u8;
        o[2] = v as u8;
    }
    Ok(n)
}

/// Decodes PCM24 into `out`. Returns samples written.
pub fn decode_pcm24(data: &[u8], out: &mut [i32]) -> Result<usize, Error> {
    if data.len() % 3 != 0 {
        return Err(Error::BadBlocks);
    }
    let n = data.len() / 3;
    if out.len() < n {
        return Err(Error::OutputTooSmall);
    }
    for (d, o) in data.chunks_exact(3).zip(out.iter_mut()) {
        // Place the 24 bits at the top of an i32, then arithmetic-shift back
        // down so the sign extends.
        let v = (i32::from(d[0]) << 24) | (i32::from(d[1]) << 16) | (i32::from(d[2]) << 8);
        *o = v >> 8;
    }
    Ok(n)
}

/// Encodes the top 16 bits of each 24-bit sample. Returns bytes written.
pub fn encode_pcm16(samples: &[i32], out: &mut [u8]) -> Result<usize, Error> {
    let n = samples.len() * 2;
    if out.len() < n {
        return Err(Error::OutputTooSmall);
    }
    for (s, o) in samples.iter().zip(out.chunks_exact_mut(2)) {
        let v = (clamp24(*s) >> 8) as i16;
        o.copy_from_slice(&v.to_be_bytes());
    }
    Ok(n)
}

/// Decodes PCM16 back to the 24-bit scale (low 8 bits zero). Returns samples written.
pub fn decode_pcm16(data: &[u8], out: &mut [i32]) -> Result<usize, Error> {
    if data.len() % 2 != 0 {
        return Err(Error::BadBlocks);
    }
    let n = data.len() / 2;
    if out.len() < n {
        return Err(Error::OutputTooSmall);
    }
    for (d, o) in data.chunks_exact(2).zip(out.iter_mut()) {
        *o = i32::from(i16::from_be_bytes([d[0], d[1]])) << 8;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDGE: [i32; 7] = [0, 1, -1, MAX_24, MIN_24, 0x12_3456, -0x12_3456];

    #[test]
    fn pcm24_is_bit_exact() {
        let mut buf = [0u8; 21];
        let mut back = [0i32; 7];
        assert_eq!(encode_pcm24(&EDGE, &mut buf).unwrap(), 21);
        assert_eq!(decode_pcm24(&buf, &mut back).unwrap(), 7);
        assert_eq!(back, EDGE);
    }

    #[test]
    fn pcm24_matches_aes67_byte_order() {
        let mut buf = [0u8; 3];
        encode_pcm24(&[0x12_3456], &mut buf).unwrap();
        assert_eq!(buf, [0x12, 0x34, 0x56]);
    }

    #[test]
    fn pcm16_keeps_the_top_16_bits() {
        let mut buf = [0u8; 14];
        let mut back = [0i32; 7];
        encode_pcm16(&EDGE, &mut buf).unwrap();
        decode_pcm16(&buf, &mut back).unwrap();
        for (a, b) in EDGE.iter().zip(back.iter()) {
            assert_eq!(*b, (a >> 8) << 8);
            assert!((a - b).abs() < 256);
        }
    }

    #[test]
    fn out_of_range_input_is_clamped() {
        let mut buf = [0u8; 3];
        let mut back = [0i32; 1];
        encode_pcm24(&[i32::MAX], &mut buf).unwrap();
        decode_pcm24(&buf, &mut back).unwrap();
        assert_eq!(back[0], MAX_24);
    }
}
