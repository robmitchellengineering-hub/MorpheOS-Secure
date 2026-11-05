//! Stereo meters: phase correlation, balance, and the goniometer (vector
//! scope) point stream.
//!
//! **Correlation**: +1 mono-compatible (same signal both sides), 0 unrelated
//! (wide), -1 out of phase (cancels in mono). Exponentially averaged
//! (~300 ms by default).
//!
//! **Goniometer**: each point is (side, mid) of one sample, rotated 45
//! degrees so mono is a vertical line and out-of-phase a horizontal one; the
//! meter keeps the latest points (decimated) in a fixed ring for the display
//! to draw. No allocation after `new`.

use crate::prelude::*;

#[derive(Debug, Clone)]
pub struct Correlation {
    k: f64,
    lr: f64,
    ll: f64,
    rr: f64,
}

impl Correlation {
    pub fn new(fs: f64, time_s: f64) -> Correlation {
        Correlation {
            k: (-1.0 / (time_s * fs)).exp(),
            lr: 0.0,
            ll: 0.0,
            rr: 0.0,
        }
    }

    pub fn process(&mut self, l: &[f32], r: &[f32]) {
        let k = self.k;
        for (&a, &b) in l.iter().zip(r) {
            let (a, b) = (f64::from(a), f64::from(b));
            self.lr = k * self.lr + (1.0 - k) * a * b;
            self.ll = k * self.ll + (1.0 - k) * a * a;
            self.rr = k * self.rr + (1.0 - k) * b * b;
        }
    }

    /// -1 .. +1 (0 when silent).
    pub fn read(&self) -> f64 {
        let d = (self.ll * self.rr).sqrt();
        if d < 1e-20 {
            0.0
        } else {
            (self.lr / d).clamp(-1.0, 1.0)
        }
    }

    /// Balance in dB, positive when the right is louder.
    pub fn balance_db(&self) -> f64 {
        10.0 * (self.rr.max(1e-20) / self.ll.max(1e-20)).log10()
    }
}

/// A goniometer point: (side, mid).
pub type Point = (f32, f32);

#[derive(Debug, Clone)]
pub struct Goniometer {
    points: Vec<Point>,
    head: usize,
    every: usize,
    count: usize,
}

impl Goniometer {
    /// Keeps the latest `capacity` points, one of every `every` samples.
    pub fn new(capacity: usize, every: usize) -> Goniometer {
        Goniometer {
            points: vec![(0.0, 0.0); capacity.max(1)],
            head: 0,
            every: every.max(1),
            count: 0,
        }
    }

    pub fn process(&mut self, l: &[f32], r: &[f32]) {
        let s = core::f32::consts::FRAC_1_SQRT_2;
        for (&a, &b) in l.iter().zip(r) {
            self.count += 1;
            if self.count % self.every != 0 {
                continue;
            }
            // x: side (L - R), y: mid (L + R).
            self.points[self.head] = ((a - b) * s, (a + b) * s);
            self.head = (self.head + 1) % self.points.len();
        }
    }

    /// The points, oldest first, as two slices (no copying).
    pub fn points(&self) -> (&[Point], &[Point]) {
        let (a, b) = self.points.split_at(self.head);
        (b, a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    fn sig(n: usize, hz: f64, phase: f64) -> Vec<f32> {
        (0..n)
            .map(|i| (0.5 * (core::f64::consts::TAU * hz * i as f64 / FS + phase).sin()) as f32)
            .collect()
    }

    #[test]
    fn correlation_mono_wide_and_inverted() {
        let a = sig(48_000, 440.0, 0.0);
        let inv: Vec<f32> = a.iter().map(|v| -v).collect();
        let other = sig(48_000, 523.25, 1.0);
        let mut c = Correlation::new(FS, 0.3);
        c.process(&a, &a);
        assert!((c.read() - 1.0).abs() < 1e-6);
        let mut c = Correlation::new(FS, 0.3);
        c.process(&a, &inv);
        assert!((c.read() + 1.0).abs() < 1e-6);
        let mut c = Correlation::new(FS, 0.3);
        c.process(&a, &other);
        assert!(c.read().abs() < 0.1, "unrelated: {}", c.read());
        // Right 6 dB down: balance -6 dB.
        let half: Vec<f32> = a.iter().map(|v| v * 0.5).collect();
        let mut c = Correlation::new(FS, 0.3);
        c.process(&a, &half);
        assert!((c.balance_db() + 6.02).abs() < 0.05);
    }

    #[test]
    fn goniometer_draws_mono_vertical_and_inverted_horizontal() {
        let a = sig(4_800, 440.0, 0.0);
        let inv: Vec<f32> = a.iter().map(|v| -v).collect();
        let mut g = Goniometer::new(256, 4);
        g.process(&a, &a);
        let (x, y) = g.points();
        assert!(x.iter().chain(y).all(|p| p.0.abs() < 1e-6), "mono: no side");
        let mut g = Goniometer::new(256, 4);
        g.process(&a, &inv);
        let (x, y) = g.points();
        assert!(
            x.iter().chain(y).all(|p| p.1.abs() < 1e-6),
            "inverted: no mid"
        );
        assert_eq!(x.len() + y.len(), 256);
    }
}
