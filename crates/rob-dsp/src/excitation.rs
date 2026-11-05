//! Excitation for measurement (design 33 phase 1): a log-swept sine, generated **offline**
//! into a buffer so the audio thread only ever reads it (invariant 6 — no allocation on the
//! audio path).
//!
//! An exponential sine sweep is used rather than noise because a sweep's deconvolution is a
//! single impulse response per capture channel: the harmonics it generates land *before* the
//! IR, so they can be separated from it instead of being averaged into it.
//!
//! **The inverse filter is not here.** It is a property of the analysis, not of the sound
//! played, and `rob_lab` builds it from this sweep's own spectrum. The first version of this
//! module constructed it here from the analytic envelope of a log sweep, and it was wrong twice
//! over: the envelope had the reciprocal slope (**58.5 dB** of tilt across 20 Hz–20 kHz), and
//! even corrected it left **0.9 dB** of residual at the bottom of a loopback, because an analytic
//! envelope only approximates the sweep's real spectrum. Exact inversion of the same sweep
//! measures **0.001 dB** (design 33 phase 1, 2026-10-30).
//!
//! The method is the standard swept-sine form. It is **a method, not a standard**: no AES, IEC
//! or ISO document is claimed by this module (design 33 §2c).

/// One log-swept sine, ready to play.
///
/// `len` samples at `fs`, sweeping from `f1` to `f2` Hz. The buffer is generated once and
/// owned by the caller; playing it allocates nothing.
use crate::prelude::*;

#[derive(Debug, Clone, PartialEq)]
pub struct Sweep {
    /// The samples to play, in order, at unit amplitude (`±1.0`).
    pub samples: Vec<f32>,
    /// `f1` and `f2` — kept so a report can say what was swept rather than assuming.
    pub f1: f32,
    pub f2: f32,
}

impl Sweep {
    /// Generates the sweep. `f1 < f2 < fs/2` is enforced: a sweep above Nyquist aliases, and
    /// an aliased measurement is worse than no measurement.
    pub fn new(len: usize, f1: f32, f2: f32, fs: f32) -> Result<Sweep, String> {
        if len < 64 {
            return Err(format!(
                "a sweep needs at least 64 samples to have any resolution (asked for {len})"
            ));
        }
        if !(fs.is_finite() && fs > 0.0) {
            return Err(format!("sample rate {fs} is not usable"));
        }
        if !(f1 >= 1.0 && f2 > f1 && f2 < fs / 2.0) {
            return Err(format!(
                "a sweep must run from above 1 Hz to below Nyquist ({:.1} Hz here); asked {f1}..{f2}",
                fs / 2.0
            ));
        }
        // The phase is integrated in **f64**, though the buffer is f32. It is not a
        // micro-optimisation: a 5 s sweep ends at about 80,000 radians, where an f32 ulp is
        // 0.008 rad, and the sweep is then a different signal from the one the inverse was
        // built for. Measured 2026-10-30: a 2 s loopback read 1.8 dB of deviation, a 5 s one
        // 7.6 dB and a 10 s one 24 dB, all of it from this. In f64 the phase is exact to
        // 1e-11 rad at the same lengths.
        let k = (f64::from(f2) / f64::from(f1)).ln();
        let t = len as f64 / f64::from(fs);
        let f1d = f64::from(f1);
        // Instantaneous frequency f(t) = f1 e^(kt/T): the phase is its integral.
        let mut samples = Vec::with_capacity(len);
        for n in 0..len {
            let tau = n as f64 / f64::from(fs);
            // phase = 2π f1 T / k (e^(k tau/T) − 1)
            let phase = 2.0 * core::f64::consts::PI * f1d * t / k * ((k * tau / t).exp() - 1.0);
            samples.push(phase.sin() as f32);
        }
        Ok(Sweep { samples, f1, f2 })
    }

    /// The sweep's duration in seconds.
    pub fn seconds(&self, fs: f32) -> f32 {
        self.samples.len() as f32 / fs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sweep must start at `f1` and end at `f2`: measured by counting zero crossings in
    /// the first and last tenth, which is what tells a frequency apart without an FFT.
    #[test]
    fn a_sweep_starts_and_ends_at_the_frequencies_it_was_asked_for() {
        let (fs, len) = (48_000.0, 96_000); // 2 s
        let s = Sweep::new(len, 20.0, 20_000.0, fs).expect("a valid sweep");
        assert_eq!(s.samples.len(), len);

        let crossings = |a: &[f32]| a.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        let tenth = len / 10;
        let f_first = crossings(&s.samples[..tenth]) as f32 / (tenth as f32 / fs);
        let f_last = crossings(&s.samples[len - tenth..]) as f32 / (tenth as f32 / fs);

        // A tenth of a sweep spans a frequency band, so compare to the band's midpoint, not
        // to the endpoints exactly: e^(k/10) is the ratio across one tenth.
        let r = (s.f2 / s.f1).powf(0.1);
        let expect_first = (s.f1 * r.sqrt()).max(2.0);
        let expect_last = (s.f2 / r.sqrt()).min(fs / 2.0 - 1.0);
        assert!(
            (f_first - expect_first).abs() / expect_first < 0.10,
            "the first tenth should measure ~{expect_first:.0} Hz, got {f_first:.0}"
        );
        assert!(
            (f_last - expect_last).abs() / expect_last < 0.10,
            "the last tenth should measure ~{expect_last:.0} Hz, got {f_last:.0}"
        );
    }

    /// The buffer is played as audio, so it must stay inside ±1: a sweep that clips is a
    /// measurement of the clipper.
    #[test]
    fn a_sweep_stays_inside_unit_amplitude() {
        let s = Sweep::new(48_000, 10.0, 22_000.0, 48_000.0).expect("a valid sweep");
        let peak = s.samples.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak <= 1.0, "the sweep reaches {peak}");
        assert!(
            peak > 0.95,
            "the sweep never gets near unit amplitude ({peak})"
        );
    }

    #[test]
    fn an_impossible_sweep_is_refused_with_the_reason() {
        let e = Sweep::new(1024, 20.0, 30_000.0, 48_000.0).expect_err("above Nyquist");
        assert!(e.contains("Nyquist"), "{e}");
        let e = Sweep::new(16, 20.0, 1000.0, 48_000.0).expect_err("too short");
        assert!(e.contains("64"), "{e}");
        let e = Sweep::new(1024, 1000.0, 500.0, 48_000.0).expect_err("f2 below f1");
        assert!(e.contains("Nyquist"), "{e}");
    }
}
