//! System tuning: the dual-channel transfer function and the delay finder.
//!
//! **Transfer function.** Feed the signal going to a system (the reference:
//! programme or pink noise) and what a measurement microphone hears back.
//! Averaged cross- and auto-spectra give, per frequency, the system's
//! **magnitude** and **phase** response and the **coherence** (0..1: how much
//! of what the mic heard is explained by the reference; low coherence means
//! noise, reverberation or a different source, so don't EQ on it). Works on
//! programme material, not just test noise, so a system can be checked during
//! a show. The reference is delayed by the measured delay (see below) first.
//!
//! **Delay finder.** Cross-correlation with phase-transform weighting
//! (GCC-PHAT), sharp even in reverberant rooms, refined between samples by a
//! parabola through the peak: how far behind the reference the measurement
//! is. For aligning delay towers and fills (2.9 ms per metre) and for setting
//! the transfer function's delay.
//!
//! Off the audio thread. `TransferFunction` allocates in `new` only; the delay
//! finder allocates per call.

use crate::prelude::*;

use crate::fft::{Fft, C};

#[derive(Debug, Clone)]
pub struct TransferFunction {
    n: usize,
    fft: Fft,
    window: Vec<f64>,
    /// Reference delay line (to line it up with the measurement) and inputs.
    delay: usize,
    ref_line: Vec<f32>,
    ref_head: usize,
    buf_x: Vec<f32>,
    buf_y: Vec<f32>,
    fill: usize,
    hop: usize,
    since: usize,
    sxx: Vec<f64>,
    syy: Vec<f64>,
    sxy: Vec<C>,
    fx: Vec<C>,
    fy: Vec<C>,
    pub frames: u64,
}

/// One frequency's result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub hz: f64,
    pub magnitude_db: f64,
    pub phase_deg: f64,
    pub coherence: f64,
}

impl TransferFunction {
    /// `n`-point frames (power of two), half overlapped; reference delayed
    /// by up to `max_delay` samples.
    pub fn new(n: usize, max_delay: usize) -> TransferFunction {
        TransferFunction {
            n,
            fft: Fft::new(n),
            window: (0..n)
                .map(|i| 0.5 - 0.5 * (core::f64::consts::TAU * i as f64 / n as f64).cos())
                .collect(),
            delay: 0,
            ref_line: vec![0.0; max_delay + 1],
            ref_head: 0,
            buf_x: vec![0.0; n],
            buf_y: vec![0.0; n],
            fill: 0,
            hop: n / 2,
            since: 0,
            sxx: vec![0.0; n / 2 + 1],
            syy: vec![0.0; n / 2 + 1],
            sxy: vec![C::default(); n / 2 + 1],
            fx: vec![C::default(); n],
            fy: vec![C::default(); n],
            frames: 0,
        }
    }

    /// Delays the reference by `samples` (from the delay finder); restarts
    /// the averages.
    pub fn set_delay(&mut self, samples: usize) {
        self.delay = samples.min(self.ref_line.len() - 1);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.sxx.fill(0.0);
        self.syy.fill(0.0);
        self.sxy.fill(C::default());
        self.frames = 0;
    }

    /// Feeds the reference and the measurement, sample for sample.
    pub fn push(&mut self, reference: &[f32], measured: &[f32]) {
        let len = self.ref_line.len();
        for (&x, &y) in reference.iter().zip(measured) {
            self.ref_line[self.ref_head] = x;
            let xd = self.ref_line[(self.ref_head + len - self.delay) % len];
            self.ref_head = (self.ref_head + 1) % len;
            // Shift-in buffers (simple: frames are infrequent).
            if self.fill < self.n {
                self.buf_x[self.fill] = xd;
                self.buf_y[self.fill] = y;
                self.fill += 1;
            } else {
                self.buf_x.copy_within(1.., 0);
                self.buf_y.copy_within(1.., 0);
                self.buf_x[self.n - 1] = xd;
                self.buf_y[self.n - 1] = y;
            }
            self.since += 1;
            if self.fill == self.n && self.since >= self.hop {
                self.since = 0;
                self.frame();
            }
        }
    }

