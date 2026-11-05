//! A plain radix-2 FFT (complex, in place, f64), with twiddles and the
//! bit-reversal table computed once. Sizes are powers of two. Used off the
//! audio thread by the analysers; allocates only in `new`.

use crate::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct C {
    pub re: f64,
    pub im: f64,
}

impl C {
    pub fn new(re: f64, im: f64) -> C {
        C { re, im }
    }
    pub fn conj(self) -> C {
        C::new(self.re, -self.im)
    }
    pub fn times(self, o: C) -> C {
        C::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }
    pub fn norm2(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
    pub fn abs(self) -> f64 {
        self.norm2().sqrt()
    }
    pub fn arg(self) -> f64 {
        self.im.atan2(self.re)
    }
}

#[derive(Debug, Clone)]
pub struct Fft {
    n: usize,
    rev: Vec<usize>,
    tw: Vec<C>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        assert!(
            n.is_power_of_two() && n >= 2,
            "FFT size must be a power of two"
        );
        let bits = n.trailing_zeros();
        let rev = (0..n)
            .map(|i| i.reverse_bits() >> (usize::BITS - bits))
            .collect();
        let tw = (0..n / 2)
            .map(|k| {
                let a = -core::f64::consts::TAU * k as f64 / n as f64;
                C::new(a.cos(), a.sin())
            })
            .collect();
        Fft { n, rev, tw }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Forward transform in place (no scaling).
    pub fn forward(&self, x: &mut [C]) {
        self.run(x, false);
    }

    /// Inverse transform in place, scaled by 1/n.
    pub fn inverse(&self, x: &mut [C]) {
        self.run(x, true);
        let s = 1.0 / self.n as f64;
        for v in x.iter_mut() {
            v.re *= s;
            v.im *= s;
        }
    }

    fn run(&self, x: &mut [C], inverse: bool) {
        let n = self.n;
        assert_eq!(x.len(), n);
        for i in 0..n {
            let j = self.rev[i];
            if i < j {
                x.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let mut w = self.tw[k * step];
                    if inverse {
                        w = w.conj();
                    }
                    let a = x[start + k];
                    let b = x[start + k + len / 2].times(w);
                    x[start + k] = C::new(a.re + b.re, a.im + b.im);
                    x[start + k + len / 2] = C::new(a.re - b.re, a.im - b.im);
                }
            }
            len *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_a_plain_dft_and_round_trips() {
        let n = 64;
        let x: Vec<C> = (0..n)
            .map(|i| {
                C::new(
                    ((i * 7919) % 101) as f64 / 50.0 - 1.0,
                    ((i * 31) % 17) as f64 / 8.0 - 1.0,
                )
            })
            .collect();
        let mut y = x.clone();
        let f = Fft::new(n);
        f.forward(&mut y);
        for k in 0..n {
            let mut s = C::default();
            for (j, v) in x.iter().enumerate() {
                let a = -core::f64::consts::TAU * (j * k) as f64 / n as f64;
                s = C::new(
                    s.re + v.re * a.cos() - v.im * a.sin(),
                    s.im + v.re * a.sin() + v.im * a.cos(),
                );
            }
            assert!(
                (s.re - y[k].re).abs() < 1e-9 && (s.im - y[k].im).abs() < 1e-9,
                "bin {k}"
            );
        }
        f.inverse(&mut y);
        for (a, b) in x.iter().zip(&y) {
            assert!((a.re - b.re).abs() < 1e-12 && (a.im - b.im).abs() < 1e-12);
        }
    }
}
