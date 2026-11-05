//! Duplicate detection for multipath flows.
//!
//! Every path carries identical packets with the same RTP sequence number, so
//! the receiver keeps the first copy of each sequence number and drops the rest
//! (SMPTE 2022-7 semantics). The window covers the last 256 sequence numbers,
//! per the spec.

/// Window size in sequence numbers.
pub const WINDOW: u16 = 256;

/// What to do with an arriving packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// First copy: use it.
    New,
    /// Already seen on another path: drop it.
    Duplicate,
    /// Older than the window: drop it (it would be far too late anyway).
    TooOld,
}

/// Tracks recently seen sequence numbers for one flow.
#[derive(Debug, Clone, Default)]
pub struct Dedupe {
    highest: Option<u16>,
    /// Bit i set = sequence number `highest - i` has been seen.
    seen: [u64; 4],
}

impl Dedupe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `seq` and says whether this copy should be used.
    pub fn check(&mut self, seq: u16) -> Verdict {
        let Some(highest) = self.highest else {
            self.highest = Some(seq);
            self.seen = [1, 0, 0, 0];
            return Verdict::New;
        };
        // Signed distance on the 16-bit circle.
        let ahead = seq.wrapping_sub(highest) as i16;
        if ahead > 0 {
            self.shift(ahead as u16);
            self.highest = Some(seq);
            self.seen[0] |= 1;
            return Verdict::New;
        }
        let age = ahead.unsigned_abs();
        if age >= WINDOW {
            return Verdict::TooOld;
        }
        let (word, bit) = (usize::from(age / 64), age % 64);
        if self.seen[word] & (1 << bit) != 0 {
            Verdict::Duplicate
        } else {
            self.seen[word] |= 1 << bit;
            Verdict::New
        }
    }

    /// Shifts the window towards older sequence numbers by `n`.
    fn shift(&mut self, n: u16) {
        if n >= WINDOW {
            self.seen = [0; 4];
            return;
        }
        let (words, bits) = (usize::from(n / 64), u32::from(n % 64));
        for i in (0..4usize).rev() {
            let src = i.checked_sub(words);
            let hi = src.map_or(0, |s| self.seen[s] << bits);
            let lo = match (src.and_then(|s| s.checked_sub(1)), bits) {
                (Some(s), b) if b > 0 => self.seen[s] >> (64 - b),
                _ => 0,
            };
            self.seen[i] = hi | lo;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Verdict::*;

    #[test]
    fn keeps_first_copy_from_either_path() {
        let mut d = Dedupe::new();
        assert_eq!(d.check(10), New);
        assert_eq!(d.check(10), Duplicate); // path B copy
        assert_eq!(d.check(12), New);
        assert_eq!(d.check(11), New); // reordered, still first copy
        assert_eq!(d.check(11), Duplicate);
        assert_eq!(d.check(12), Duplicate);
    }

    #[test]
    fn wraps_around_65535() {
        let mut d = Dedupe::new();
        assert_eq!(d.check(65_534), New);
        assert_eq!(d.check(65_535), New);
        assert_eq!(d.check(0), New);
        assert_eq!(d.check(65_535), Duplicate);
        assert_eq!(d.check(1), New);
        assert_eq!(d.check(0), Duplicate);
    }

    #[test]
    fn window_edges() {
        let mut d = Dedupe::new();
        assert_eq!(d.check(1000), New);
        assert_eq!(d.check(1000 + 255), New);
        assert_eq!(d.check(1000), Duplicate, "age 255 is inside the window");
        assert_eq!(d.check(1000 + 256), New);
        assert_eq!(d.check(1000), TooOld, "age 256 is outside the window");
    }

    #[test]
    fn shifting_across_word_boundaries_keeps_history() {
        let mut d = Dedupe::new();
        for s in 0..200u16 {
            assert_eq!(d.check(s), New);
        }
        for step in [1u16, 63, 64, 65, 100] {
            let mut d2 = d.clone();
            let top = 199 + step;
            assert_eq!(d2.check(top), New);
            for s in 0..200u16 {
                let expect = if top - s >= WINDOW { TooOld } else { Duplicate };
                assert_eq!(d2.check(s), expect, "step {step}, seq {s}");
            }
        }
        d.check(199 + 300);
        assert_eq!(d.check(400), New, "a jump past the window clears history");
    }
}
