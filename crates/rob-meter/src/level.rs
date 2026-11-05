//! Level meters with their classic ballistics: sample peak with hold, PPM
//! (quasi-peak) in the usual flavours, and VU.
//!
//! **PPM.** A quasi-peak meter: rises fast but not instantly (a short burst
//! reads a little under its level, by design: the "integration time"), falls
//! slowly at a fixed rate in decibels. The presets use the commonly published
//! figures for IEC 60268-10 types; **verify against the standard's text
//! before claiming compliance** (marked in the design note).
//!
//! **VU.** An averaging meter: a full-wave rectified signal through a
//! second-order response that reaches 99 % of a steady tone's reading in
//! 300 ms with a little (1.5 %) overshoot (IEC 60268-17 / ANSI C16.5).
//! 0 VU is set by a reference level (commonly a sine at -18 or -20 dBFS).
//!
//! Every meter: allocation-free, one `process` call per block, cheap.

/// Sample peak with a hold and a dB-linear fall (for the bar).
use crate::prelude::*;

#[derive(Debug, Clone)]
pub struct PeakMeter {
    fs: f64,
    level: f64,
    hold: f64,
    hold_left: u64,
    hold_samples: u64,
    fall_per_sample: f64,
    /// Largest sample since reset (for "over" indication).
    pub max: f64,
}

impl PeakMeter {
    /// `hold_s` before the held peak falls; the bar falls at `fall_db_s`.
    pub fn new(fs: f64, hold_s: f64, fall_db_s: f64) -> PeakMeter {
        PeakMeter {
            fs,
            level: 0.0,
            hold: 0.0,
            hold_left: 0,
            hold_samples: (hold_s * fs) as u64,
            fall_per_sample: 10f64.powf(-fall_db_s / 20.0 / fs),
            max: 0.0,
        }
    }

    pub fn process(&mut self, x: &[f32]) {
        for &v in x {
            let a = f64::from(v.abs());
            self.level = if a > self.level {
                a
            } else {
                self.level * self.fall_per_sample
            };
            if a >= self.hold {
                self.hold = a;
                self.hold_left = self.hold_samples;
            } else if self.hold_left > 0 {
                self.hold_left -= 1;
            } else {
                self.hold *= self.fall_per_sample;
            }
            self.max = self.max.max(a);
        }
    }

    /// (bar, held peak) in dBFS.
    pub fn read_db(&self) -> (f64, f64) {
        (to_db(self.level), to_db(self.hold))
    }

    pub fn sample_rate(&self) -> f64 {
        self.fs
    }
}

fn to_db(x: f64) -> f64 {
    20.0 * x.max(1e-10).log10()
}

/// PPM ballistics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PpmSpec {
    pub name: &'static str,
    /// A tone burst this long reads `burst_db` under its steady level.
    pub burst_ms: f64,
    pub burst_db: f64,
    /// Falls `fall_db` in `fall_s`.
    pub fall_db: f64,
    pub fall_s: f64,
}

impl PpmSpec {
    /// DIN 45406 / IEC 60268-10 Type I: 5 ms burst -2 dB; 20 dB in 1.5 s.
    pub const DIN: PpmSpec = PpmSpec {
        name: "DIN",
        burst_ms: 5.0,
        burst_db: -2.0,
        fall_db: 20.0,
        fall_s: 1.5,
    };
    /// Nordic (Type I): 5 ms burst -2 dB; 20 dB in 1.7 s.
    pub const NORDIC: PpmSpec = PpmSpec {
        name: "Nordic",
        burst_ms: 5.0,
        burst_db: -2.0,
        fall_db: 20.0,
        fall_s: 1.7,
    };
    /// BBC and EBU (Type II): 10 ms burst -2.5 dB; 24 dB in 2.8 s.
    pub const BBC: PpmSpec = PpmSpec {
        name: "BBC",
        burst_ms: 10.0,
        burst_db: -2.5,
        fall_db: 24.0,
        fall_s: 2.8,
    };
    pub const EBU: PpmSpec = PpmSpec {
        name: "EBU",
        ..PpmSpec::BBC
    };
}

#[derive(Debug, Clone)]
pub struct PpmMeter {
    spec: PpmSpec,
    attack: f64,
    fall: f64,
    level: f64,
    /// Scales the reading so a steady 1 kHz tone reads its peak level.
    cal: f64,
}

