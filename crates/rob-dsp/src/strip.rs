//! The channel strip: trim and polarity, gate, EQ (with its high-pass),
//! compressor, in that order (the usual console order: the gate sees the
//! untouched level, the compressor sees the EQ'd sound). Mono or linked
//! stereo. Faders, pans and sends are the matrix's crosspoints; the strip is
//! everything before them.

use crate::prelude::*;

use crate::dynamics::{CompParams, Compressor, Gate, GateParams};
use crate::filter::{Eq, EqParams};

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct StripParams {
    /// Input trim (dB), -24 .. +24.
    pub trim_db: f32,
    /// Flip polarity.
    pub invert: bool,
    /// Silence the channel (glides to and from silence: no click).
    pub mute: bool,
    pub gate: GateParams,
    pub eq: EqParams,
    pub comp: CompParams,
}

impl Default for StripParams {
    fn default() -> Self {
        StripParams {
            trim_db: 0.0,
            invert: false,
            mute: false,
            gate: GateParams::default(),
            eq: EqParams::default(),
            comp: CompParams::default(),
        }
    }
}

/// Starting points, so a good mix is quick to reach (the same on every
/// engine, main or backup): `speech` (presenters, comms), `vocal` (singing),
/// `music` (a stereo programme), `flat`.
pub fn preset(name: &str) -> Option<StripParams> {
    use crate::dynamics::Detector;
    use crate::filter::{Band, Kind};
    let shelf = |kind, freq_hz, gain_db| Band {
        kind,
        freq_hz,
        gain_db,
        q: 0.707,
    };
    let flat = StripParams::default();
    Some(match name {
        "flat" => flat,
        "speech" => StripParams {
            gate: GateParams {
                on: true,
                threshold_db: -50.0,
                range_db: -20.0,
                ..GateParams::default()
            },
            eq: EqParams {
                hpf_hz: Some(100.0),
                hpf_steep: true,
                bands: [
                    Band::peak(300.0, -2.0, 1.0),
                    Band::peak(3_500.0, 3.0, 1.0),
                    shelf(Kind::HighShelf, 10_000.0, 1.5),
                    Band::default(),
                ],
            },
            comp: CompParams {
                on: true,
                threshold_db: -20.0,
                ratio: 3.0,
                knee_db: 6.0,
                attack_ms: 5.0,
                release_ms: 100.0,
                makeup_db: 3.0,
                detector: Detector::Peak,
            },
            ..flat
        },
        "vocal" => StripParams {
            eq: EqParams {
                hpf_hz: Some(90.0),
                hpf_steep: true,
                bands: [
                    Band::peak(400.0, -2.0, 1.2),
                    Band::peak(3_000.0, 2.5, 0.9),
                    shelf(Kind::HighShelf, 10_000.0, 2.0),
                    Band::default(),
                ],
            },
            comp: CompParams {
                on: true,
                threshold_db: -18.0,
                ratio: 3.5,
                knee_db: 6.0,
                attack_ms: 8.0,
                release_ms: 150.0,
                makeup_db: 3.0,
                detector: Detector::Rms,
            },
            ..flat
        },
        "music" => StripParams {
            eq: EqParams {
                hpf_hz: Some(30.0),
                ..EqParams::default()
            },
            comp: CompParams {
                on: true,
                threshold_db: -12.0,
                ratio: 2.0,
                knee_db: 10.0,
                attack_ms: 30.0,
                release_ms: 250.0,
                makeup_db: 1.0,
                detector: Detector::Rms,
            },
            ..flat
        },
        _ => return None,
    })
}

/// What the meters show.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StripMeters {
    pub gate_open: bool,
    pub gate_db: f32,
    pub comp_gr_db: f32,
}

#[derive(Debug, Clone)]
pub struct ChannelStrip {
    fs: f64,
    p: StripParams,
    trim: f32,
    trim_target: f32,
    glide: f32,
    gate: Gate,
    eq: Eq,
    comp: Compressor,
}

impl ChannelStrip {
    /// Allocates nothing after this.
    pub fn new(p: StripParams, fs: f64) -> ChannelStrip {
        let t = Self::trim_gain(&p);
        ChannelStrip {
            fs,
            p,
            trim: t,
            trim_target: t,
            glide: (1.0 - (-1.0 / (0.005 * fs)).exp()) as f32,
            gate: Gate::new(p.gate, fs),
            eq: Eq::new(p.eq, fs),
            comp: Compressor::new(p.comp, fs),
        }
    }

    fn trim_gain(p: &StripParams) -> f32 {
        if p.mute {
            return 0.0;
        }
        let g = 10f32.powf(p.trim_db.clamp(-24.0, 24.0) / 20.0);
        if p.invert {
            -g
        } else {
            g
        }
    }

    pub fn sample_rate(&self) -> f64 {
        self.fs
    }

    pub fn params(&self) -> StripParams {
        self.p
    }

    /// New settings, applied click-free. No allocation.
    pub fn set(&mut self, p: StripParams) {
        self.p = p;
        self.trim_target = Self::trim_gain(&p);
        self.gate.set(p.gate);
        self.eq.set(p.eq);
        self.comp.set(p.comp);
    }

