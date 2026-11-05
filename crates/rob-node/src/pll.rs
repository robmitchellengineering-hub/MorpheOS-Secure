//! The position tracker behind every asynchronous resampler: a second-order
//! (proportional-integral) phase-locked loop.
//!
//! Each block, the caller says where the block's first output sample *should*
//! read in the input (`target`, in input samples). Targets come from clocks and
//! sound-card timestamps, so they are noisy. The tracker keeps its own smooth
//! read position and rate (input samples per output sample), and nudges them
//! towards the targets:
//!
//! ```text
//! e     = target − position
//! freq += Ki·e / n            (integral: learns the clocks' rate difference)
//! step  = nominal + freq + Kp·e / n
//! ```
//!
//! A caller that moves its targets on purpose (a receiver changing its latency)
//! says so with [`Tracker::set_feed_forward`]: the known rate of change is
//! added to the step directly, so the loop has no error to chase and the read
//! position follows the planned ramp exactly, starts and stops included.
//!
//! Gains come from a loop bandwidth: wide (1 Hz) for the first seconds to acquire
//! quickly, then narrow (0.1 Hz). An error beyond `resync_samples` is not tracked
//! but jumped (a resync), which the caller should hide with a short fade.
//!
//! **The error is low-pass filtered before the loop sees it** (two poles at ten
//! times the loop bandwidth). Without that, the proportional path passes target
//! jitter straight into the playback rate: in testing, +-10 samples of timestamp
//! jitter moved the rate by 370 ppm from one 10 ms block to the next, which is
//! flutter, and on a 10 kHz tone phase-modulation noise far above broadcast
//! tolerance. Filtering costs a little phase margin (the filter sits a decade
//! above the loop) and removes that path.

use crate::prelude::*;

use core::f64::consts::TAU;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackerConfig {
    /// Input samples per output sample when both clocks are perfect.
    pub nominal: f64,
    /// Output sample rate, to turn block lengths into time.
    pub output_rate: f64,
    pub acquire_bw_hz: f64,
    pub track_bw_hz: f64,
    pub acquire_s: f64,
    pub damping: f64,
    /// Largest allowed rate deviation from nominal (fraction; 2e-3 = 2,000 ppm).
    pub max_dev: f64,
    /// Errors beyond this many input samples are jumped, not tracked.
    pub resync_samples: f64,
    /// Error filter cutoff as a multiple of the current loop bandwidth.
    pub error_filter_ratio: f64,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        TrackerConfig {
            nominal: 1.0,
            output_rate: 48_000.0,
            acquire_bw_hz: 1.0,
            track_bw_hz: 0.1,
            acquire_s: 3.0,
            damping: core::f64::consts::FRAC_1_SQRT_2,
            max_dev: 2e-3,
            resync_samples: 480.0,
            error_filter_ratio: 10.0,
        }
    }
}

/// How to read one block of output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    /// Input position of the block's first output sample.
    pub start: f64,
    /// Input samples per output sample for this block.
    pub step: f64,
    /// The position jumped to the target (hide it with a fade).
    pub resynced: bool,
}

#[derive(Debug, Clone)]
pub struct Tracker {
    cfg: TrackerConfig,
    pos: f64,
    freq: f64,
    started: bool,
    since_sync_s: f64,
    last_error: f64,
    /// Two cascaded one-pole low-pass stages on the phase error.
    ef1: f64,
    ef2: f64,
    resyncs: u64,
    /// Planned rate of change of the targets (input samples per output sample).
    feed_forward: f64,
}

impl Tracker {
    pub fn new(cfg: TrackerConfig) -> Tracker {
        Tracker {
            cfg,
            pos: 0.0,
            freq: 0.0,
            started: false,
            since_sync_s: 0.0,
            last_error: 0.0,
            ef1: 0.0,
            ef2: 0.0,
            resyncs: 0,
            feed_forward: 0.0,
        }
    }

    /// Input samples per output sample added to the step from the next block
    /// on: how fast the caller is deliberately moving its targets (0: not).
    /// Limited to +-1,000 ppm, so with the loop's own +-2,000 ppm the step
    /// stays within what `Asrc::input_needed` provides for.
    pub fn set_feed_forward(&mut self, ff: f64) {
        self.feed_forward = ff.clamp(-1e-3, 1e-3);
    }

