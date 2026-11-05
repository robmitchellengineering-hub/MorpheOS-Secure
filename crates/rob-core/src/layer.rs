//! Receiver layer selection (spec, "Protection").
//!
//! For each period, at playout time, the receiver uses the best copy it has:
//! full 24-bit from the period's own packet, else the 16-bit copy carried in the
//! next packet, else the BC10 copy in the packet after that, else concealment.

/// Which copy of a period was played.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Full24,
    Pcm16,
    Bc10,
    Conceal,
}

/// Which copies of a period had arrived by its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Available {
    pub pcm24: bool,
    pub pcm16: bool,
    pub bc10: bool,
}

/// Chooses the layer to play.
pub fn select(a: Available) -> Layer {
    match a {
        Available { pcm24: true, .. } => Layer::Full24,
        Available { pcm16: true, .. } => Layer::Pcm16,
        Available { bc10: true, .. } => Layer::Bc10,
        _ => Layer::Conceal,
    }
}

/// How many redundancy steps (0, 1 or 2) a receiver can use. A copy carried
/// `k` packets later arrives `k × packet time` later, so it only helps if the
/// latency leaves that much room above the base latency.
pub fn protection_depth(latency_us: u32, base_latency_us: u32, packet_time_us: u32) -> u8 {
    if packet_time_us == 0 {
        return 0;
    }
    let spare = latency_us.saturating_sub(base_latency_us);
    (spare / packet_time_us).min(2) as u8
}

/// A small, allocation-free TPDF dither source for playing PCM16 copies.
///
/// It fills the zeroed low 8 bits with triangular noise of ±1 16-bit LSB, so a
/// switch from 24-bit to 16-bit changes the noise floor's level, never its
/// character (no truncation distortion).
#[derive(Debug, Clone)]
pub struct Dither(u32);

impl Dither {
    pub fn new(seed: u32) -> Self {
        Dither(seed | 1)
    }

    #[inline]
    fn next_u8(&mut self) -> i32 {
        // xorshift32
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 24) as i32
    }

    /// Adds TPDF dither (in 24-bit units, range −255..=255) to a PCM16 sample.
    #[inline]
    pub fn apply(&mut self, pcm16_as_24: i32) -> i32 {
        let tpdf = self.next_u8() - self.next_u8();
        crate::pcm::clamp24(pcm16_as_24 + tpdf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_the_best_copy() {
        let all = Available {
            pcm24: true,
            pcm16: true,
            bc10: true,
        };
        assert_eq!(select(all), Layer::Full24);
        assert_eq!(
            select(Available {
                pcm24: false,
                ..all
            }),
            Layer::Pcm16
        );
        assert_eq!(
            select(Available {
                bc10: true,
                ..Default::default()
            }),
            Layer::Bc10
        );
        assert_eq!(select(Available::default()), Layer::Conceal);
    }

    #[test]
    fn depth_matches_spec_table() {
        // Spec: Pi on Wi-Fi, P = 2 ms, base ~6 ms.
        assert_eq!(protection_depth(6_000, 6_000, 2_000), 0);
        assert_eq!(protection_depth(8_000, 6_000, 2_000), 1);
        assert_eq!(protection_depth(10_000, 6_000, 2_000), 2);
        assert_eq!(protection_depth(30_000, 6_000, 2_000), 2);
        assert_eq!(protection_depth(5_000, 6_000, 2_000), 0);
    }

    #[test]
    fn dither_stays_within_one_16bit_step() {
        let mut d = Dither::new(42);
        let mut sum = 0i64;
        for _ in 0..10_000 {
            let v = d.apply(1 << 16);
            let e = v - (1 << 16);
            assert!((-255..=255).contains(&e));
            sum += i64::from(e);
        }
        assert!(
            sum.abs() < 10_000 * 10,
            "dither should be roughly zero-mean"
        );
    }
}
