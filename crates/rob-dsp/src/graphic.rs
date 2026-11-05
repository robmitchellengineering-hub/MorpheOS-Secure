//! A 31-band (1/3-octave) graphic EQ for the master output bus (design 16).
//!
//! Each band is a constant-Q peaking filter, the classic graphic-EQ shape:
//! flat when every band is at 0 dB, and the band gains sum to the curve.
//! Coefficients glide toward a new gain exactly like the channel EQ, so a
//! slider move is click-free. Allocation-free while processing.

use crate::prelude::*;

use crate::filter::{Band, Biquad, MAX_CH};

/// Number of bands (1/3 octave, 20 Hz .. 20 kHz).
pub const GRAPHIC_BANDS: usize = 31;
/// Constant Q for a 1/3-octave graphic EQ (IEC/studio convention).
const GRAPHIC_Q: f32 = 4.32;
/// Per-band gain range (dB), symmetric. Rob (2026-11-05) wanted "dramatic cuts and
/// boosts to really be able to shape the room", so the range is +-24 dB — double the
/// studio +-12. The DSP itself applies whatever gain it is given (the sliders are the
/// range); a run of adjacent boosts can still reach the limiter, which is the guard, not
/// a silent clamp here.
pub const GRAPHIC_GAIN_DB: f32 = 24.0;

/// The 31 1/3-octave band centres (IEC 61260, base ten), 20 Hz .. 20 kHz.
pub fn graphic_centres() -> [f32; GRAPHIC_BANDS] {
    core::array::from_fn(|i| (1_000.0 * 10f64.powf((i as f64 - 17.0) / 10.0)) as f32)
}

/// The settings: one gain per band.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct GraphicParams {
    pub gains: [f32; GRAPHIC_BANDS],
}

impl Default for GraphicParams {
    fn default() -> Self {
        GraphicParams {
            gains: [0.0; GRAPHIC_BANDS],
        }
    }
}

/// The graphic EQ: a cascade of 31 constant-Q peaking filters.
#[derive(Debug, Clone)]
pub struct GraphicEq {
    fs: f64,
    p: GraphicParams,
    bands: Vec<Biquad>,
}

impl GraphicEq {
    /// Allocates once; `p` is the starting shape (jumped to, not glided, since
    /// audio has not started).
    pub fn new(p: GraphicParams, fs: f64) -> GraphicEq {
        let centres = graphic_centres();
        let mut g = GraphicEq {
            fs,
            p,
            bands: centres
                .iter()
                .map(|&f| Biquad::new(Band::peak(f, 0.0, GRAPHIC_Q).coefs(fs), fs))
                .collect(),
        };
        for (i, b) in g.bands.iter_mut().enumerate() {
            b.reset_to(Band::peak(centres[i], p.gains[i], GRAPHIC_Q).coefs(fs));
        }
        g
    }

    pub fn params(&self) -> GraphicParams {
        self.p
    }

    /// New gains, applied click-free (each band glides).
    pub fn set(&mut self, p: GraphicParams) {
        self.p = p;
        let centres = graphic_centres();
        for (i, b) in self.bands.iter_mut().enumerate() {
            b.set(Band::peak(centres[i], p.gains[i], GRAPHIC_Q).coefs(self.fs));
        }
    }

    /// The composite magnitude at `hz` in dB (for the UI's curve and tests).
    pub fn magnitude(&self, hz: f64) -> f64 {
        let mut db = 0.0;
        for b in &self.bands {
            db += 20.0 * b.coefs().magnitude(hz, self.fs).log10();
        }
        db
    }

    /// Filters planar channels in place.
    pub fn process(&mut self, chans: &mut [&mut [f32]]) {
        let _ = MAX_CH;
        for b in &mut self.bands {
            b.process(chans);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    #[test]
    fn flat_is_transparent() {
        let mut g = GraphicEq::new(GraphicParams::default(), FS);
        let x: Vec<f32> = (0..2_400)
            .map(|i| ((i * 7_919) % 2_000) as f32 / 1_000.0 - 1.0)
            .collect();
        let mut y = x.clone();
        g.process(&mut [&mut y]);
        let err = x
            .iter()
            .zip(&y)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(err < 1e-6, "{err}");
    }

    #[test]
    fn a_single_band_boosts_at_its_centre() {
        let mut p = GraphicParams::default();
        p.gains[17] = 6.0; // 1 kHz band
        let g = GraphicEq::new(p, FS);
        // +6 dB at the band centre, roughly flat far away.
        let centre = g.magnitude(1_000.0);
        let away = g.magnitude(40.0);
        assert!((centre - 6.0).abs() < 0.2, "{centre}");
        assert!(away.abs() < 0.2, "{away}");
    }
}
