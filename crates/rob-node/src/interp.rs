//! Band-limited fractional interpolation: reading a sampled signal *between* its
//! samples. Every asynchronous resampler in the node (sound card ↔ network time)
//! is this plus a position tracker.
//!
//! A Kaiser-windowed sinc kernel is tabulated at `phases` fractional offsets;
//! a read at position `p` blends the two nearest tabulated phases. The table is
//! built once, at start-up, never on the audio thread.
//!
//! **Latency.** The kernel is symmetric, so reading at position `p` needs input up
//! to `p + taps/2`. That half-window is pure latency on every resampling stage:
//! 64 taps = 32 samples = 0.67 ms at 48 kHz. Fewer taps for the lowest-latency
//! tiers, more for transparency; E7 chooses (spec: Open questions).

/// Interpolation quality presets, by kernel length.
use crate::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// 32 taps: 0.33 ms latency per stage. For the lowest-latency wired tiers.
    Low,
    /// 64 taps: 0.67 ms. The default.
    Standard,
    /// 128 taps: 1.33 ms. For transparency where latency allows.
    High,
}

impl Quality {
    pub fn taps(self) -> usize {
        match self {
            Quality::Low => 32,
            Quality::Standard => 64,
            Quality::High => 128,
        }
    }

    /// Kaiser beta: longer kernels can afford deeper stopbands.
    fn beta(self) -> f64 {
        match self {
            Quality::Low => 8.0,
            Quality::Standard => 10.0,
            Quality::High => 12.0,
        }
    }
}

/// Modified Bessel function of the first kind, order 0 (series form).
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    let q = x * x / 4.0;
    while term > sum * 1e-17 {
        term *= q / (k * k);
        sum += term;
        k += 1.0;
    }
    sum
}

/// A tabulated interpolation kernel.
pub struct Interpolator {
    taps: usize,
    phases: usize,
    /// `(phases + 1) × taps` coefficients; row `m` is the kernel for fractional
    /// offset `m / phases`, so row `phases` (offset 1.0) closes the interpolation.
    table: Vec<f32>,
}

impl Interpolator {
    /// Builds the table. `cutoff` is the passband edge as a fraction of the
    /// sample rate (0.5 = Nyquist); 0.46 keeps 20 kHz at 48 kHz with margin.
    pub fn new(quality: Quality) -> Interpolator {
        Self::with(quality.taps(), 1024, quality.beta(), 0.46)
    }