    fn frame(&mut self) {
        for i in 0..self.n {
            let w = self.window[i];
            self.fx[i] = C::new(f64::from(self.buf_x[i]) * w, 0.0);
            self.fy[i] = C::new(f64::from(self.buf_y[i]) * w, 0.0);
        }
        self.fft.forward(&mut self.fx);
        self.fft.forward(&mut self.fy);
        for b in 0..=self.n / 2 {
            let (x, y) = (self.fx[b], self.fy[b]);
            self.sxx[b] += x.norm2();
            self.syy[b] += y.norm2();
            let c = y.times(x.conj());
            self.sxy[b] = C::new(self.sxy[b].re + c.re, self.sxy[b].im + c.im);
        }
        self.frames += 1;
    }

    /// The response at bin `b` (`fs` for its frequency).
    pub fn point(&self, b: usize, fs: f64) -> Point {
        let (sxx, syy, sxy) = (self.sxx[b], self.syy[b], self.sxy[b]);
        let h = C::new(sxy.re / sxx.max(1e-30), sxy.im / sxx.max(1e-30));
        Point {
            hz: b as f64 * fs / self.n as f64,
            magnitude_db: 20.0 * h.abs().max(1e-15).log10(),
            phase_deg: h.arg().to_degrees(),
            coherence: (sxy.norm2() / (sxx * syy).max(1e-30)).clamp(0.0, 1.0),
        }
    }

    /// The bin nearest `hz`.
    pub fn bin(&self, hz: f64, fs: f64) -> usize {
        ((hz * self.n as f64 / fs).round() as usize).min(self.n / 2)
    }
}

