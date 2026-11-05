//! Loudness to ITU-R BS.1770-4 and EBU R128: momentary (400 ms), short-term
//! (3 s), integrated (gated), loudness range (EBU Tech 3342), and true peak
//! (4x oversampled).
//!
//! **K-weighting**: a high shelf (+4 dB above ~1.7 kHz, the head's effect)
//! then a high-pass (~38 Hz, "RLB"). Coefficients are computed from the
//! filters' analogue design for any sample rate; at 48 kHz they reproduce the
//! values printed in BS.1770 (tested).
//!
//! **Gating** (integrated loudness): 400 ms blocks every 100 ms; blocks under
//! -70 LUFS are dropped, then blocks more than 10 LU under the loudness of
//! the rest. Integrated loudness and loudness range use histograms (0.01 LU
//! bins), so a show of any length needs no growing memory and no allocation
//! after construction.
//!
//! Stereo (two channels, weight 1 each); surround weights are a later step.

use crate::prelude::*;

use core::f64::consts::PI;

const BINS_FROM: f64 = -70.0;
const BINS_TO: f64 = 10.0;
const BIN: f64 = 0.01;

fn power_to_lufs(p: f64) -> f64 {
    -0.691 + 10.0 * p.max(1e-30).log10()
}

fn lufs_to_power(l: f64) -> f64 {
    10f64.powf((l + 0.691) / 10.0)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Biquad {
    pub b: [f64; 3],
    pub a: [f64; 3],
}

/// The two K-weighting stages for sample rate `fs`.
pub fn k_weighting(fs: f64) -> [Biquad; 2] {
    // Stage 1: high shelf.
    let (g, q, fc) = (
        3.999_843_853_973_347,
        0.707_175_236_955_419_6,
        1_681.974_450_955_533,
    );
    let k = (PI * fc / fs).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    // Stage 2: high-pass.
    let (q, fc) = (0.500_327_037_323_877_3, 38.135_470_876_024_44);
    let k = (PI * fc / fs).tan();
    let a0 = 1.0 + k / q + k * k;
    let hp = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    [shelf, hp]
}

#[derive(Debug, Clone, Copy, Default)]
struct State {
    z: [[f64; 2]; 2],
}

impl State {
    #[inline]
    fn run(&mut self, f: &[Biquad; 2], mut x: f64) -> f64 {
        for (s, b) in self.z.iter_mut().zip(f) {
            let y = b.b[0] * x + s[0];
            s[0] = b.b[1] * x - b.a[1] * y + s[1];
            s[1] = b.b[2] * x - b.a[2] * y;
            x = y;
        }
        x
    }
}

/// A fixed-size histogram of loudness values (0.01 LU bins) holding the
/// count and summed power per bin.
#[derive(Debug, Clone)]
struct Hist {
    count: Vec<u32>,
}

impl Hist {
    fn new() -> Hist {
        Hist {
            count: vec![0; ((BINS_TO - BINS_FROM) / BIN) as usize + 1],
        }
    }
    fn bin(l: f64) -> Option<usize> {
        if !(BINS_FROM..=BINS_TO).contains(&l) {
            return None;
        }
        Some(((l - BINS_FROM) / BIN).round() as usize)
    }
    fn value(i: usize) -> f64 {
        BINS_FROM + i as f64 * BIN
    }
    fn add(&mut self, l: f64) {
        if let Some(i) = Self::bin(l.min(BINS_TO)) {
            self.count[i] = self.count[i].saturating_add(1);
        }
    }
    /// Mean power of the values at or above `gate` (LUFS), as loudness.
    fn gated_mean(&self, gate: f64) -> Option<f64> {
        let from = Self::bin(gate.max(BINS_FROM)).unwrap_or(0);
        let (mut n, mut p) = (0u64, 0.0f64);
        for i in from..self.count.len() {
            let c = u64::from(self.count[i]);
            n += c;
            p += c as f64 * lufs_to_power(Self::value(i));
        }
        (n > 0).then(|| power_to_lufs(p / n as f64))
    }
    /// The value at quantile `q` of those at or above `gate`.
    fn quantile(&self, gate: f64, q: f64) -> Option<f64> {
        let from = Self::bin(gate.max(BINS_FROM)).unwrap_or(0);
        let n: u64 = self.count[from..].iter().map(|&c| u64::from(c)).sum();
        if n == 0 {
            return None;
        }
        let want = ((q * n as f64).ceil() as u64).clamp(1, n);
        let mut seen = 0u64;
        for i in from..self.count.len() {
            seen += u64::from(self.count[i]);
            if seen >= want {
                return Some(Self::value(i));
            }
        }
        None
    }
    fn clear(&mut self) {
        self.count.fill(0);
    }
}

/// What the loudness meter shows.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Loudness {
    pub momentary: Option<f64>,
    pub short_term: Option<f64>,
    pub integrated: Option<f64>,
    /// Loudness range (LU).
    pub range: Option<f64>,
    /// Highest true peak so far (dBTP), and the highest momentary loudness.
    pub true_peak: Option<f64>,
    pub max_momentary: Option<f64>,
}