    pub fn meters(&self) -> StripMeters {
        StripMeters {
            gate_open: self.gate.is_open(),
            gate_db: self.gate.gain_db(),
            comp_gr_db: self.comp.gain_reduction_db(),
        }
    }

    /// The strip's linear part right now, for running a correction (a
    /// difference signal) through it: the trim and polarity, the dynamics'
    /// current gain, and the EQ at rest. Exact for a flat strip and for EQ
    /// alone (up to its ringing beyond the correction); approximate through
    /// the compressor and gate, whose gain would have moved a little with the
    /// true signal. No allocation.
    pub fn linear_part(&self) -> LinearPart {
        let dyn_db = f64::from(self.gate.gain_db()) + f64::from(self.comp.gain_reduction_db());
        let makeup = if self.p.comp.on {
            f64::from(self.p.comp.makeup_db)
        } else {
            0.0
        };
        LinearPart {
            gain: self.trim * 10f64.powf((dyn_db + makeup) / 20.0) as f32,
            eq: self.eq.at_rest(),
        }
    }

    /// Whether the strip's EQ rings on after a change in its input (so a
    /// correction should carry that ringing).
    pub fn rings(&self) -> bool {
        !self.eq.is_flat()
    }

    /// The EQ curve (for the UI).
    pub fn eq_magnitude(&self, hz: f64) -> f64 {
        self.eq.magnitude(hz)
    }

    /// Processes planar mono or stereo in place.
    pub fn process(&mut self, chans: &mut [&mut [f32]]) {
        let n = chans.first().map_or(0, |c| c.len());
        if self.trim != self.trim_target || self.trim != 1.0 {
            for i in 0..n {
                // A polarity flip glides through zero over ~5 ms: no click.
                self.trim += self.glide * (self.trim_target - self.trim);
                if (self.trim - self.trim_target).abs() < 1e-7 {
                    self.trim = self.trim_target;
                }
                for c in chans.iter_mut() {
                    c[i] *= self.trim;
                }
            }
        }
        self.gate.process(chans);
        self.eq.process(chans);
        self.comp.process(chans);
    }
}

/// See [`ChannelStrip::linear_part`].
#[derive(Debug, Clone)]
pub struct LinearPart {
    pub gain: f32,
    eq: Eq,
}

impl LinearPart {
    pub fn process(&mut self, chans: &mut [&mut [f32]]) {
        for c in chans.iter_mut() {
            for v in c.iter_mut() {
                *v *= self.gain;
            }
        }
        self.eq.process(chans);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::Band;

    const FS: f64 = 48_000.0;

    #[test]
    fn presets_exist_and_are_sane() {
        for name in ["flat", "speech", "vocal", "music"] {
            let p = preset(name).unwrap_or_else(|| panic!("{name}"));
            let s = ChannelStrip::new(p, FS);
            // No preset boosts the mids by more than a few dB, or cuts the
            // voice range away.
            let g = 20.0 * s.eq_magnitude(1_000.0).log10();
            assert!(g.abs() < 3.0, "{name}: {g:.1} dB at 1 kHz");
        }
        assert!(preset("nonsense").is_none());
    }

    #[test]
    fn mute_glides_to_silence_and_back() {
        let mut s = ChannelStrip::new(StripParams::default(), FS);
        let mut p = s.params();
        p.mute = true;
        s.set(p);
        let mut y = vec![0.5f32; 4_800];
        s.process(&mut [&mut y]);
        let step = y
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(step < 0.01, "no click going silent: {step}");
        assert!(y[4_799].abs() < 1e-6, "silent: {}", y[4_799]);
        p.mute = false;
        s.set(p);
        let mut z = vec![0.5f32; 4_800];
        s.process(&mut [&mut z]);
        assert!((z[4_799] - 0.5).abs() < 1e-4, "back: {}", z[4_799]);
    }

    #[test]
    fn a_flat_strip_is_transparent() {
        let mut s = ChannelStrip::new(StripParams::default(), FS);
        let x: Vec<f32> = (0..4_800)
            .map(|i| ((i * 7919) % 2000) as f32 / 1000.0 - 1.0)
            .collect();
        let mut y = x.clone();
        s.process(&mut [&mut y]);
        let err = x
            .iter()
            .zip(&y)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(err < 1e-6, "{err}");
    }

    #[test]
    fn trim_polarity_and_order() {
        let mut p = StripParams {
            trim_db: 6.0,
            invert: true,
            ..StripParams::default()
        };
        p.eq.bands[0] = Band::peak(1_000.0, 0.0, 1.0);
        let mut s = ChannelStrip::new(p, FS);
        let mut y = vec![0.25f32; 4_800];
        s.process(&mut [&mut y]);
        assert!(
            (y[4_799] + 0.25 * 10f32.powf(0.3)).abs() < 1e-4,
            "{}",
            y[4_799]
        );
        // Flip polarity back: glides through zero, no step bigger than the
        // glide allows.
        p.invert = false;
        s.set(p);
        let mut z = vec![0.25f32; 4_800];
        s.process(&mut [&mut z]);
        let step = z
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(step < 0.01, "{step}");
        assert!((z[4_799] - 0.25 * 10f32.powf(0.3)).abs() < 1e-4);
    }
}