/// How far `measured` lags `reference` (samples, sub-sample precise), and a
/// confidence (peak height over the next-highest, >= 1). `None` if nothing
/// stands out. Searches lags `0 ..= max_lag`.
pub fn find_delay(reference: &[f32], measured: &[f32], max_lag: usize) -> Option<(f64, f64)> {
    let len = reference.len().min(measured.len());
    let n = (2 * len).next_power_of_two();
    let fft = Fft::new(n);
    let mut x: Vec<C> = (0..n)
        .map(|i| {
            C::new(
                if i < len {
                    f64::from(reference[i])
                } else {
                    0.0
                },
                0.0,
            )
        })
        .collect();
    let mut y: Vec<C> = (0..n)
        .map(|i| C::new(if i < len { f64::from(measured[i]) } else { 0.0 }, 0.0))
        .collect();
    fft.forward(&mut x);
    fft.forward(&mut y);
    let mut r: Vec<C> = x
        .iter()
        .zip(&y)
        .map(|(a, b)| {
            let c = b.times(a.conj());
            let m = c.abs().max(1e-20);
            C::new(c.re / m, c.im / m)
        })
        .collect();
    fft.inverse(&mut r);
    let lags = max_lag.min(n / 2 - 1);
    let (mut best, mut at) = (f64::MIN, 0usize);
    for (k, v) in r.iter().enumerate().take(lags + 1) {
        if v.re > best {
            best = v.re;
            at = k;
        }
    }
    let second = r
        .iter()
        .enumerate()
        .take(lags + 1)
        .filter(|(k, _)| k.abs_diff(at) > 2)
        .map(|(_, v)| v.re)
        .fold(f64::MIN, f64::max);
    if best <= 0.0 {
        return None;
    }
    // Parabola through the peak and its neighbours.
    let frac = if at > 0 && at < lags {
        let (a, b, c) = (r[at - 1].re, r[at].re, r[at + 1].re);
        let d = a - 2.0 * b + c;
        if d.abs() > 1e-20 {
            0.5 * (a - c) / d
        } else {
            0.0
        }
    } else {
        0.0
    };
    Some((at as f64 + frac, best / second.max(1e-9)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    fn noise(n: usize, seed: u64) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                ((s >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) as f32 * 0.3
            })
            .collect()
    }

    /// The "system": +6 dB peak at 1 kHz (Q 2), delayed `d` samples, plus a
    /// little noise at the microphone.
    fn system(x: &[f32], d: usize) -> Vec<f32> {
        let (f, g, q) = (1_000.0f64, 6.0f64, 2.0f64);
        let w0 = core::f64::consts::TAU * f / FS;
        let a = 10f64.powf(g / 40.0);
        let al = w0.sin() / (2.0 * q);
        let a0 = 1.0 + al / a;
        let (b0, b1, b2, a1, a2) = (
            (1.0 + al * a) / a0,
            -2.0 * w0.cos() / a0,
            (1.0 - al * a) / a0,
            -2.0 * w0.cos() / a0,
            (1.0 - al / a) / a0,
        );
        let (mut z1, mut z2) = (0.0, 0.0);
        let filtered: Vec<f64> = x
            .iter()
            .map(|&v| {
                let v = f64::from(v);
                let y = b0 * v + z1;
                z1 = b1 * v - a1 * y + z2;
                z2 = b2 * v - a2 * y;
                y
            })
            .collect();
        let hiss = noise(x.len(), 99);
        (0..x.len())
            .map(|i| {
                let v = if i >= d { filtered[i - d] } else { 0.0 };
                (v + f64::from(hiss[i]) * 0.01) as f32
            })
            .collect()
    }

    #[test]
    fn finds_the_delay_to_a_fraction_of_a_sample() {
        let x = noise(48_000, 7);
        let y = system(&x, 437); // 9.1 ms: about 3.1 m
        let (d, conf) = find_delay(&x, &y, 4_800).unwrap_or((0.0, 0.0));
        assert!((d - 437.0).abs() < 0.2, "found {d:.2}");
        assert!(conf > 3.0, "confidence {conf:.1}");
        // Half a sample (linear interpolation of the delayed signal).
        let y2: Vec<f32> = (0..y.len())
            .map(|i| if i == 0 { 0.0 } else { 0.5 * (y[i] + y[i - 1]) })
            .collect();
        let (d2, _) = find_delay(&x, &y2, 4_800).unwrap_or((0.0, 0.0));
        assert!((d2 - 437.5).abs() < 0.25, "found {d2:.2}");
    }

    #[test]
    fn transfer_function_shows_the_eq_and_trusts_itself() {
        let x = noise(48_000 * 4, 11);
        let y = system(&x, 437);
        let (d, _) = find_delay(&x[..48_000], &y[..48_000], 4_800).unwrap_or((0.0, 0.0));
        let mut tf = TransferFunction::new(8_192, 4_800);
        tf.set_delay(d.round() as usize);
        for (a, b) in x.chunks(4_800).zip(y.chunks(4_800)) {
            tf.push(a, b);
        }
        let at = |hz: f64| tf.point(tf.bin(hz, FS), FS);
        let (p1k, p100, p10k) = (at(1_000.0), at(100.0), at(10_000.0));
        assert!(
            (p1k.magnitude_db - 6.0).abs() < 0.3,
            "1 kHz {:.2} dB",
            p1k.magnitude_db
        );
        assert!(
            p100.magnitude_db.abs() < 0.3,
            "100 Hz {:.2} dB",
            p100.magnitude_db
        );
        assert!(
            p10k.magnitude_db.abs() < 0.3,
            "10 kHz {:.2} dB",
            p10k.magnitude_db
        );
        assert!(
            p100.phase_deg.abs() < 10.0,
            "delay removed: phase {:.1}",
            p100.phase_deg
        );
        for p in [p1k, p100, p10k] {
            assert!(
                p.coherence > 0.95,
                "{:.0} Hz coherence {:.3}",
                p.hz,
                p.coherence
            );
        }
        // Without the delay lined up, coherence collapses at high frequency
        // and the phase spins: the reason to find the delay first.
        let mut raw = TransferFunction::new(8_192, 4_800);
        for (a, b) in x.chunks(4_800).zip(y.chunks(4_800)) {
            raw.push(a, b);
        }
        assert!(raw.point(raw.bin(10_000.0, FS), FS).coherence < p10k.coherence);
    }
}
