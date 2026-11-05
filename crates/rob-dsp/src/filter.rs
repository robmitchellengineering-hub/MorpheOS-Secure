//! Second-order filters (biquads) and the channel EQ built from them.
//!
//! Coefficients follow the RBJ "Audio EQ Cookbook". Arithmetic is f64 inside
//! (a 20 Hz high-pass at 48 kHz in f32 has audible noise and drift), audio in
//! and out is f32. Transposed direct form II.
//!
//! **Click-free changes.** A new setting does not replace the coefficients;
//! the coefficients glide towards it (one-pole, ~5 ms). For a second-order
//! section the stable region of (a1, a2) is a triangle, which is convex, so
//! every point on the glide between two stable filters is stable too.

use crate::prelude::*;

use core::f64::consts::PI;

/// Planar stereo at most.
pub const MAX_CH: usize = 2;

/// Keeps recursive state away from subnormal numbers (slow on many CPUs)
/// when the input goes silent: far below anything audible (-400 dB).
pub(crate) const ANTI_DENORMAL: f64 = 1e-20;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coefs {
    pub const IDENTITY: Coefs = Coefs {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn norm(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Coefs {
        Coefs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Magnitude response at `hz` (for tests and the UI's EQ curve).
    pub fn magnitude(&self, hz: f64, fs: f64) -> f64 {
        let w = 2.0 * PI * hz / fs;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let (nr, ni) = (
            self.b0 + self.b1 * c1 + self.b2 * c2,
            -(self.b1 * s1 + self.b2 * s2),
        );
        let (dr, di) = (
            1.0 + self.a1 * c1 + self.a2 * c2,
            -(self.a1 * s1 + self.a2 * s2),
        );
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }
}

/// What a band does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Kind {
    #[default]
    Off,
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
}

/// One filter's settings. `q` for shelves is the cookbook's Q (0.707 is the
/// usual gentle shelf).
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct Band {
    pub kind: Kind,
    pub freq_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

impl Default for Band {
    fn default() -> Self {
        Band {
            kind: Kind::Off,
            freq_hz: 1_000.0,
            gain_db: 0.0,
            q: 0.707,
        }
    }
}

impl Band {
    pub fn peak(freq_hz: f32, gain_db: f32, q: f32) -> Band {
        Band {
            kind: Kind::Peak,
            freq_hz,
            gain_db,
            q,
        }
    }

    pub fn coefs(&self, fs: f64) -> Coefs {
        let f = f64::from(self.freq_hz).clamp(10.0, fs * 0.49);
        let q = f64::from(self.q).clamp(0.1, 30.0);
        let w0 = 2.0 * PI * f / fs;
        let (cw, sw) = (w0.cos(), w0.sin());
        let alpha = sw / (2.0 * q);
        let a = 10f64.powf(f64::from(self.gain_db.clamp(-30.0, 30.0)) / 40.0);
        let sa = 2.0 * a.sqrt() * alpha;
        match self.kind {
            Kind::Off => Coefs::IDENTITY,
            Kind::LowPass => Coefs::norm(
                (1.0 - cw) / 2.0,
                1.0 - cw,
                (1.0 - cw) / 2.0,
                1.0 + alpha,
                -2.0 * cw,
                1.0 - alpha,
            ),
            Kind::HighPass => Coefs::norm(
                (1.0 + cw) / 2.0,
                -(1.0 + cw),
                (1.0 + cw) / 2.0,
                1.0 + alpha,
                -2.0 * cw,
                1.0 - alpha,
            ),
            Kind::Peak => Coefs::norm(
                1.0 + alpha * a,
                -2.0 * cw,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cw,
                1.0 - alpha / a,
            ),
            Kind::LowShelf => Coefs::norm(
                a * ((a + 1.0) - (a - 1.0) * cw + sa),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
                a * ((a + 1.0) - (a - 1.0) * cw - sa),
                (a + 1.0) + (a - 1.0) * cw + sa,
                -2.0 * ((a - 1.0) + (a + 1.0) * cw),
                (a + 1.0) + (a - 1.0) * cw - sa,
            ),
            Kind::HighShelf => Coefs::norm(
                a * ((a + 1.0) + (a - 1.0) * cw + sa),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
                a * ((a + 1.0) + (a - 1.0) * cw - sa),
                (a + 1.0) - (a - 1.0) * cw + sa,
                2.0 * ((a - 1.0) - (a + 1.0) * cw),
                (a + 1.0) - (a - 1.0) * cw - sa,
            ),
        }
    }
}

/// One second-order section with gliding coefficients, for up to
/// [`MAX_CH`] channels.
#[derive(Debug, Clone)]
pub struct Biquad {
    cur: Coefs,
    target: Coefs,
    /// Glide factor per sample.
    k: f64,
    z: [[f64; 2]; MAX_CH],
    gliding: bool,
}

impl Biquad {
    pub fn new(c: Coefs, fs: f64) -> Biquad {
        Biquad {
            cur: c,
            target: c,
            k: 1.0 - (-1.0 / (0.005 * fs)).exp(),
            z: [[0.0; 2]; MAX_CH],
            gliding: false,
        }
    }

    /// Glides to `c` (click-free).
    pub fn set(&mut self, c: Coefs) {
        self.target = c;
        self.gliding = c != self.cur;
    }

    /// Jumps to `c` (only before audio starts).
    pub fn reset_to(&mut self, c: Coefs) {
        self.cur = c;
        self.target = c;
        self.gliding = false;
        self.z = [[0.0; 2]; MAX_CH];
    }

    pub fn coefs(&self) -> Coefs {
        self.cur
    }

    #[inline]
    fn glide(&mut self) {
        if !self.gliding {
            return;
        }
        let (c, t, k) = (&mut self.cur, self.target, self.k);
        c.b0 += k * (t.b0 - c.b0);
        c.b1 += k * (t.b1 - c.b1);
        c.b2 += k * (t.b2 - c.b2);
        c.a1 += k * (t.a1 - c.a1);
        c.a2 += k * (t.a2 - c.a2);
        let d = (t.b0 - c.b0).abs()
            + (t.b1 - c.b1).abs()
            + (t.b2 - c.b2).abs()
            + (t.a1 - c.a1).abs()
            + (t.a2 - c.a2).abs();
        if d < 1e-12 {
            *c = t;
            self.gliding = false;
        }
    }

    #[inline]
    fn tick(&mut self, ch: usize, x: f64) -> f64 {
        let c = &self.cur;
        let z = &mut self.z[ch];
        let y = c.b0 * x + z[0];
        z[0] = c.b1 * x - c.a1 * y + z[1];
        z[1] = c.b2 * x - c.a2 * y;
        y
    }

    /// Filters planar channels in place.
    pub fn process(&mut self, chans: &mut [&mut [f32]]) {
        let n = chans.first().map_or(0, |c| c.len());
        for i in 0..n {
            self.glide();
            for (ch, buf) in chans.iter_mut().enumerate().take(MAX_CH) {
                buf[i] = self.tick(ch, f64::from(buf[i]) + ANTI_DENORMAL) as f32;
            }
        }
    }
}

/// The channel EQ: a high-pass (12 or 24 dB/octave) and four bands.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct EqParams {
    /// High-pass corner (Hz), `None` for off.
    pub hpf_hz: Option<f32>,
    /// 24 dB/octave (two sections) instead of 12.
    pub hpf_steep: bool,
    pub bands: [Band; 4],
}

impl Default for EqParams {
    fn default() -> Self {
        EqParams {
            hpf_hz: None,
            hpf_steep: false,
            // Four off bands, spread across the range so the EQ editor shows
            // four distinct nodes rather than four stacked at 1 kHz.
            bands: [
                Band {
                    freq_hz: 100.0,
                    ..Band::default()
                },
                Band {
                    freq_hz: 500.0,
                    ..Band::default()
                },
                Band {
                    freq_hz: 2_000.0,
                    ..Band::default()
                },
                Band {
                    freq_hz: 8_000.0,
                    ..Band::default()
                },
            ],
        }
    }
}

/// Butterworth Qs: one section 0.707; two sections 0.541 and 1.307.
const HPF_Q: [[f32; 2]; 2] = [[0.707, 0.707], [0.541, 1.307]];

#[derive(Debug, Clone)]
pub struct Eq {
    fs: f64,
    params: EqParams,
    sections: [Biquad; 6],
}

impl Eq {
    pub fn new(params: EqParams, fs: f64) -> Eq {
        let mut eq = Eq {
            fs,
            params,
            sections: core::array::from_fn(|_| Biquad::new(Coefs::IDENTITY, fs)),
        };
        for (s, c) in eq.sections.iter_mut().zip(Self::targets(&params, fs)) {
            s.reset_to(c);
        }
        eq
    }

    fn targets(p: &EqParams, fs: f64) -> [Coefs; 6] {
        let mut t = [Coefs::IDENTITY; 6];
        if let Some(f) = p.hpf_hz {
            let qs = HPF_Q[usize::from(p.hpf_steep)];
            for (k, q) in qs.iter().enumerate().take(if p.hpf_steep { 2 } else { 1 }) {
                t[k] = Band {
                    kind: Kind::HighPass,
                    freq_hz: f,
                    gain_db: 0.0,
                    q: *q,
                }
                .coefs(fs);
            }
        }
        for (k, b) in p.bands.iter().enumerate() {
            t[2 + k] = b.coefs(fs);
        }
        t
    }

    pub fn params(&self) -> EqParams {
        self.params
    }

    /// The same filters at their current settings, at rest (no stored
    /// signal): runs a *difference* signal through the EQ, since a linear
    /// filter's response to x + d is its response to x plus that to d.
    /// No allocation.
    pub fn at_rest(&self) -> Eq {
        let mut e = self.clone();
        for s in &mut e.sections {
            s.z = [[0.0; 2]; MAX_CH];
            s.target = s.cur;
            s.gliding = false;
        }
        e
    }

    /// New settings, glided to (click-free). No allocation.
    pub fn set(&mut self, p: EqParams) {
        self.params = p;
        for (s, c) in self.sections.iter_mut().zip(Self::targets(&p, self.fs)) {
            s.set(c);
        }
    }

    /// True when every section is flat and staying flat (the EQ changes
    /// nothing and has no ringing).
    pub fn is_flat(&self) -> bool {
        self.sections
            .iter()
            .all(|s| s.cur == Coefs::IDENTITY && !s.gliding)
    }

    /// The response the EQ is heading to, at `hz` (for the UI curve).
    pub fn magnitude(&self, hz: f64) -> f64 {
        Self::targets(&self.params, self.fs)
            .iter()
            .map(|c| c.magnitude(hz, self.fs))
            .product()
    }

    pub fn process(&mut self, chans: &mut [&mut [f32]]) {
        for s in &mut self.sections {
            // Skip sections that are flat and staying flat.
            if s.cur == Coefs::IDENTITY && !s.gliding {
                continue;
            }
            s.process(chans);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    fn db(x: f64) -> f64 {
        20.0 * x.log10()
    }

    /// Steady-state gain of `eq` on a sine at `hz`, measured in the time
    /// domain (so it checks the processing, not only the coefficients).
    fn measured(eq: &mut Eq, hz: f64) -> f64 {
        let n = 48_000;
        let mut x: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * hz * i as f64 / FS).sin() as f32 * 0.25)
            .collect();
        eq.process(&mut [&mut x]);
        let tail = &x[n / 2..];
        let rms =
            (tail.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / tail.len() as f64).sqrt();
        rms / (0.25 / 2f64.sqrt())
    }

    #[test]
    fn peak_shelves_and_passes_hit_their_marks() {
        let b = Band::peak(1_000.0, 6.0, 1.0).coefs(FS);
        assert!((db(b.magnitude(1_000.0, FS)) - 6.0).abs() < 0.01);
        assert!(db(b.magnitude(100.0, FS)).abs() < 0.2);
        let cut = Band::peak(3_000.0, -9.0, 4.0).coefs(FS);
        assert!((db(cut.magnitude(3_000.0, FS)) + 9.0).abs() < 0.01);
        let ls = Band {
            kind: Kind::LowShelf,
            freq_hz: 200.0,
            gain_db: 4.0,
            q: 0.707,
        }
        .coefs(FS);
        assert!((db(ls.magnitude(20.0, FS)) - 4.0).abs() < 0.1);
        assert!(db(ls.magnitude(5_000.0, FS)).abs() < 0.05);
        let hs = Band {
            kind: Kind::HighShelf,
            freq_hz: 8_000.0,
            gain_db: -5.0,
            q: 0.707,
        }
        .coefs(FS);
        assert!((db(hs.magnitude(20_000.0, FS)) + 5.0).abs() < 0.3);
        assert!(db(hs.magnitude(500.0, FS)).abs() < 0.05);
        let hp = Band {
            kind: Kind::HighPass,
            freq_hz: 80.0,
            gain_db: 0.0,
            q: 0.707,
        }
        .coefs(FS);
        assert!(
            (db(hp.magnitude(80.0, FS)) + 3.01).abs() < 0.05,
            "-3 dB at the corner"
        );
        assert!(
            (db(hp.magnitude(40.0, FS)) + 12.3).abs() < 0.3,
            "12 dB/octave"
        );
    }

    #[test]
    fn the_eq_processes_what_its_curve_says() {
        let p = EqParams {
            hpf_hz: Some(100.0),
            hpf_steep: true,
            bands: [
                Band::peak(400.0, -4.0, 1.4),
                Band::peak(2_500.0, 3.0, 0.8),
                Band {
                    kind: Kind::HighShelf,
                    freq_hz: 10_000.0,
                    gain_db: 2.0,
                    q: 0.707,
                },
                Band::default(),
            ],
        };
        let eq = Eq::new(p, FS);
        for hz in [50.0, 100.0, 400.0, 1_000.0, 2_500.0, 12_000.0] {
            let mut e = Eq::new(p, FS);
            let (want, got) = (db(eq.magnitude(hz)), db(measured(&mut e, hz)));
            assert!(
                (want - got).abs() < 0.05,
                "{hz} Hz: curve {want:.2} dB, measured {got:.2} dB"
            );
        }
        // 24 dB/octave: an octave under the corner is about -24 dB, -3 at it.
        assert!((db(eq.magnitude(100.0)) + 3.0).abs() < 0.6);
        assert!(db(eq.magnitude(50.0)) < -22.0);
    }

    #[test]
    fn changes_glide_without_a_click_and_land_exactly() {
        let mut eq = Eq::new(EqParams::default(), FS);
        let mut x = vec![0.25f32; 4_800];
        eq.process(&mut [&mut x]);
        // Big change mid-stream: +12 dB low shelf on a DC-like signal.
        let mut p = EqParams::default();
        p.bands[0] = Band {
            kind: Kind::LowShelf,
            freq_hz: 300.0,
            gain_db: 12.0,
            q: 0.707,
        };
        eq.set(p);
        let mut y = vec![0.25f32; 9_600];
        eq.process(&mut [&mut y]);
        let jump = y
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            jump < 0.005,
            "largest step {jump}: it should glide, not jump"
        );
        let end = f64::from(*y.last().unwrap_or(&0.0)) / 0.25;
        assert!(
            (db(end) - 12.0).abs() < 0.05,
            "settles on the new curve: {:.2} dB",
            db(end)
        );
    }

    #[test]
    fn silence_stays_clean_and_stereo_channels_are_independent() {
        let mut eq = Eq::new(
            EqParams {
                hpf_hz: Some(20.0),
                hpf_steep: true,
                ..EqParams::default()
            },
            FS,
        );
        let mut l = vec![0.0f32; 48_000 * 5];
        let mut r = vec![0.0f32; 48_000 * 5];
        l[0] = 1.0;
        eq.process(&mut [&mut l, &mut r]);
        assert!(l.iter().all(|v| v.is_finite()));
        assert!(
            r.iter().all(|v| v.abs() < 1e-12),
            "right never saw the impulse"
        );
        assert!(l[48_000 * 4..].iter().all(|v| v.abs() < 1e-9), "rings out");
    }
}
