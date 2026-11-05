//! A stereo reverb: a feedback delay network (FDN).
//!
//! Eight delay lines of mutually prime lengths feed each other through a
//! Householder matrix (lossless and dense: every line reaches every other in
//! one pass), so echoes multiply into a smooth tail. Each line loses exactly
//! the gain that makes the tail fall 60 dB in the decay time, with a one-pole
//! filter that makes the treble decay faster (damping), as rooms do. The
//! input is pre-delayed and diffused (four all-pass filters) so the onset is
//! not a flutter. Left and right are taken from the lines with different
//! signs, which decorrelates them.
//!
//! Used as an effect on an aux bus (`dry` 0, `wet` 1). Buffers are allocated
//! once (for the largest size); changing the size glides the line lengths.
//! Lengths are whole samples once settled: reading between samples is a
//! gentle low-pass, and on every pass round the network it would add damping
//! nobody asked for (measured: a "bright" 2 s tail lost its treble in 1.1 s).

use crate::prelude::*;

use crate::filter::ANTI_DENORMAL;

const N: usize = 8;
/// Line lengths at size 1 (ms), mutually prime in samples at 48 kHz.
const LINES_MS: [f64; N] = [29.7, 37.1, 41.1, 43.7, 53.3, 59.9, 67.4, 79.1];
/// All-pass diffuser lengths (ms) and gain.
const DIFF_MS: [f64; 4] = [4.77, 3.59, 12.73, 9.30];
const DIFF_G: f64 = 0.62;
const MAX_SIZE: f64 = 2.0;
const MAX_PREDELAY_MS: f64 = 250.0;

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct ReverbParams {
    /// Time for the tail to fall 60 dB at low and mid frequencies (s).
    pub decay_s: f32,
    /// 0 (bright) .. 1 (dark): how much faster the treble decays.
    pub damping: f32,
    /// Room size, 0.3 .. 2 (scales the line lengths).
    pub size: f32,
    pub predelay_ms: f32,
    /// 0 (mono) .. 1 (wide).
    pub width: f32,
    pub dry: f32,
    pub wet: f32,
}