    /// Plans a block of `n` output samples. `target` is where its first sample
    /// should read; `None` free-runs on the learned rate (holdover).
    pub fn plan(&mut self, n: usize, target: Option<f64>) -> Plan {
        let nominal = self.cfg.nominal;
        let limit = self.cfg.max_dev * nominal;
        let mut resynced = false;
        let mut step = nominal + self.freq;
        match target {
            Some(t) if !self.started || (t - self.pos).abs() > self.cfg.resync_samples => {
                self.pos = t;
                self.started = true;
                self.since_sync_s = 0.0;
                self.last_error = 0.0;
                self.ef1 = 0.0;
                self.ef2 = 0.0;
                self.resyncs += 1;
                resynced = true;
                step = nominal + self.freq;
            }
            Some(t) => {
                let e = t - self.pos;
                self.last_error = e;
                let block_s = n as f64 / self.cfg.output_rate;
                let bw = if self.since_sync_s < self.cfg.acquire_s {
                    self.cfg.acquire_bw_hz
                } else {
                    self.cfg.track_bw_hz
                };
                let a = 1.0 - (-TAU * bw * self.cfg.error_filter_ratio * block_s).exp();
                self.ef1 += a * (e - self.ef1);
                self.ef2 += a * (self.ef1 - self.ef2);
                let ef = self.ef2;
                let wn_t = TAU * bw * block_s;
                let kp = 2.0 * self.cfg.damping * wn_t;
                let ki = wn_t * wn_t;
                self.freq = (self.freq + ki * ef / n as f64).clamp(-limit, limit);
                step = nominal + (self.freq + kp * ef / n as f64).clamp(-limit, limit);
            }
            None => {}
        }
        let step = step + self.feed_forward;
        let plan = Plan {
            start: self.pos,
            step,
            resynced,
        };
        self.pos += step * n as f64;
        self.since_sync_s += n as f64 / self.cfg.output_rate;
        plan
    }

    /// Latest phase error (input samples), before correction.
    pub fn last_error(&self) -> f64 {
        self.last_error
    }

    /// The learned rate deviation, in ppm of nominal.
    pub fn freq_ppm(&self) -> f64 {
        self.freq / self.cfg.nominal * 1e6
    }

    pub fn resyncs(&self) -> u64 {
        self.resyncs
    }