/// Runs the PPM's envelope over a sine of `hz` for `seconds` from silence.
fn ppm_envelope(attack: f64, fall: f64, fs: f64, hz: f64, seconds: f64) -> f64 {
    let mut level = 0.0f64;
    let w = core::f64::consts::TAU * hz / fs;
    for i in 0..(seconds * fs).round() as usize {
        let a = (w * i as f64).sin().abs();
        level = if a > level {
            attack * level + (1.0 - attack) * a
        } else {
            level * fall
        };
    }
    level
}

impl PpmMeter {
    /// Calibrates itself (a few milliseconds of simulation, here only): the
    /// attack is chosen so a 5 kHz burst of `burst_ms` reads `burst_db` under
    /// the same tone held steady, as the standards test it; then the scale so
    /// a steady 1 kHz tone reads its peak. (A quasi-peak envelope settles a
    /// little under a steady tone's peak, where its top-up at each crest
    /// matches its sag between crests: about 0.3 dB uncalibrated.)
    pub fn new(spec: PpmSpec, fs: f64) -> PpmMeter {
        let fall = 10f64.powf(-spec.fall_db / spec.fall_s / 20.0 / fs);
        let want = 10f64.powf(spec.burst_db / 20.0);
        let ratio = |tau: f64| {
            let a = (-1.0 / tau).exp();
            ppm_envelope(a, fall, fs, 5_000.0, spec.burst_ms * 1e-3)
                / ppm_envelope(a, fall, fs, 5_000.0, 0.3)
        };
        // Longer attack time constant: the burst reads lower.
        let (mut lo, mut hi) = (1.0f64, 20_000.0f64);
        for _ in 0..50 {
            let mid = (lo * hi).sqrt();
            if ratio(mid) > want {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let attack = (-1.0 / (lo * hi).sqrt()).exp();
        let cal = 1.0 / ppm_envelope(attack, fall, fs, 1_000.0, 0.5);
        PpmMeter {
            spec,
            attack,
            fall,
            level: 0.0,
            cal,
        }
    }

    pub fn spec(&self) -> PpmSpec {
        self.spec
    }

    pub fn process(&mut self, x: &[f32]) {
        for &v in x {
            let a = f64::from(v.abs());
            self.level = if a > self.level {
                self.attack * self.level + (1.0 - self.attack) * a
            } else {
                self.level * self.fall
            };
        }
    }

    /// Reading in dBFS (calibrated: a steady 1 kHz sine reads its peak).
    pub fn read_db(&self) -> f64 {
        to_db(self.level * self.cal)
    }
}

#[derive(Debug, Clone)]
pub struct VuMeter {
    /// Second-order response state (position, velocity) and its constants.
    y: f64,
    v: f64,
    w2: f64,
    two_zw: f64,
    dt: f64,
    /// RMS of the 0 VU reference sine.
    reference: f64,
}

impl VuMeter {
    /// 0 VU = a sine whose peak is at `reference_dbfs` (e.g. -18).
    pub fn new(fs: f64, reference_dbfs: f64) -> VuMeter {
        // Damping for 1.5 % overshoot, then the natural frequency for which
        // the step response first reaches 99 % at 300 ms.
        let os: f64 = 0.015;
        let z = -os.ln() / (core::f64::consts::PI.powi(2) + os.ln().powi(2)).sqrt();
        let t99 = |wn: f64| {
            let wd = wn * (1.0 - z * z).sqrt();
            let mut t = 0.0;
            loop {
                t += 1e-4;
                let y = 1.0
                    - (-z * wn * t).exp()
                        * ((wd * t).cos() + z / (1.0 - z * z).sqrt() * (wd * t).sin());
                if y >= 0.99 || t > 5.0 {
                    return t;
                }
            }
        };
        let (mut lo, mut hi) = (1.0, 200.0);
        for _ in 0..60 {
            let mid = 0.5 * (lo + hi);
            if t99(mid) > 0.3 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let wn = 0.5 * (lo + hi);
        VuMeter {
            y: 0.0,
            v: 0.0,
            w2: wn * wn,
            two_zw: 2.0 * z * wn,
            dt: 1.0 / fs,
            reference: 10f64.powf(reference_dbfs / 20.0) / 2f64.sqrt(),
        }
    }

    pub fn process(&mut self, x: &[f32]) {
        // Average-responding, scaled to read a sine's RMS.
        let scale = core::f64::consts::PI / (2.0 * 2f64.sqrt());
        for &s in x {
            let u = f64::from(s.abs()) * scale;
            // Semi-implicit Euler is stable and accurate at audio rates.
            self.v += (self.w2 * (u - self.y) - self.two_zw * self.v) * self.dt;
            self.y += self.v * self.dt;
        }
    }

    /// Reading in VU (dB relative to the reference).
    pub fn read_vu(&self) -> f64 {
        to_db(self.y.max(0.0) / self.reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    fn tone(db: f64, seconds: f64) -> Vec<f32> {
        let a = 10f64.powf(db / 20.0);
        (0..(seconds * FS) as usize)
            .map(|i| (a * (core::f64::consts::TAU * 5_000.0 * i as f64 / FS).sin()) as f32)
            .collect()
    }

    #[test]
    fn peak_meter_holds_then_falls() {
        let mut m = PeakMeter::new(FS, 1.0, 20.0);
        m.process(&tone(-6.0, 0.1));
        let (bar, held) = m.read_db();
        assert!((bar + 6.0).abs() < 0.1 && (held + 6.0).abs() < 0.1);
        m.process(&vec![0.0; 24_000]);
        let (bar, held) = m.read_db();
        assert!((held + 6.0).abs() < 0.1, "held for 1 s: {held}");
        assert!((bar + 16.0).abs() < 0.2, "bar fell 10 dB in 0.5 s: {bar}");
        m.process(&vec![0.0; 48_000]);
        assert!(m.read_db().1 < -15.0, "then the hold falls too");
    }

    fn tone_at(db: f64, hz: f64, seconds: f64) -> Vec<f32> {
        let a = 10f64.powf(db / 20.0);
        (0..(seconds * FS) as usize)
            .map(|i| (a * (core::f64::consts::TAU * hz * i as f64 / FS).sin()) as f32)
            .collect()
    }

    #[test]
    fn ppm_burst_and_fall() {
        for spec in [PpmSpec::DIN, PpmSpec::BBC, PpmSpec::NORDIC] {
            // Calibrated on a steady 1 kHz tone.
            let mut m = PpmMeter::new(spec, FS);
            m.process(&tone_at(0.0, 1_000.0, 1.0));
            assert!(
                m.read_db().abs() < 0.1,
                "{}: 1 kHz reads {:.3}",
                spec.name,
                m.read_db()
            );
            // Bursts of 5 kHz (as the standard tests them) read `burst_db`
            // under the same tone held steady. (Held steady, 5 kHz itself
            // reads ~0.3 dB low on samples, which miss crests: noted in the
            // design note.)
            let mut steady = PpmMeter::new(spec, FS);
            steady.process(&tone_at(0.0, 5_000.0, 1.0));
            let mut m = PpmMeter::new(spec, FS);
            m.process(&tone_at(0.0, 5_000.0, spec.burst_ms * 1e-3));
            let rel = m.read_db() - steady.read_db();
            assert!(
                (rel - spec.burst_db).abs() < 0.05,
                "{}: burst reads {rel:.2}",
                spec.name
            );
            // Fall.
            let mut m = PpmMeter::new(spec, FS);
            m.process(&tone_at(0.0, 1_000.0, 1.0));
            m.process(&vec![0.0; (spec.fall_s * FS) as usize]);
            assert!(
                (m.read_db() + spec.fall_db).abs() < 0.2,
                "{}: fell to {:.2}",
                spec.name,
                m.read_db()
            );
        }
    }

    #[test]
    fn vu_reference_rise_time_and_overshoot() {
        let mut m = VuMeter::new(FS, -18.0);
        m.process(&tone(-18.0, 2.0));
        assert!(
            m.read_vu().abs() < 0.05,
            "reference sine reads 0 VU: {}",
            m.read_vu()
        );
        // Rise: 99 % of the steady reading (linear) at 300 ms, then a small
        // overshoot that settles.
        let mut m = VuMeter::new(FS, -18.0);
        let x = tone(-18.0, 1.0);
        let mut readings = Vec::new();
        for c in x.chunks(48) {
            m.process(c);
            readings.push(10f64.powf(m.read_vu() / 20.0));
        }
        let at_300 = readings[299];
        assert!((at_300 - 0.99).abs() < 0.02, "at 300 ms: {at_300:.3}");
        let peak = readings.iter().cloned().fold(0.0, f64::max);
        assert!(
            peak > 1.005 && peak < 1.03,
            "overshoot {:.1} %",
            (peak - 1.0) * 100.0
        );
    }
}