/// The loudness meter for one stereo signal. Allocates in `new` only.
#[derive(Debug, Clone)]
pub struct LoudnessMeter {
    filt: [Biquad; 2],
    st: [State; 2],
    /// Samples per 100 ms block and the running sum of this block.
    block: usize,
    in_block: usize,
    acc: f64,
    /// The last 30 blocks' mean powers (3 s), a ring.
    blocks: [f64; 30],
    filled: usize,
    head: usize,
    integrated: Hist,
    short_terms: Hist,
    tp: TruePeak,
    max_tp: f64,
    max_m: Option<f64>,
    last: Loudness,
}

impl LoudnessMeter {
    pub fn new(fs: f64) -> LoudnessMeter {
        LoudnessMeter {
            filt: k_weighting(fs),
            st: [State::default(); 2],
            block: (fs / 10.0).round() as usize,
            in_block: 0,
            acc: 0.0,
            blocks: [0.0; 30],
            filled: 0,
            head: 0,
            integrated: Hist::new(),
            short_terms: Hist::new(),
            tp: TruePeak::new(),
            max_tp: 0.0,
            max_m: None,
            last: Loudness::default(),
        }
    }

    /// Starts integrated loudness, loudness range and maxima again.
    pub fn reset(&mut self) {
        self.integrated.clear();
        self.short_terms.clear();
        self.max_tp = 0.0;
        self.max_m = None;
        self.tp = TruePeak::new();
    }

    /// Feeds planar stereo (or mono: one channel). No allocation.
    pub fn process(&mut self, chans: &[&[f32]]) {
        let n = chans.first().map_or(0, |c| c.len());
        for i in 0..n {
            let mut p = 0.0;
            for (ch, buf) in chans.iter().enumerate().take(2) {
                let x = f64::from(buf[i]);
                let y = self.st[ch].run(&self.filt, x);
                p += y * y;
                let t = self.tp.push(ch, x);
                if t > self.max_tp {
                    self.max_tp = t;
                }
            }
            self.acc += p;
            self.in_block += 1;
            if self.in_block == self.block {
                self.end_block();
            }
        }
    }

