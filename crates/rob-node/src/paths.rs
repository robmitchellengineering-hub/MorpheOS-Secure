//! Per-path delivery statistics for multipath flows (network thread).
//!
//! When a sender transmits every packet over two or more independent paths
//! (two radios, two access points, Wi-Fi plus a wired or 60 GHz link), the
//! receiver keeps the first copy of each packet. This module measures what that
//! buys: for every path, the delay from "period complete" to arrival (on
//! network time, so it is the true one-way delay plus sender buffering), how
//! many packets the path lost, and how often its copy arrived first; and the
//! same delay for the first copy across all paths, which is what the jitter
//! buffer actually sees. The latency a receiver needs is set by the high
//! percentiles of that first-copy delay.
//!
//! A packet is settled (its losses counted) once the sequence number `RING`
//! packets later arrives, so loss figures lag by about one second at 2 ms
//! periods. No allocation after [`PathMonitor::new`].

/// Paths told apart (more are ignored).
use crate::prelude::*;

pub const MAX_PATHS: usize = 4;
/// Histogram resolution and range: 0.1 ms buckets up to 100 ms, the last
/// bucket holding everything later.
pub const BUCKET_NS: u64 = 100_000;
pub const BUCKETS: usize = 1_000;
/// Packets in flight that can be matched across paths.
const RING: usize = 512;

/// Delay histogram.
#[derive(Clone)]
pub struct Histogram {
    counts: [u32; BUCKETS],
    total: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Histogram {
            counts: [0; BUCKETS],
            total: 0,
        }
    }
}

impl Histogram {
    /// Records one delay (negative delays, from clock error, count as zero).
    pub fn add(&mut self, delay_ns: i64) {
        let b = (delay_ns.max(0) as u64 / BUCKET_NS).min(BUCKETS as u64 - 1) as usize;
        self.counts[b] = self.counts[b].saturating_add(1);
        self.total += 1;
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    /// Delay (ms, the upper edge of its bucket) at or below which a fraction
    /// `q` of the samples lie. `None` when empty.
    pub fn quantile_ms(&self, q: f64) -> Option<f64> {
        if self.total == 0 {
            return None;
        }
        let want = ((q * self.total as f64).ceil() as u64).clamp(1, self.total);
        let mut seen = 0u64;
        for (b, &c) in self.counts.iter().enumerate() {
            seen += u64::from(c);
            if seen >= want {
                return Some((b + 1) as f64 * BUCKET_NS as f64 / 1e6);
            }
        }
        Some(BUCKETS as f64 * BUCKET_NS as f64 / 1e6)
    }

    /// The smallest sample (ms, the lower edge of its bucket: never more than
    /// the true value). `None` when empty.
    pub fn min_ms(&self) -> Option<f64> {
        let b = self.counts.iter().position(|&c| c > 0)?;
        Some(b as f64 * BUCKET_NS as f64 / 1e6)
    }

    /// Samples later than `ns` (to bucket resolution).
    pub fn later_than(&self, ns: u64) -> u64 {
        let from = ((ns / BUCKET_NS) as usize).min(BUCKETS);
        self.counts[from..].iter().map(|&c| u64::from(c)).sum()
    }

    pub fn clear(&mut self) {
        self.counts = [0; BUCKETS];
        self.total = 0;
    }
}

/// What one path delivered.
#[derive(Clone, Default)]
pub struct PathTotals {
    /// Copies received (first copies and duplicates).
    pub packets: u64,
    /// Settled packets this path never delivered.
    pub missed: u64,
    /// Packets whose first copy came over this path.
    pub first: u64,
    /// Copies older than anything still matchable (very late).
    pub stale: u64,
    pub delay: Histogram,
}

#[derive(Clone, Copy, Default)]
struct Slot {
    used: bool,
    seq: u16,
    /// Bit p: path p delivered a copy.
    seen: u8,
}

pub struct PathMonitor {
    slots: Box<[Slot]>,
    /// Paths heard so far (indices below this are counted for losses).
    active: usize,
    pub paths: [PathTotals; MAX_PATHS],
    /// Delay of the first copy of every packet, over all paths.
    pub first_copy: Histogram,
    /// Packets settled (seen on at least one path).
    pub settled: u64,
}

impl Default for PathMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl PathMonitor {
    pub fn new() -> PathMonitor {
        PathMonitor {
            slots: vec![Slot::default(); RING].into_boxed_slice(),
            active: 0,
            paths: Default::default(),
            first_copy: Histogram::default(),
            settled: 0,
        }
    }

    /// Paths heard so far.
    pub fn active(&self) -> usize {
        self.active
    }

