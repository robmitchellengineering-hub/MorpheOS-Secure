//! Stereo delay (echo) with feedback, damping and ping-pong.
//!
//! Used as an effect on an aux bus (`dry` 0, `wet` 1: the return carries only
//! the echoes) or as an insert. The delay time glides when changed (reads
//! between samples, like a tape delay sliding), so a change never clicks.
//! Buffers are allocated once, for [`MAX_MS`].

use crate::prelude::*;

use crate::filter::ANTI_DENORMAL;

/// Longest delay.
pub const MAX_MS: f32 = 2_000.0;

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct DelayParams {
    pub time_ms: f32,
    /// 0 .. 0.95.
    pub feedback: f32,
    /// Echoes lose treble above this (one pole in the feedback loop).
    pub damping_hz: f32,
    /// Echoes alternate left and right.
    pub ping_pong: bool,
    pub dry: f32,
    pub wet: f32,
}

impl Default for DelayParams {
    fn default() -> Self {
        DelayParams {
            time_ms: 375.0,
            feedback: 0.35,
            damping_hz: 6_000.0,
            ping_pong: false,
            dry: 0.0,
            wet: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Delay {
    fs: f64,
    p: DelayParams,
    lines: [Vec<f32>; 2],
    w: usize,
    /// Delay in samples now (glides) and target.
    d: f64,
    d_target: f64,
    glide: f64,
    lp: [f64; 2],
    lp_k: f64,
}

impl Delay {
    pub fn new(p: DelayParams, fs: f64) -> Delay {
        let len = (f64::from(MAX_MS) * 1e-3 * fs) as usize + 4;
        let mut d = Delay {
            fs,
            p,
            lines: [vec![0.0; len], vec![0.0; len]],
            w: 0,
            d: 0.0,
            d_target: 0.0,
            glide: 1.0 - (-1.0 / (0.08 * fs)).exp(),
            lp: [0.0; 2],
            lp_k: 0.0,
        };
        d.set(p);
        d.d = d.d_target;
        d
    }

    pub fn set(&mut self, p: DelayParams) {
        self.p = p;
        let max = (self.lines[0].len() - 3) as f64;
        // Whole samples once settled (reading between samples dulls echoes).
        self.d_target = (f64::from(p.time_ms) * 1e-3 * self.fs)
            .round()
            .clamp(1.0, max);
        let fc = f64::from(p.damping_hz).clamp(200.0, self.fs * 0.45);
        self.lp_k = (-2.0 * core::f64::consts::PI * fc / self.fs).exp();
    }

    #[inline]
    fn read(&self, ch: usize) -> f64 {
        let len = self.lines[ch].len();
        let pos = self.w as f64 + len as f64 - self.d;
        let i = pos.floor();
        let frac = pos - i;
        let i = i as usize % len;
        let a = f64::from(self.lines[ch][i]);
        let b = f64::from(self.lines[ch][(i + 1) % len]);
        a + (b - a) * frac
    }

    /// Processes planar stereo (or mono: one channel) in place.
    pub fn process(&mut self, chans: &mut [&mut [f32]]) {
        let n = chans.first().map_or(0, |c| c.len());
        let stereo = chans.len() >= 2;
        let fb = f64::from(self.p.feedback.clamp(0.0, 0.95));
        let (dry, wet) = (self.p.dry, self.p.wet);
        let len = self.lines[0].len();
        for i in 0..n {
            if self.d != self.d_target {
                self.d += self.glide * (self.d_target - self.d);
                if (self.d_target - self.d).abs() < 1e-4 {
                    self.d = self.d_target;
                }
            }
            let x = [
                f64::from(chans[0][i]),
                if stereo { f64::from(chans[1][i]) } else { 0.0 },
            ];
            let mut y = [self.read(0), self.read(1)];
            for (ch, v) in y.iter_mut().enumerate() {
                self.lp[ch] = (1.0 - self.lp_k) * *v + self.lp_k * self.lp[ch] + ANTI_DENORMAL;
                *v = self.lp[ch];
            }
            // Feed the lines: straight, or crossed for ping-pong (the input
            // enters on the left; every echo swaps sides).
            let (in_l, in_r) = if self.p.ping_pong {
                (x[0] + x[1] + fb * y[1], fb * y[0])
            } else {
                (x[0] + fb * y[0], x[1] + fb * y[1])
            };
            self.lines[0][self.w] = in_l as f32;
            self.lines[1][self.w] = in_r as f32;
            self.w = (self.w + 1) % len;
            if stereo {
                chans[0][i] = dry * chans[0][i] + wet * y[0] as f32;
                chans[1][i] = dry * chans[1][i] + wet * y[1] as f32;
            } else {
                chans[0][i] = dry * chans[0][i] + wet * y[0] as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    fn peaks(x: &[f32], over: f32) -> Vec<(usize, f32)> {
        let mut v = Vec::new();
        for i in 1..x.len() - 1 {
            if x[i].abs() > over && x[i].abs() >= x[i - 1].abs() && x[i].abs() > x[i + 1].abs() {
                v.push((i, x[i]));
            }
        }
        v
    }

    #[test]
    fn echoes_land_on_time_and_fall_by_the_feedback() {
        let p = DelayParams {
            time_ms: 100.0,
            feedback: 0.5,
            damping_hz: 20_000.0,
            ..DelayParams::default()
        };
        let mut d = Delay::new(p, FS);
        let mut l = vec![0.0f32; 48_000];
        let mut r = vec![0.0f32; 48_000];
        l[0] = 1.0;
        d.process(&mut [&mut l, &mut r]);
        let pk = peaks(&l, 0.05);
        assert_eq!(
            pk.iter().map(|p| p.0).take(3).collect::<Vec<_>>(),
            [4_800, 9_600, 14_400]
        );
        // Each echo's total (the damping filter spreads it a little, so
        // compare areas, not peaks): every echo is half the one before.
        let area = |c: usize| {
            l[c - 50..c + 200]
                .iter()
                .map(|v| f64::from(*v))
                .sum::<f64>()
        };
        let (a1, a2, a3) = (area(4_800), area(9_600), area(14_400));
        assert!(
            (a1 - 1.0).abs() < 0.01,
            "first echo carries the impulse: {a1}"
        );
        assert!(
            (a2 / a1 - 0.5).abs() < 0.01 && (a3 / a2 - 0.5).abs() < 0.01,
            "{a1} {a2} {a3}"
        );
        assert!(
            r.iter().all(|v| v.abs() < 1e-9),
            "plain stereo keeps sides apart"
        );
    }

    #[test]
    fn ping_pong_alternates_sides() {
        let mut d = Delay::new(
            DelayParams {
                time_ms: 50.0,
                feedback: 0.6,
                ping_pong: true,
                damping_hz: 20_000.0,
                ..DelayParams::default()
            },
            FS,
        );
        let mut l = vec![0.0f32; 24_000];
        let mut r = vec![0.0f32; 24_000];
        l[0] = 1.0;
        d.process(&mut [&mut l, &mut r]);
        let (pl, pr) = (peaks(&l, 0.05), peaks(&r, 0.05));
        assert_eq!(pl[0].0, 2_400, "first echo left");
        assert_eq!(pr[0].0, 4_800, "second right");
        assert_eq!(pl[1].0, 7_200, "third left");
    }

    #[test]
    fn a_time_change_glides_and_silence_stays_finite() {
        let mut d = Delay::new(
            DelayParams {
                time_ms: 200.0,
                feedback: 0.9,
                ..DelayParams::default()
            },
            FS,
        );
        let mut x: Vec<f32> = (0..96_000)
            .map(|i| (0.3 * (core::f64::consts::TAU * 220.0 * i as f64 / FS).sin()) as f32)
            .collect();
        d.process(&mut [&mut x]);
        let mut p = DelayParams {
            time_ms: 260.0,
            feedback: 0.9,
            ..DelayParams::default()
        };
        p.time_ms = 260.0;
        d.set(p);
        let mut y: Vec<f32> = (96_000..144_000)
            .map(|i| (0.3 * (core::f64::consts::TAU * 220.0 * i as f64 / FS).sin()) as f32)
            .collect();
        d.process(&mut [&mut y]);
        let step = y
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(step < 0.2, "no jump when the time changes: {step}");
        let mut s = vec![0.0f32; 48_000 * 30];
        d.process(&mut [&mut s]);
        assert!(s.iter().all(|v| v.is_finite()));
        // 90 % feedback every 260 ms: about -36 dB per 10 s.
        let peak = |r: core::ops::Range<usize>| s[r].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let (early, late) = (peak(0..48_000), peak(48_000 * 29..48_000 * 30));
        assert!(late < early * 1e-3, "dies away: {early} -> {late}");
    }
}