    fn end_block(&mut self) {
        let mean = self.acc / self.block as f64;
        self.acc = 0.0;
        self.in_block = 0;
        self.blocks[self.head] = mean;
        self.head = (self.head + 1) % self.blocks.len();
        self.filled = (self.filled + 1).min(self.blocks.len());
        let recent = |n: usize| -> Option<f64> {
            if self.filled < n {
                return None;
            }
            let len = self.blocks.len();
            let s: f64 = (1..=n)
                .map(|k| self.blocks[(self.head + len - k) % len])
                .sum();
            Some(power_to_lufs(s / n as f64))
        };
        let m = recent(4);
        let s = recent(30);
        if let Some(m) = m {
            if m >= BINS_FROM {
                self.integrated.add(m);
            }
            self.max_m = Some(self.max_m.map_or(m, |x: f64| x.max(m)));
        }
        if let Some(s) = s {
            if s >= BINS_FROM {
                self.short_terms.add(s);
            }
        }
        let integrated = self
            .integrated
            .gated_mean(BINS_FROM)
            .and_then(|ungated| self.integrated.gated_mean(ungated - 10.0));
        let range = self.short_terms.gated_mean(BINS_FROM).and_then(|l| {
            let gate = l - 20.0;
            Some(self.short_terms.quantile(gate, 0.95)? - self.short_terms.quantile(gate, 0.10)?)
        });
        self.last = Loudness {
            momentary: m,
            short_term: s,
            integrated,
            range,
            true_peak: (self.max_tp > 0.0).then(|| 20.0 * self.max_tp.log10()),
            max_momentary: self.max_m,
        };
    }

    /// The readings as of the last complete 100 ms block.
    pub fn read(&self) -> Loudness {
        self.last
    }
}

/// True peak: the signal oversampled 4x (windowed-sinc interpolation,
/// 12 taps per phase), largest magnitude. Reads within about 0.5 dB of the
/// analogue peak up to 20 kHz, as BS.1770-4 Annex 2 expects of a 4x meter.
#[derive(Debug, Clone)]
pub struct TruePeak {
    taps: [[f64; TAPS]; 4],
    hist: [[f64; TAPS]; 2],
    pos: usize,
}

const TAPS: usize = 12;

impl Default for TruePeak {
    fn default() -> Self {
        Self::new()
    }
}

impl TruePeak {
    pub fn new() -> TruePeak {
        // Kaiser-windowed sinc, cutoff at the original Nyquist.
        let n = TAPS * 4;
        let beta = 7.0;
        let i0 = |x: f64| {
            let (mut s, mut t) = (1.0, 1.0);
            for k in 1..30 {
                t *= (x / 2.0) / k as f64;
                s += t * t;
            }
            s
        };
        let mut taps = [[0.0; TAPS]; 4];
        let mid = (n as f64 - 1.0) / 2.0;
        for j in 0..n {
            let t = (j as f64 - mid) / 4.0;
            let sinc = if t.abs() < 1e-12 {
                1.0
            } else {
                (PI * t).sin() / (PI * t)
            };
            let r = (j as f64 - mid) / mid;
            let w = i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0(beta);
            taps[j % 4][j / 4] = sinc * w;
        }
        TruePeak {
            taps,
            hist: [[0.0; TAPS]; 2],
            pos: 0,
        }
    }