    pub fn with(taps: usize, phases: usize, beta: f64, cutoff: f64) -> Interpolator {
        assert!(taps >= 4 && taps % 2 == 0, "taps must be even");
        let half = (taps / 2) as f64;
        let norm = bessel_i0(beta);
        let mut table = Vec::with_capacity((phases + 1) * taps);
        for m in 0..=phases {
            let f = m as f64 / phases as f64;
            let mut row = Vec::with_capacity(taps);
            for k in 0..taps {
                // Distance from the read position to this tap's input sample.
                let d = (k as f64 - half + 1.0) - f;
                let x = 2.0 * cutoff * d;
                let sinc = if x.abs() < 1e-12 {
                    1.0
                } else {
                    (core::f64::consts::PI * x).sin() / (core::f64::consts::PI * x)
                };
                let r = d / half;
                let w = if r.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(beta * (1.0 - r * r).sqrt()) / norm
                };
                row.push(2.0 * cutoff * sinc * w);
            }
            // Unity gain at DC for every phase.
            let sum: f64 = row.iter().sum();
            table.extend(row.iter().map(|c| (c / sum) as f32));
        }
        Interpolator {
            taps,
            phases,
            table,
        }
    }

    pub fn taps(&self) -> usize {
        self.taps
    }

    /// Input samples needed after the read position (the latency, in samples).
    pub fn lookahead(&self) -> usize {
        self.taps / 2
    }

    /// Input samples needed before the read position.
    pub fn history(&self) -> usize {
        self.taps / 2 - 1
    }

    /// Reads the signal at `frac` (0 ≤ frac < 1) past `window[history()]`.
    ///
    /// `window` must hold exactly `taps()` samples, starting `history()` samples
    /// before the integer part of the read position. No allocation.
    #[inline]
    pub fn read(&self, window: &[f32], frac: f64) -> f32 {
        debug_assert_eq!(window.len(), self.taps);
        let pos = frac.clamp(0.0, 1.0) * self.phases as f64;
        let m = (pos as usize).min(self.phases - 1);
        let t = (pos - m as f64) as f32;
        let a = &self.table[m * self.taps..(m + 1) * self.taps];
        let b = &self.table[(m + 1) * self.taps..(m + 2) * self.taps];
        let (mut ya, mut yb) = (0.0f32, 0.0f32);
        for ((x, ca), cb) in window.iter().zip(a).zip(b) {
            ya += x * ca;
            yb += x * cb;
        }
        ya + t * (yb - ya)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resamples a sine at a non-trivial ratio and returns the SNR (dB) against
    /// the exact sine at the read positions.
    fn snr_db(q: Quality, hz: f64, ratio: f64) -> f64 {
        let it = Interpolator::new(q);
        let fs = 48_000.0;
        let n_in = 20_000;
        let x: Vec<f32> = (0..n_in)
            .map(|i| (0.5 * (core::f64::consts::TAU * hz * i as f64 / fs).sin()) as f32)
            .collect();
        let (mut sig, mut err) = (0.0f64, 0.0f64);
        let mut p = 1_000.25_f64; // start well inside the data
        while p < (n_in - 1_000) as f64 {
            let i = p.floor() as usize;
            let w = &x[i - it.history()..i - it.history() + it.taps()];
            let y = f64::from(it.read(w, p - p.floor()));
            let truth = 0.5 * (core::f64::consts::TAU * hz * p / fs).sin();
            sig += truth * truth;
            err += (y - truth) * (y - truth);
            p += ratio;
        }
        10.0 * (sig / err).log10()
    }

    #[test]
    fn quality_presets_measured() {
        // Measured, not assumed; these floors are the regression guard.
        for (q, floor_1k, floor_15k) in [
            (Quality::Low, 70.0, 55.0),
            (Quality::Standard, 90.0, 80.0),
            (Quality::High, 105.0, 95.0),
        ] {
            let a = snr_db(q, 997.0, 1.000_123);
            let b = snr_db(q, 15_000.0, 0.999_877);
            assert!(a > floor_1k, "{q:?} at 1 kHz: {a:.1} dB");
            assert!(b > floor_15k, "{q:?} at 15 kHz: {b:.1} dB");
        }
    }

    #[test]
    fn integer_positions_reproduce_in_band_input() {
        // The kernel is a low-pass at 0.46 fs, so only in-band content comes back
        // unchanged; a signal of sines below 18 kHz must, at integer positions.
        let it = Interpolator::new(Quality::Standard);
        let x: Vec<f32> = (0..400)
            .map(|i| {
                let t = i as f64 / 48_000.0;
                let tau = core::f64::consts::TAU;
                (0.3 * (tau * 1_000.0 * t).sin()
                    + 0.2 * (tau * 7_000.0 * t).sin()
                    + 0.1 * (tau * 13_000.0 * t).sin()) as f32
            })
            .collect();
        for i in [100usize, 177, 250] {
            let w = &x[i - it.history()..i - it.history() + it.taps()];
            assert!((it.read(w, 0.0) - x[i]).abs() < 1e-4, "sample {i}");
        }
    }

    #[test]
    fn dc_passes_at_unity_for_every_phase() {
        let it = Interpolator::new(Quality::Low);
        let w = vec![0.25f32; it.taps()];
        for k in 0..50 {
            let y = it.read(&w, k as f64 / 50.0);
            assert!((y - 0.25).abs() < 1e-5, "{y}");
        }
    }

    #[test]
    fn latency_is_half_the_kernel() {
        assert_eq!(Interpolator::new(Quality::Standard).lookahead(), 32);
        assert_eq!(Interpolator::new(Quality::High).history(), 63);
    }

    #[test]
    #[ignore] // `cargo test -p rob-node -- --ignored --nocapture quality_table` prints the table
    fn quality_table() {
        for q in [Quality::Low, Quality::Standard, Quality::High] {
            let row: Vec<String> = [
                100.0, 997.0, 5_000.0, 10_000.0, 15_000.0, 19_000.0, 20_000.0,
            ]
            .iter()
            .map(|&hz| format!("{hz:>6.0} Hz {:>6.1} dB", snr_db(q, hz, 1.000_123)))
            .collect();
            println!("{q:?} ({} taps): {}", q.taps(), row.join(" | "));
        }
    }
}
