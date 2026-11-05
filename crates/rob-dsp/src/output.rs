//! Output-bus processing (design 16): a per-output EQ and limiter, applied
//! between the matrix mix and the output flow, so the PA, in-ears and
//! broadcast mixes leave the engine shaped and protected.
//!
//! The EQ is the channel EQ's biquad set ([`Eq`]). The limiter is a
//! stereo-linked peak limiter in the log domain: it reduces gain the moment
//! a sample exceeds the ceiling (instant attack, so nothing passes the
//! ceiling) and releases smoothly with a one-pole, exactly as the
//! compressor's release does. It reports its current gain reduction for the
//! meter (design 18). EQ runs before the limiter, so a boosted band is caught
//! by the ceiling rather than clipping.
//!
//! Allocation-free while processing; every setting may change at any time.

use crate::prelude::*;

use crate::dynamics::{from_db, to_db};
use crate::filter::{Eq, EqParams, MAX_CH};
use crate::graphic::{GraphicEq, GraphicParams};

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct LimiterParams {
    pub on: bool,
    /// The absolute ceiling (dBFS): nothing leaves louder than this.
    pub ceiling_db: f32,
    /// Release time constant (ms). The attack is instant by design.
    pub release_ms: f32,
}

impl Default for LimiterParams {
    fn default() -> Self {
        LimiterParams {
            on: false,
            ceiling_db: -1.0,
            release_ms: 120.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct OutputParams {
    pub graphic: GraphicParams,
    pub eq: EqParams,
    pub limiter: LimiterParams,
}

#[derive(Debug, Clone)]
pub struct Limiter {
    fs: f64,
    p: LimiterParams,
    rel: f64,
    /// Current gain reduction (dB, <= 0), for the meter.
    gr_db: f64,
}

impl Limiter {
    pub fn new(p: LimiterParams, fs: f64) -> Limiter {
        let mut l = Limiter {
            fs,
            p,
            rel: 0.0,
            gr_db: 0.0,
        };
        l.set(p);
        l
    }

    pub fn set(&mut self, p: LimiterParams) {
        self.p = p;
        self.rel = (-1.0 / (f64::from(p.release_ms.max(0.01)) * 1e-3 * self.fs)).exp();
    }

    pub fn params(&self) -> LimiterParams {
        self.p
    }

    /// Current gain reduction (dB, <= 0), for the meter.
    pub fn gain_reduction_db(&self) -> f32 {
        self.gr_db as f32
    }

    pub fn process(&mut self, chans: &mut [&mut [f32]]) {
        let n = chans.first().map_or(0, |c| c.len());
        let ceiling_db = f64::from(self.p.ceiling_db.min(0.0));
        if !self.p.on && self.gr_db > -0.001 {
            self.gr_db = 0.0;
            return;
        }
        for i in 0..n {
            let mut peak = 0.0f64;
            for buf in chans.iter().take(MAX_CH) {
                peak = peak.max(f64::from(buf[i]).abs());
            }
            let target = if self.p.on {
                (ceiling_db - to_db(peak)).min(0.0)
            } else {
                0.0
            };
            // Instant attack (a sample never passes the ceiling), smooth
            // release (no click when the gain comes back).
            if target < self.gr_db {
                self.gr_db = target;
            } else {
                self.gr_db = self.rel * self.gr_db + (1.0 - self.rel) * target;
            }
            let g = from_db(self.gr_db) as f32;
            for buf in chans.iter_mut().take(MAX_CH) {
                buf[i] *= g;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutputProc {
    p: OutputParams,
    graphic: GraphicEq,
    eq: Eq,
    limiter: Limiter,
}

impl OutputProc {
    /// Allocates nothing after this.
    pub fn new(p: OutputParams, fs: f64) -> OutputProc {
        OutputProc {
            p,
            graphic: GraphicEq::new(p.graphic, fs),
            eq: Eq::new(p.eq, fs),
            limiter: Limiter::new(p.limiter, fs),
        }
    }

    pub fn params(&self) -> OutputParams {
        self.p
    }

    /// New settings, applied click-free (filters glide, the limiter's gain is
    /// already smooth). No allocation.
    pub fn set(&mut self, p: OutputParams) {
        self.p = p;
        self.graphic.set(p.graphic);
        self.eq.set(p.eq);
        self.limiter.set(p.limiter);
    }

    /// Current limiter gain reduction (dB, <= 0), for the meter.
    pub fn limiter_gr_db(&self) -> f32 {
        self.limiter.gain_reduction_db()
    }

    pub fn limiter_on(&self) -> bool {
        self.p.limiter.on
    }

    /// The EQ's curve at `hz` (for the UI).
    pub fn eq_magnitude(&self, hz: f64) -> f64 {
        self.eq.magnitude(hz)
    }

    /// The graphic EQ's curve at `hz` (for the UI).
    pub fn graphic_magnitude(&self, hz: f64) -> f64 {
        self.graphic.magnitude(hz)
    }

    /// Graphic EQ, then parametric EQ, then limiter.
    pub fn process(&mut self, chans: &mut [&mut [f32]]) {
        self.graphic.process(chans);
        self.eq.process(chans);
        self.limiter.process(chans);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{Band, Kind};
    use core::f64::consts::TAU;

    const FS: f64 = 48_000.0;

    fn level_db(x: &[f32]) -> f64 {
        to_db(x.iter().map(|v| f64::from(v.abs())).fold(0.0, f64::max))
    }

    fn limiting(ceiling_db: f32) -> OutputProc {
        OutputProc::new(
            OutputParams {
                limiter: LimiterParams {
                    on: true,
                    ceiling_db,
                    ..LimiterParams::default()
                },
                ..OutputParams::default()
            },
            FS,
        )
    }

    #[test]
    fn a_flat_output_is_transparent() {
        let mut o = OutputProc::new(OutputParams::default(), FS);
        let x: Vec<f32> = (0..4_800)
            .map(|i| ((i * 7919) % 2000) as f32 / 1000.0 - 1.0)
            .collect();
        let mut y = x.clone();
        o.process(&mut [&mut y]);
        let err = x
            .iter()
            .zip(&y)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(err < 1e-6, "{err}");
    }

    #[test]
    fn the_limiter_holds_the_ceiling_and_reports_its_reduction() {
        // A steady 0.9 (about -0.92 dBFS) into a -1 dBFS ceiling: the output
        // settles at the ceiling and the meter reads the exact reduction.
        let mut o = limiting(-1.0);
        let mut x = vec![0.9f32; 480];
        o.process(&mut [&mut x]);
        assert!((level_db(&x) + 1.0).abs() < 0.01, "{}", level_db(&x));
        let want = -1.0 - 20.0 * 0.9f64.log10();
        assert!(
            (f64::from(o.limiter_gr_db()) - want).abs() < 0.01,
            "GR {} vs {want}",
            o.limiter_gr_db()
        );
        // Below the ceiling: untouched, and the meter reads no reduction.
        let mut o = limiting(-1.0);
        let mut q = vec![0.3f32; 480];
        o.process(&mut [&mut q]);
        assert!((level_db(&q) - 20.0 * 0.3f64.log10()).abs() < 0.01);
        assert!(o.limiter_gr_db() > -0.01, "{}", o.limiter_gr_db());
        // A full-scale step never passes the ceiling.
        let mut o = limiting(-3.0);
        let mut s = vec![0.0f32; 480];
        s[0] = 1.0;
        o.process(&mut [&mut s]);
        let ceiling = 10f64.powf(-3.0 / 20.0) as f32;
        let max = s.iter().cloned().fold(0.0f32, f32::max);
        assert!(max <= ceiling + 1e-4, "max {max} exceeds ceiling {ceiling}");
    }

    #[test]
    fn the_eq_runs_before_the_limiter() {
        // A +20 dB low shelf on a -20 dBFS 40 Hz tone lifts it to full scale;
        // the limiter then holds it at the -1 dBFS ceiling, proving the EQ's
        // boost is caught rather than clipped.
        let p = OutputParams {
            graphic: Default::default(),
            eq: EqParams {
                bands: [
                    Band {
                        kind: Kind::LowShelf,
                        freq_hz: 400.0,
                        gain_db: 20.0,
                        q: 0.707,
                    },
                    Band::default(),
                    Band::default(),
                    Band::default(),
                ],
                ..EqParams::default()
            },
            limiter: LimiterParams {
                on: true,
                ceiling_db: -1.0,
                ..LimiterParams::default()
            },
        };
        let mut o = OutputProc::new(p, FS);
        let mut x: Vec<f32> = (0..48_000)
            .map(|i| 0.1 * (TAU * 40.0 * i as f64 / FS).sin() as f32)
            .collect();
        o.process(&mut [&mut x]);
        let tail = &x[40_000..];
        assert!((level_db(tail) + 1.0).abs() < 0.05, "{}", level_db(tail));
        assert!(o.limiter_gr_db() <= -0.9, "{}", o.limiter_gr_db());
    }

    #[test]
    fn turning_the_limiter_off_releases_without_a_click() {
        let mut o = limiting(-1.0);
        let mut x = vec![1.0f32; 480];
        o.process(&mut [&mut x]);
        assert!(o.limiter_gr_db() <= -0.9);
        let mut p = o.params();
        p.limiter.on = false;
        o.set(p);
        // 1.25 s of signal: the reduction glides back to unity (release time
        // 120 ms, so a handful of time constants) with no step.
        let mut y = vec![1.0f32; 60_000];
        o.process(&mut [&mut y]);
        let step = y
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(step < 0.01, "{step}");
        assert!(y[59_999] > 0.999, "{}", y[59_999]);
        assert!(o.limiter_gr_db() > -0.01);
    }
}
