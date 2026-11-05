//! Media timestamps on network time (spec, "Stream and timing model").
//!
//! The RTP timestamp is the capture time of a packet's first sample, counted in
//! samples on network time, modulo 2^32. It wraps about every 24.9 hours at
//! 48 kHz, so receivers unwrap it against their own network time.

const NS_PER_S: u128 = 1_000_000_000;
const WRAP: u64 = 1 << 32;
const HALF_WRAP: i128 = 1 << 31;

/// RTP timestamp for a capture time given in network-time nanoseconds.
pub fn rtp_timestamp(capture_ns: u64, sample_rate: u32) -> u32 {
    samples_at(capture_ns, sample_rate) as u32
}

/// Whole samples elapsed at `ns` on network time (the unwrapped timestamp).
pub fn samples_at(ns: u64, sample_rate: u32) -> u64 {
    (u128::from(ns) * u128::from(sample_rate) / NS_PER_S) as u64
}

/// Network time in nanoseconds at which `sample` (unwrapped) was captured.
pub fn ns_at(sample: u64, sample_rate: u32) -> u64 {
    (u128::from(sample) * NS_PER_S / u128::from(sample_rate)) as u64
}

/// Expands a 32-bit RTP timestamp to the 64-bit sample count nearest to
/// `reference` (normally the receiver's current network time in samples).
pub fn unwrap(ts: u32, reference: u64) -> u64 {
    let cand = (reference & !(WRAP - 1)) | u64::from(ts);
    let diff = i128::from(cand) - i128::from(reference);
    if diff > HALF_WRAP {
        cand.checked_sub(WRAP).unwrap_or(cand)
    } else if diff < -HALF_WRAP {
        cand + WRAP
    } else {
        cand
    }
}

/// Network time (ns) at which the sample with RTP timestamp `ts` must leave the
/// receiver's converter, given link latency `latency_ns`.
pub fn playout_ns(ts: u32, now_ns: u64, sample_rate: u32, latency_ns: u64) -> u64 {
    let sample = unwrap(ts, samples_at(now_ns, sample_rate));
    ns_at(sample, sample_rate) + latency_ns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_counts_samples() {
        assert_eq!(rtp_timestamp(1_000_000_000, 48_000), 48_000);
        assert_eq!(rtp_timestamp(2_000_000, 48_000), 96); // 2 ms = 96 frames
    }

    #[test]
    fn unwrap_handles_both_sides_of_a_wrap() {
        let reference = 5 * WRAP + 10; // just after a wrap
        assert_eq!(unwrap(5, reference), 5 * WRAP + 5);
        assert_eq!(unwrap(u32::MAX - 3, reference), 5 * WRAP - 4);
        let before = 5 * WRAP - 10; // just before a wrap
        assert_eq!(unwrap(3, before), 5 * WRAP + 3);
        assert_eq!(unwrap(0, 0), 0);
        assert_eq!(
            unwrap(u32::MAX, 0),
            u64::from(u32::MAX),
            "no negative times"
        );
    }

    #[test]
    fn playout_adds_latency_after_25_hours() {
        let sr = 48_000;
        let capture = 25 * 3600 * 1_000_000_000u64; // past the first wrap
        let ts = rtp_timestamp(capture, sr);
        let now = capture + 3_000_000; // arrived 3 ms later
        let play = playout_ns(ts, now, sr, 8_000_000);
        let err = play as i128 - i128::from(capture + 8_000_000);
        assert!(err.abs() < 21_000, "within one sample: {err} ns");
    }
}