impl Default for ReverbParams {
    fn default() -> Self {
        ReverbParams {
            decay_s: 1.8,
            damping: 0.5,
            size: 1.0,
            predelay_ms: 20.0,
            width: 1.0,
            dry: 0.0,
            wet: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
struct Line {
    buf: Vec<f32>,
    w: usize,
}

impl Line {
    fn new(max: usize) -> Line {
        Line {
            buf: vec![0.0; max + 2],
            w: 0,
        }
    }
    #[inline]
    fn read(&self, d: f64) -> f64 {
        let len = self.buf.len();
        let pos = self.w as f64 + len as f64 - d;
        let i = pos.floor();
        let frac = pos - i;
        let i = i as usize % len;
        let a = f64::from(self.buf[i]);
        a + (f64::from(self.buf[(i + 1) % len]) - a) * frac
    }
    #[inline]
    fn write(&mut self, x: f64) {
        self.buf[self.w] = x as f32;
        self.w = (self.w + 1) % self.buf.len();
    }
}

#[derive(Debug, Clone)]
pub struct Reverb {
    fs: f64,
    p: ReverbParams,
    lines: Vec<Line>,
    /// Line lengths now (samples, glide) and targets.
    len: [f64; N],
    len_target: [f64; N],
    gain: [f64; N],
    damp: [f64; N],
    lp: [f64; N],
    diff: Vec<Line>,
    diff_len: [f64; 4],
    pre: Line,
    pre_len: f64,
    glide: f64,
}

impl Reverb {
    pub fn new(p: ReverbParams, fs: f64) -> Reverb {
        let max = |ms: f64| (ms * 1e-3 * fs * MAX_SIZE) as usize + 4;
        let mut r = Reverb {
            fs,
            p,
            lines: LINES_MS.iter().map(|&ms| Line::new(max(ms))).collect(),
            len: [0.0; N],
            len_target: [0.0; N],
            gain: [0.0; N],
            damp: [0.0; N],
            lp: [0.0; N],
            diff: DIFF_MS.iter().map(|&ms| Line::new(max(ms))).collect(),
            diff_len: [0.0; 4],
            pre: Line::new((MAX_PREDELAY_MS * 1e-3 * fs) as usize + 4),
            pre_len: 1.0,
            glide: 1.0 - (-1.0 / (0.2 * fs)).exp(),
        };
        r.set(p);
        r.len = r.len_target;
        r
    }

    pub fn params(&self) -> ReverbParams {
        self.p
    }

    pub fn set(&mut self, p: ReverbParams) {
        self.p = p;
        let size = f64::from(p.size).clamp(0.3, MAX_SIZE);
        let rt = f64::from(p.decay_s).clamp(0.1, 20.0);
        // Treble decays this much faster at full damping.
        let rt_hi = rt * (1.0 - 0.85 * f64::from(p.damping.clamp(0.0, 1.0)));
        for k in 0..N {
            let l = (LINES_MS[k] * 1e-3 * self.fs * size).round();
            self.len_target[k] = l;
            // Gain per pass that gives -60 dB after `rt` seconds.
            let g_lo = 10f64.powf(-3.0 * l / (rt * self.fs));
            let g_hi = 10f64.powf(-3.0 * l / (rt_hi * self.fs));
            self.gain[k] = g_lo;
            // One-pole low-pass whose Nyquist gain is g_hi / g_lo.
            let ratio = g_hi / g_lo;
            self.damp[k] = (1.0 - ratio) / (1.0 + ratio);
        }
        for k in 0..4 {
            self.diff_len[k] = (DIFF_MS[k] * 1e-3 * self.fs * size.min(1.2)).round();
        }
        self.pre_len = (f64::from(p.predelay_ms).clamp(0.0, MAX_PREDELAY_MS) * 1e-3 * self.fs)
            .round()
            .max(1.0);
    }

    /// Processes planar stereo (or mono) in place.
    pub fn process(&mut self, chans: &mut [&mut [f32]]) {
        let n = chans.first().map_or(0, |c| c.len());
        let stereo = chans.len() >= 2;
        let (dry, wet) = (self.p.dry, self.p.wet);
        let width = f64::from(self.p.width.clamp(0.0, 1.0));
        let norm = 1.0 / (N as f64).sqrt();
        for i in 0..n {
            let x_l = f64::from(chans[0][i]);
            let x_r = if stereo { f64::from(chans[1][i]) } else { x_l };
            // Pre-delay, then diffuse (series all-passes).
            self.pre.write(0.5 * (x_l + x_r) + ANTI_DENORMAL);
            let mut s = self.pre.read(self.pre_len);
            for (d, &len) in self.diff.iter_mut().zip(&self.diff_len) {
                let delayed = d.read(len);
                let v = s + DIFF_G * delayed;
                d.write(v);
                s = delayed - DIFF_G * v;
            }
            // Read every line (gliding lengths), damp, apply the decay gain.
            let mut y = [0.0f64; N];
            let mut sum = 0.0;
            for k in 0..N {
                if self.len[k] != self.len_target[k] {
                    self.len[k] += self.glide * (self.len_target[k] - self.len[k]);
                    if (self.len_target[k] - self.len[k]).abs() < 1e-4 {
                        self.len[k] = self.len_target[k];
                    }
                }
                let v = self.lines[k].read(self.len[k]);
                self.lp[k] = (1.0 - self.damp[k]) * v + self.damp[k] * self.lp[k];
                y[k] = self.gain[k] * self.lp[k];
                sum += y[k];
            }
            // Householder feedback: y - (2/N) * sum(y); input in with
            // alternating signs.
            let h = 2.0 / N as f64 * sum;
            for k in 0..N {
                let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
                self.lines[k].write(y[k] - h + sign * s);
            }
            // Two decorrelated outputs.
            let (mut out_l, mut out_r) = (0.0, 0.0);
            for (k, v) in y.iter().enumerate() {
                out_l += if k % 4 < 2 { *v } else { -*v };
                out_r += if k % 2 == 0 { *v } else { -*v };
            }
            let (out_l, out_r) = (out_l * norm, out_r * norm);
            let mid = 0.5 * (out_l + out_r);
            let (wl, wr) = (
                (mid + width * (out_l - mid)) as f32,
                (mid + width * (out_r - mid)) as f32,
            );
            chans[0][i] = dry * chans[0][i] + wet * wl;
            if stereo {
                chans[1][i] = dry * chans[1][i] + wet * wr;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    fn impulse_response(p: ReverbParams, seconds: f64) -> (Vec<f32>, Vec<f32>) {
        let mut r = Reverb::new(p, FS);
        let n = (seconds * FS) as usize;
        let mut l = vec![0.0f32; n];
        let mut rr = vec![0.0f32; n];
        l[0] = 1.0;
        rr[0] = 1.0;
        r.process(&mut [&mut l, &mut rr]);
        (l, rr)
    }

    /// Decay time from the Schroeder backward integral, -5 to -25 dB.
    fn rt60(x: &[f32]) -> f64 {
        let mut e: Vec<f64> = x.iter().map(|v| f64::from(*v).powi(2)).collect();
        for i in (0..e.len() - 1).rev() {
            e[i] += e[i + 1];
        }
        let e0 = e[0];
        let db = |i: usize| 10.0 * (e[i] / e0).log10();
        let t5 = (0..e.len()).find(|&i| db(i) < -5.0).unwrap_or(0);
        let t25 = (0..e.len()).find(|&i| db(i) < -25.0).unwrap_or(e.len() - 1);
        let slope = -20.0 / ((t25 - t5) as f64 / FS); // dB per second
        -60.0 / slope
    }

    #[test]
    fn the_tail_lasts_as_long_as_asked() {
        for rt in [0.8f32, 1.8, 4.0] {
            let (l, r) = impulse_response(
                ReverbParams {
                    decay_s: rt,
                    damping: 0.0,
                    predelay_ms: 0.0,
                    ..ReverbParams::default()
                },
                f64::from(rt) * 2.0,
            );
            for (side, x) in [("left", &l), ("right", &r)] {
                let got = rt60(x);
                assert!(
                    (got / f64::from(rt) - 1.0).abs() < 0.15,
                    "asked {rt} s, {side} decays in {got:.2} s"
                );
            }
        }
    }

    #[test]
    fn damping_shortens_the_treble_and_sides_are_decorrelated() {
        let p = ReverbParams {
            decay_s: 2.0,
            damping: 0.8,
            predelay_ms: 0.0,
            ..ReverbParams::default()
        };
        let (l, r) = impulse_response(p, 3.0);
        let (bright, _) = impulse_response(ReverbParams { damping: 0.0, ..p }, 3.0);
        // Treble (second difference: rises 12 dB/octave) decays much faster
        // with damping; the body of the tail much less so.
        let treble =
            |x: &[f32]| -> Vec<f32> { x.windows(3).map(|w| w[2] - 2.0 * w[1] + w[0]).collect() };
        let (t_damped, t_bright) = (rt60(&treble(&l)), rt60(&treble(&bright)));
        assert!(
            t_damped < 0.6 * t_bright,
            "treble {t_damped:.2} s damped vs {t_bright:.2} s bright"
        );
        // Undamped, the treble lasts (nearly) as long as asked: no hidden
        // damping from reading between samples (it was 1.1 s of 2).
        assert!(
            t_bright > 1.6,
            "bright treble decays in {t_bright:.2} s of 2"
        );
        assert!(
            rt60(&l) > 0.75 * rt60(&bright),
            "body {:.2} vs {:.2}",
            rt60(&l),
            rt60(&bright)
        );
        // Left and right tails are mostly different.
        let tail = 4_800..96_000;
        let dot: f64 = tail
            .clone()
            .map(|i| f64::from(l[i]) * f64::from(r[i]))
            .sum();
        let nl: f64 = tail.clone().map(|i| f64::from(l[i]).powi(2)).sum();
        let nr: f64 = tail.map(|i| f64::from(r[i]).powi(2)).sum();
        let corr = dot / (nl * nr).sqrt();
        assert!(corr.abs() < 0.3, "correlation {corr:.2}");
    }

    #[test]
    fn predelay_long_decays_and_silence_are_safe() {
        let (l, _) = impulse_response(
            ReverbParams {
                predelay_ms: 50.0,
                ..ReverbParams::default()
            },
            0.2,
        );
        let first = l.iter().position(|v| v.abs() > 1e-6).unwrap_or(0);
        assert!(
            first >= 2_400,
            "nothing before the pre-delay: first sound at {first}"
        );
        // A 20 s decay stays bounded and finite; silence afterwards stays
        // finite and dies away.
        let mut r = Reverb::new(
            ReverbParams {
                decay_s: 20.0,
                ..ReverbParams::default()
            },
            FS,
        );
        let mut x: Vec<f32> = (0..48_000)
            .map(|i| (0.5 * (core::f64::consts::TAU * 330.0 * i as f64 / FS).sin()) as f32)
            .collect();
        r.process(&mut [&mut x]);
        assert!(x.iter().all(|v| v.is_finite() && v.abs() < 20.0));
        let mut p = r.params();
        p.decay_s = 0.5;
        r.set(p);
        let mut s = vec![0.0f32; 48_000 * 20];
        r.process(&mut [&mut s]);
        assert!(s.iter().all(|v| v.is_finite()));
        assert!(s[s.len() - 1].abs() < 1e-6);
    }
}
