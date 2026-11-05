//! Dynamics: a compressor and a gate, for up to two linked channels.
//!
//! **Compressor** (feed-forward, log domain): the level of the louder channel
//! (peak, or RMS over ~10 ms) goes through the static curve
//! (threshold, ratio, soft knee); the resulting gain reduction is smoothed
//! with separate attack and release times (the usual "smooth decoupled"
//! detector) and applied to both channels, so a stereo image never shifts.
//!
//! **Gate**: opens when the level rises above the threshold, closes when it
//! falls below threshold minus hysteresis (so it does not chatter on a level
//! that hovers), after a hold time; the closed gain is `range` (e.g. -40 dB,
//! or -80 for fully off). Opening and closing glide with their own times, in
//! decibels (a release then fades evenly down to the range, as a gate should,
//! rather than lingering near the bottom as a linear glide would).
//!
//! Allocation-free; every setting may change at any time.

use crate::prelude::*;

use crate::filter::MAX_CH;

fn coef(ms: f32, fs: f64) -> f64 {
    let n = f64::from(ms.max(0.01)) * 1e-3 * fs;
    (-1.0 / n).exp()
}

pub(crate) fn to_db(x: f64) -> f64 {
    20.0 * (x.max(1e-12)).log10()
}

pub(crate) fn from_db(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Detector {
    #[default]
    Peak,
    Rms,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct CompParams {
    pub on: bool,
    pub threshold_db: f32,
    pub ratio: f32,
    /// Soft knee width (dB); 0 is a hard knee.
    pub knee_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
    pub detector: Detector,
}

impl Default for CompParams {
    fn default() -> Self {
        CompParams {
            on: false,
            threshold_db: -18.0,
            ratio: 3.0,
            knee_db: 6.0,
            attack_ms: 10.0,
            release_ms: 120.0,
            makeup_db: 0.0,
            detector: Detector::Peak,
        }
    }
}

impl CompParams {
    /// The static curve: output level for input level `x` (dB).
    pub fn curve(&self, x: f64) -> f64 {
        let (t, r, w) = (
            f64::from(self.threshold_db),
            f64::from(self.ratio.max(1.0)),
            f64::from(self.knee_db.max(0.0)),
        );
        let over = x - t;
        if 2.0 * over < -w {
            x
        } else if w > 0.0 && 2.0 * over.abs() <= w {
            x + (1.0 / r - 1.0) * (over + w / 2.0).powi(2) / (2.0 * w)
        } else {
            t + over / r
        }
    }
}

#[derive(Debug, Clone)]
pub struct Compressor {
    fs: f64,
    p: CompParams,
    att: f64,
    rel: f64,
    rms_k: f64,
    /// Smoothed gain reduction (dB, <= 0) and the RMS state.
    gr_db: f64,
    ms: f64,
    /// Makeup and on/off glide (linear gain applied on top).
    makeup: f64,
    makeup_target: f64,
    glide: f64,
}

impl Compressor {
    pub fn new(p: CompParams, fs: f64) -> Compressor {
        let mut c = Compressor {
            fs,
            p,
            att: 0.0,
            rel: 0.0,
            rms_k: coef(10.0, fs),
            gr_db: 0.0,
            ms: 0.0,
            makeup: 1.0,
            makeup_target: 1.0,
            glide: 1.0 - coef(5.0, fs),
        };
        c.set(p);
        c.makeup = c.makeup_target;
        c
    }

    pub fn set(&mut self, p: CompParams) {
        self.p = p;
        self.att = coef(p.attack_ms, self.fs);
        self.rel = coef(p.release_ms, self.fs);
        self.makeup_target = if p.on {
            from_db(f64::from(p.makeup_db))
        } else {
            1.0
        };
    }

    pub fn params(&self) -> CompParams {
        self.p
    }

    /// Current gain reduction (dB, <= 0), for the meter.
    pub fn gain_reduction_db(&self) -> f32 {
        self.gr_db as f32
    }

    pub fn process(&mut self, chans: &mut [&mut [f32]]) {
        let n = chans.first().map_or(0, |c| c.len());
        let on = self.p.on;
        if !on && self.gr_db > -0.001 && (self.makeup - 1.0).abs() < 1e-6 {
            self.gr_db = 0.0;
            return;
        }
        for i in 0..n {
            let mut peak = 0.0f64;
            for buf in chans.iter().take(MAX_CH) {
                peak = peak.max(f64::from(buf[i]).abs());
            }
            let level = match self.p.detector {
                Detector::Peak => peak,
                Detector::Rms => {
                    self.ms = self.rms_k * self.ms + (1.0 - self.rms_k) * peak * peak;
                    self.ms.sqrt()
                }
            };
            // Off: let any reduction release; on: follow the curve.
            let target = if on {
                let x = to_db(level);
                self.p.curve(x) - x
            } else {
                0.0
            };
            let k = if target < self.gr_db {
                self.att
            } else {
                self.rel
            };
            self.gr_db = k * self.gr_db + (1.0 - k) * target;
            self.makeup += self.glide * (self.makeup_target - self.makeup);
            let g = (from_db(self.gr_db) * self.makeup) as f32;
            for buf in chans.iter_mut().take(MAX_CH) {
                buf[i] *= g;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct GateParams {
    pub on: bool,
    pub threshold_db: f32,
    /// Gain when closed (dB), e.g. -40 or -80.
    pub range_db: f32,
    pub hysteresis_db: f32,
    pub attack_ms: f32,
    pub hold_ms: f32,
    pub release_ms: f32,
}

impl Default for GateParams {
    fn default() -> Self {
        GateParams {
            on: false,
            threshold_db: -45.0,
            range_db: -40.0,
            hysteresis_db: 4.0,
            attack_ms: 0.5,
            hold_ms: 50.0,
            release_ms: 150.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Gate {
    fs: f64,
    p: GateParams,
    att: f64,
    rel: f64,
    /// Peak envelope (fast up, ~10 ms down).
    env: f64,
    env_rel: f64,
    open: bool,
    hold_left: u64,
    gain_db: f64,
}

impl Gate {
    pub fn new(p: GateParams, fs: f64) -> Gate {
        let mut g = Gate {
            fs,
            p,
            att: 0.0,
            rel: 0.0,
            env: 0.0,
            env_rel: coef(10.0, fs),
            open: true,
            hold_left: 0,
            gain_db: 0.0,
        };
        g.set(p);
        g
    }

    pub fn set(&mut self, p: GateParams) {
        self.p = p;
        self.att = coef(p.attack_ms, self.fs);
        self.rel = coef(p.release_ms, self.fs);
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Current gain (dB), for the meter.
    pub fn gain_db(&self) -> f32 {
        self.gain_db as f32
    }

    pub fn process(&mut self, chans: &mut [&mut [f32]]) {
        let n = chans.first().map_or(0, |c| c.len());
        if !self.p.on && self.gain_db > -1e-5 {
            self.gain_db = 0.0;
            self.open = true;
            return;
        }
        let (open_at, close_at) = (
            f64::from(self.p.threshold_db),
            f64::from(self.p.threshold_db - self.p.hysteresis_db.max(0.0)),
        );
        let closed_db = f64::from(self.p.range_db.min(0.0));
        let hold = (f64::from(self.p.hold_ms.max(0.0)) * 1e-3 * self.fs) as u64;
        for i in 0..n {
            let mut peak = 0.0f64;
            for buf in chans.iter().take(MAX_CH) {
                peak = peak.max(f64::from(buf[i]).abs());
            }
            self.env = if peak > self.env {
                peak
            } else {
                self.env_rel * self.env + (1.0 - self.env_rel) * peak
            };
            let lvl = to_db(self.env);
            if !self.p.on {
                self.open = true;
            } else if lvl >= open_at {
                self.open = true;
                self.hold_left = hold;
            } else if lvl < close_at {
                if self.hold_left > 0 {
                    self.hold_left -= 1;
                } else {
                    self.open = false;
                }
            }
            let (target, k) = if self.open {
                (0.0, self.att)
            } else {
                (closed_db, self.rel)
            };
            self.gain_db = k * self.gain_db + (1.0 - k) * target;
            let g = from_db(self.gain_db) as f32;
            for buf in chans.iter_mut().take(MAX_CH) {
                buf[i] *= g;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    fn level_db(x: &[f32]) -> f64 {
        to_db(x.iter().map(|v| f64::from(v.abs())).fold(0.0, f64::max))
    }

    fn comp(p: CompParams) -> Compressor {
        Compressor::new(CompParams { on: true, ..p }, FS)
    }

    #[test]
    fn static_curve_hard_and_soft_knee() {
        // Steady -10 dBFS into threshold -20, ratio 4: out -17.5 dBFS.
        let mut c = comp(CompParams {
            threshold_db: -20.0,
            ratio: 4.0,
            knee_db: 0.0,
            ..CompParams::default()
        });
        let a = from_db(-10.0) as f32;
        let mut x = vec![a; 48_000];
        c.process(&mut [&mut x]);
        assert!(
            (level_db(&x[40_000..]) + 17.5).abs() < 0.02,
            "{}",
            level_db(&x[40_000..])
        );
        assert!((f64::from(c.gain_reduction_db()) + 7.5).abs() < 0.02);
        // Below the threshold: untouched.
        let mut c = comp(CompParams {
            threshold_db: -20.0,
            ratio: 4.0,
            ..CompParams::default()
        });
        let mut q = vec![from_db(-30.0) as f32; 48_000];
        c.process(&mut [&mut q]);
        assert!((level_db(&q[40_000..]) + 30.0).abs() < 0.01);
        // Soft knee (6 dB) at the threshold: (1/R - 1) * W / 8 = -0.5625 dB.
        let p = CompParams {
            threshold_db: -20.0,
            ratio: 4.0,
            knee_db: 6.0,
            ..CompParams::default()
        };
        assert!((p.curve(-20.0) - (-20.5625)).abs() < 1e-9);
        assert!(
            (p.curve(-24.0) + 24.0).abs() < 1e-9,
            "below the knee: unity"
        );
        assert!(
            (p.curve(-10.0) + 17.5).abs() < 1e-9,
            "above the knee: the ratio"
        );
    }

    #[test]
    fn attack_and_release_times() {
        let mut c = comp(CompParams {
            threshold_db: -30.0,
            ratio: 1_000.0,
            knee_db: 0.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            ..CompParams::default()
        });
        // Step from silence to -10 dBFS: target reduction -20 dB. One time
        // constant (10 ms) in: 63 % of it.
        let a = from_db(-10.0) as f32;
        let mut x = vec![a; 480];
        c.process(&mut [&mut x]);
        let gr = f64::from(c.gain_reduction_db());
        assert!((gr / -20.0 - 0.632).abs() < 0.02, "after 10 ms: {gr:.2} dB");
        let mut x = vec![a; 48_000];
        c.process(&mut [&mut x]);
        // Back to silence: 100 ms later, 37 % of the reduction is left.
        let mut s = vec![0.0f32; 4_800];
        c.process(&mut [&mut s]);
        let gr = f64::from(c.gain_reduction_db());
        assert!(
            (gr / -20.0 - 0.368).abs() < 0.02,
            "after 100 ms release: {gr:.2} dB"
        );
    }

    #[test]
    fn stereo_link_and_makeup_and_switching_off() {
        let mut c = comp(CompParams {
            threshold_db: -20.0,
            ratio: 4.0,
            knee_db: 0.0,
            makeup_db: 3.0,
            ..CompParams::default()
        });
        let mut l = vec![from_db(-10.0) as f32; 48_000];
        let mut r = vec![from_db(-30.0) as f32; 48_000];
        c.process(&mut [&mut l, &mut r]);
        // Right is quiet but gets the left's 7.5 dB reduction (plus 3 makeup).
        assert!((level_db(&r[40_000..]) - (-30.0 - 7.5 + 3.0)).abs() < 0.02);
        // Switched off: glides back to unity, no step.
        let mut p = c.params();
        p.on = false;
        c.set(p);
        let mut l = vec![from_db(-10.0) as f32; 48_000];
        c.process(&mut [&mut l]);
        let step = l
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(step < 0.002, "{step}");
        assert!((level_db(&l[40_000..]) + 10.0).abs() < 0.01);
    }

    #[test]
    fn gate_opens_holds_closes_and_ignores_hovering() {
        let mut g = Gate::new(
            GateParams {
                on: true,
                threshold_db: -40.0,
                range_db: -60.0,
                hysteresis_db: 6.0,
                hold_ms: 50.0,
                ..GateParams::default()
            },
            FS,
        );
        // Quiet noise floor at -50: closes to -60 dB.
        let mut x = vec![from_db(-50.0) as f32; 48_000];
        g.process(&mut [&mut x]);
        assert!(!g.is_open());
        assert!((f64::from(g.gain_db()) + 60.0).abs() < 0.5);
        // A voice at -20: opens quickly (attack 0.5 ms).
        let mut v = vec![from_db(-20.0) as f32; 480];
        g.process(&mut [&mut v]);
        assert!(g.is_open());
        assert!(f64::from(g.gain_db()) > -0.01);
        // Drops to -43: between close (-46) and open (-40): stays open.
        let mut h = vec![from_db(-43.0) as f32; 48_000];
        g.process(&mut [&mut h]);
        assert!(g.is_open(), "hysteresis: hovering does not chatter");
        // Silence: held open 50 ms, then closes.
        let mut s = vec![0.0f32; 48 * 40];
        g.process(&mut [&mut s]);
        assert!(g.is_open(), "still in the hold time");
        let mut s = vec![0.0f32; 48 * 60];
        g.process(&mut [&mut s]);
        assert!(!g.is_open());
    }
}