    /// Pushes one sample of channel `ch`; returns the largest magnitude of
    /// the four interpolated points it completes.
    #[inline]
    pub fn push(&mut self, ch: usize, x: f64) -> f64 {
        let h = &mut self.hist[ch];
        if ch == 0 {
            self.pos = (self.pos + 1) % TAPS;
        }
        h[self.pos] = x;
        let mut best = 0.0f64;
        for phase in &self.taps {
            let mut s = 0.0;
            for (k, t) in phase.iter().enumerate() {
                s += t * h[(self.pos + TAPS - k) % TAPS];
            }
            best = best.max(s.abs());
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    fn sine(db: f64, hz: f64, seconds: f64, phase: f64) -> Vec<f32> {
        let a = 10f64.powf(db / 20.0);
        (0..(seconds * FS) as usize)
            .map(|i| (a * (2.0 * PI * hz * i as f64 / FS + phase).sin()) as f32)
            .collect()
    }

    fn run(parts: &[(f64, f64)]) -> Loudness {
        let mut m = LoudnessMeter::new(FS);
        for &(db, s) in parts {
            let x = sine(db, 1_000.0, s, 0.0);
            for blk in x.chunks(480) {
                m.process(&[blk, blk]);
            }
        }
        m.read()
    }

    #[test]
    fn k_weighting_matches_bs1770_at_48k() {
        let [s, h] = k_weighting(FS);
        let want_s = [
            1.535_124_859_586_97,
            -2.691_696_189_406_38,
            1.198_392_810_852_85,
        ];
        let want_sa = [-1.690_659_293_182_41, 0.732_480_774_215_85];
        let want_ha = [-1.990_047_454_833_98, 0.990_072_250_366_21];
        for k in 0..3 {
            assert!((s.b[k] - want_s[k]).abs() < 1e-9, "shelf b{k}");
        }
        for k in 0..2 {
            assert!((s.a[k + 1] - want_sa[k]).abs() < 1e-9, "shelf a{}", k + 1);
            assert!((h.a[k + 1] - want_ha[k]).abs() < 1e-8, "hp a{}", k + 1);
        }
    }

    #[test]
    fn ebu_3341_steady_tones() {
        // Tech 3341 cases 1 and 2: stereo 1 kHz at -23 and -33 dBFS.
        for db in [-23.0, -33.0] {
            let l = run(&[(db, 20.0)]);
            for (what, v) in [("M", l.momentary), ("S", l.short_term), ("I", l.integrated)] {
                let v = v.unwrap_or(f64::NAN);
                assert!((v - db).abs() < 0.1, "{what} {v:.2} for {db} dBFS");
            }
        }
    }

    #[test]
    fn ebu_3341_gating() {
        // Case 3: -36, -23, -36 dBFS (10, 60, 10 s): the relative gate
        // drops the quiet parts. Case 4: -72, -36, -23, -36, -72: the
        // absolute gate too.
        let l = run(&[(-36.0, 10.0), (-23.0, 60.0), (-36.0, 10.0)]);
        assert!((l.integrated.unwrap_or(0.0) + 23.0).abs() < 0.1, "{l:?}");
        let l = run(&[
            (-72.0, 10.0),
            (-36.0, 10.0),
            (-23.0, 60.0),
            (-36.0, 10.0),
            (-72.0, 10.0),
        ]);
        assert!((l.integrated.unwrap_or(0.0) + 23.0).abs() < 0.1, "{l:?}");
    }

    #[test]
    fn ebu_3342_loudness_range() {
        // Cases 1 and 2: -20 then -30 dBFS (LRA 10), -20 then -15 (LRA 5),
        // 20 s each; tolerance +-1 LU.
        let l = run(&[(-20.0, 20.0), (-30.0, 20.0)]);
        assert!((l.range.unwrap_or(0.0) - 10.0).abs() < 1.0, "{l:?}");
        let l = run(&[(-20.0, 20.0), (-15.0, 20.0)]);
        assert!((l.range.unwrap_or(0.0) - 5.0).abs() < 1.0, "{l:?}");
    }

    #[test]
    fn true_peak_finds_peaks_between_samples() {
        // 12 kHz (a quarter of the rate) at 45 degrees: every sample is at
        // 0.707 of the peak (-3 dB), the true peak is 0 dB of the setting.
        let mut m = LoudnessMeter::new(FS);
        let x = sine(-6.0, 12_000.0, 1.0, PI / 4.0);
        let sample_peak = x.iter().fold(0.0f32, |a, v| a.max(v.abs()));
        m.process(&[&x, &x]);
        let tp = m.read().true_peak.unwrap_or(-99.0);
        assert!((20.0 * f64::from(sample_peak).log10() + 9.0).abs() < 0.05);
        assert!(
            (tp + 6.0).abs() < 0.5,
            "true peak {tp:.2} dBTP for a -6 dB sine"
        );
        // A low tone: true peak equals the sample peak.
        let mut m = LoudnessMeter::new(FS);
        let x = sine(-1.0, 997.0, 1.0, 0.3);
        m.process(&[&x]);
        assert!((m.read().true_peak.unwrap_or(0.0) + 1.0).abs() < 0.05);
    }
}