    /// Where the next block will start reading.
    pub fn position(&self) -> f64 {
        self.pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_forward_follows_a_planned_ramp_without_lag() {
        // Targets ramp at -1,000 ppm for 3 s (a receiver raising its latency),
        // then stop. Told the ramp, the tracker stays on it; not told, it lags
        // by tens of samples (the loop is only 0.1 Hz wide).
        let lag = |told: bool| {
            let mut t = Tracker::new(TrackerConfig::default());
            let n = 64;
            let mut target = 10_000.0;
            let mut worst: f64 = 0.0;
            for b in 0..48_000 * 12 / n {
                let s = b as f64 * n as f64 / 48_000.0;
                let ramp = if (6.0..9.0).contains(&s) { -1e-3 } else { 0.0 };
                t.set_feed_forward(if told { ramp } else { 0.0 });
                let p = t.plan(n, Some(target));
                if s > 5.0 {
                    worst = worst.max((target - p.start).abs());
                }
                target += n as f64 * (1.0 + ramp);
            }
            worst
        };
        let (told, blind) = (lag(true), lag(false));
        assert!(told < 0.01, "told the ramp: {told} samples off");
        assert!(blind > 20.0, "not told: {blind} samples off");
    }

    /// Deterministic noise in [-1, 1).
    struct Noise(u64);
    impl Noise {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 52) as f64 - 1.0
        }
    }

    /// The input runs `ppm` fast relative to output; targets carry `jitter`
    /// samples of uniform noise. Returns (true − tracked) for every block, the
    /// largest block-to-block step change (ppm), and the tracker.
    fn run(ppm: f64, jitter: f64, seconds: f64) -> (Vec<f64>, f64, Tracker) {
        let mut t = Tracker::new(TrackerConfig::default());
        let mut noise = Noise(0x9E37_79B9);
        let n = 480;
        let rate = 1.0 + ppm * 1e-6;
        let blocks = (seconds * 100.0) as usize;
        let mut errs = Vec::new();
        let mut last_step: Option<f64> = None;
        let mut worst_change: f64 = 0.0;
        for b in 0..blocks {
            let truth = 10_000.0 + (b * n) as f64 * rate;
            let p = t.plan(n, Some(truth + jitter * noise.next()));
            errs.push(truth - p.start);
            if let Some(ls) = last_step {
                if b > 400 {
                    worst_change = worst_change.max((p.step - ls).abs() * 1e6);
                }
            }
            last_step = Some(p.step);
        }
        (errs, worst_change, t)
    }

    fn p99(v: &[f64]) -> f64 {
        let mut a: Vec<f64> = v.iter().map(|x| x.abs()).collect();
        a.sort_by(f64::total_cmp);
        a[(a.len() - 1) * 99 / 100]
    }

    #[test]
    fn locks_to_a_rate_offset_through_harsh_jitter() {
        // 73 ppm between two crystals, targets jittered by +-10 samples (+-210 us,
        // far worse than real sound-card timestamps). A 0.1 Hz loop averages over
        // about 3 s; theory gives a p99 of about 1.2 samples here, and that is
        // what it measures. 1.5 samples is 31 us: inside the sync budget.
        let (errs, _, t) = run(73.0, 10.0, 60.0);
        // The rate estimate wanders slowly with the filtered noise (2.5 ppm seen,
        // about 0.004 cents of pitch: inaudible). Position error is what matters.
        assert!((t.freq_ppm() - 73.0).abs() < 5.0, "{} ppm", t.freq_ppm());
        let settled = p99(&errs[3_000..]);
        assert!(settled < 1.5, "p99 error {settled} samples after 30 s");
        assert_eq!(t.resyncs(), 1, "only the initial sync");
    }

    #[test]
    fn realistic_jitter_gives_a_tiny_position_error() {
        // +-1 sample (+-21 us) of timestamp jitter: ten times tighter.
        let (errs, _, t) = run(-41.0, 1.0, 60.0);
        let settled = p99(&errs[3_000..]);
        assert!(settled < 0.2, "p99 error {settled} samples");
        assert!((t.freq_ppm() + 41.0).abs() < 1.0, "{} ppm", t.freq_ppm());
    }

    #[test]
    fn jitter_barely_moves_the_rate_once_tracking() {
        // The point of the narrow bandwidth: pitch must not wobble with jitter.
        let (_, worst_change, _) = run(0.0, 10.0, 30.0);
        assert!(
            worst_change < 50.0,
            "rate moved {worst_change} ppm between blocks"
        );
    }

    #[test]
    fn follows_a_drifting_rate() {
        let mut t = Tracker::new(TrackerConfig::default());
        let n = 480;
        let mut pos_true = 0.0;
        let mut worst: f64 = 0.0;
        for b in 0..12_000usize {
            // Rate offset ramps from +20 to +80 ppm over two minutes.
            let ppm = 20.0 + 60.0 * (b as f64 / 12_000.0);
            let p = t.plan(n, Some(pos_true));
            if b > 1_000 {
                worst = worst.max((pos_true - p.start).abs());
            }
            pos_true += n as f64 * (1.0 + ppm * 1e-6);
        }
        assert!(worst < 1.0, "lagged {worst} samples behind a ramping rate");
    }

    #[test]
    fn a_jump_beyond_the_threshold_resyncs_instead_of_slewing() {
        let mut t = Tracker::new(TrackerConfig::default());
        for b in 0..500usize {
            t.plan(480, Some((b * 480) as f64));
        }
        let p = t.plan(480, Some((500 * 480) as f64 + 2_000.0));
        assert!(p.resynced);
        assert_eq!(p.start, (500 * 480) as f64 + 2_000.0);
        assert_eq!(t.resyncs(), 2);
    }

    #[test]
    fn holdover_keeps_the_learned_rate() {
        let mut t = Tracker::new(TrackerConfig::default());
        let rate = 1.0 + 50e-6;
        for b in 0..6_000usize {
            t.plan(480, Some(b as f64 * 480.0 * rate));
        }
        // Ten seconds of no targets: free-run on the learned rate.
        let mut last = t.plan(480, None);
        for _ in 0..999 {
            last = t.plan(480, None);
        }
        let truth = 6_999.0 * 480.0 * rate;
        assert!(
            (truth - last.start).abs() < 5.0,
            "drifted {} samples",
            truth - last.start
        );
    }
}