    /// A copy of packet `seq` arrived over `path`, `delay_ns` after its newest
    /// period was complete (network time).
    pub fn observe(&mut self, path: usize, seq: u16, delay_ns: i64) {
        if path >= MAX_PATHS {
            return;
        }
        self.active = self.active.max(path + 1);
        let i = usize::from(seq) % RING;
        let s = self.slots[i];
        if s.used && s.seq != seq {
            if (seq.wrapping_sub(s.seq) as i16) < 0 {
                // Older than the packet now in this slot: far too late to match.
                self.paths[path].stale += 1;
                self.paths[path].packets += 1;
                return;
            }
            self.settle(i);
        }
        let s = &mut self.slots[i];
        if !s.used {
            *s = Slot {
                used: true,
                seq,
                seen: 0,
            };
        }
        let bit = 1u8 << path;
        if s.seen & bit != 0 {
            return; // the same path delivered it twice: not a second path
        }
        let first = s.seen == 0;
        s.seen |= bit;
        let p = &mut self.paths[path];
        p.packets += 1;
        p.delay.add(delay_ns);
        if first {
            p.first += 1;
            self.first_copy.add(delay_ns);
        }
    }

    fn settle(&mut self, i: usize) {
        let s = self.slots[i];
        for (p, t) in self.paths.iter_mut().enumerate().take(self.active) {
            if s.seen & (1 << p) == 0 {
                t.missed += 1;
            }
        }
        self.settled += 1;
        self.slots[i].used = false;
    }

    /// Starts a new reporting interval (keeps the packets still in flight).
    pub fn clear_totals(&mut self) {
        for p in &mut self.paths {
            *p = PathTotals::default();
        }
        self.first_copy.clear();
        self.settled = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;

    #[test]
    fn histogram_quantiles() {
        let mut h = Histogram::default();
        assert_eq!(h.quantile_ms(0.5), None);
        for k in 0..1_000 {
            h.add(k * 10_000); // 0 .. 10 ms evenly
        }
        h.add(500 * MS); // one very late packet lands in the last bucket
        assert!((h.quantile_ms(0.5).unwrap() - 5.0).abs() < 0.11);
        assert!((h.quantile_ms(0.99).unwrap() - 9.9).abs() < 0.11);
        assert_eq!(h.quantile_ms(1.0), Some(100.0));
        assert_eq!(h.later_than(10 * MS as u64), 1);
        assert_eq!(h.min_ms(), Some(0.0));
        let mut g = Histogram::default();
        assert_eq!(g.min_ms(), None);
        g.add(2_345_000);
        g.add(7 * MS);
        assert!(
            (g.min_ms().unwrap() - 2.3).abs() < 1e-9,
            "lower edge, never above the truth"
        );
        h.add(-3 * MS);
        assert_eq!(h.quantile_ms(0.0), Some(0.1), "negative counts as zero");
    }

    #[test]
    fn first_copy_takes_the_better_path_packet_by_packet() {
        let mut m = PathMonitor::new();
        // Path 0 is usually faster but has a 20 ms burst every 50 packets;
        // path 1 is a steady 4 ms; path 0 also loses every 100th packet.
        for k in 0..10_000u32 {
            let seq = k as u16;
            let a = if k % 50 < 3 { 20 * MS } else { 2 * MS };
            let lost_a = k % 100 == 99;
            if a < 4 * MS {
                if !lost_a {
                    m.observe(0, seq, a);
                }
                m.observe(1, seq, 4 * MS);
            } else {
                m.observe(1, seq, 4 * MS);
                if !lost_a {
                    m.observe(0, seq, a);
                }
            }
        }
        let (a, b) = (&m.paths[0], &m.paths[1]);
        assert_eq!(a.packets, 9_900);
        assert_eq!(b.packets, 10_000);
        // Settled up to 512 packets back: path 0 missed one in a hundred.
        assert!(m.settled >= 10_000 - 512);
        let want = m.settled / 100;
        assert!(
            a.missed.abs_diff(want) <= 1,
            "missed {} want {want}",
            a.missed
        );
        assert_eq!(b.missed, 0);
        assert_eq!(a.first + b.first, 10_000);
        assert_eq!(b.first, 10_000 / 50 * 3 + 100);
        // Alone, path 0 has a 20 ms tail; the first copy never exceeds 4 ms.
        assert!(a.delay.quantile_ms(0.999).unwrap() > 19.0);
        assert!(m.first_copy.quantile_ms(1.0).unwrap() <= 4.1);
        assert_eq!(m.first_copy.total(), 10_000);
    }

    #[test]
    fn duplicates_on_one_path_and_stale_copies_are_not_double_counted() {
        let mut m = PathMonitor::new();
        m.observe(0, 10, MS);
        m.observe(0, 10, MS); // the same path repeating itself
        assert_eq!(m.paths[0].packets, 1);
        assert_eq!(m.first_copy.total(), 1);
        m.observe(1, 10 + RING as u16, MS); // evicts seq 10
        assert_eq!(m.settled, 1);
        assert_eq!(m.paths[1].missed, 1, "path 1 never sent seq 10");
        m.observe(1, 10, 90 * MS); // arrives after it was settled
        assert_eq!(m.paths[1].stale, 1);
        assert_eq!(m.first_copy.total(), 2);
    }

    #[test]
    fn sequence_wrap() {
        let mut m = PathMonitor::new();
        for k in 0..70_000u32 {
            m.observe(0, k as u16, MS);
            m.observe(1, k as u16, 2 * MS);
        }
        assert_eq!(m.paths[0].missed + m.paths[1].missed, 0);
        assert_eq!(m.paths[0].first, 70_000);
        assert_eq!(m.paths[0].stale + m.paths[1].stale, 0);
    }
}
