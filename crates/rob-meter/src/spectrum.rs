//! Spectrum analysis: a running FFT analyser, the 1/3-octave RTA, and the
//! scrolling spectrogram.
//!
//! These run **off the audio thread**: the audio thread copies samples into a
//! lock-free queue; an analysis thread feeds them here. Everything allocates
//! once in `new`.
//!
//! **Calibration.** A full-scale sine reads 0 dBFS at its bin (Hann window,
//! coherent gain corrected). Between bins a Hann window under-reads by up to
//! 1.4 dB (scalloping); band readings (RTA) sum power and are not affected.
//!
//! **Low frequencies.** A 1/3-octave band at 20 Hz is under 5 Hz wide; an
//! 8192-point FFT at 48 kHz has 5.9 Hz bins. So the RTA runs a second
//! analyser on the signal low-passed and decimated by 16 (3 kHz rate,
//! 0.37 Hz bins) for every band under 160 Hz.

use crate::prelude::*;

use crate::fft::{Fft, C};

/// One running analyser: Hann-windowed frames every `hop` samples,
/// exponentially averaged power per bin.
#[derive(Debug, Clone)]
pub struct Analyzer {
    fs: f64,
    n: usize,
    hop: usize,
    fft: Fft,
    window: Vec<f64>,
    ring: Vec<f32>,
    head: usize,
    since: usize,
    filled: usize,
    frame: Vec<C>,
    /// Averaged power per bin (n/2 + 1), full scale sine = 1.
    pub power: Vec<f64>,
    /// Power of the latest frame alone (for the spectrogram).
    pub latest: Vec<f64>,
    avg: f64,
    pub frames: u64,
}

impl Analyzer {
    /// `n`-point FFT (power of two), a new frame every `hop` samples,
    /// averaging time constant `avg_s` (0: no averaging).
    pub fn new(fs: f64, n: usize, hop: usize, avg_s: f64) -> Analyzer {
        let window: Vec<f64> = (0..n)
            .map(|i| 0.5 - 0.5 * (core::f64::consts::TAU * i as f64 / n as f64).cos())
            .collect();
        let frame_rate = fs / hop as f64;
        Analyzer {
            fs,
            n,
            hop: hop.max(1),
            fft: Fft::new(n),
            window,
            ring: vec![0.0; n],
            head: 0,
            since: 0,
            filled: 0,
            frame: vec![C::default(); n],
            power: vec![0.0; n / 2 + 1],
            latest: vec![0.0; n / 2 + 1],
            avg: if avg_s > 0.0 {
                (-1.0 / (avg_s * frame_rate)).exp()
            } else {
                0.0
            },
            frames: 0,
        }
    }

    pub fn bin_hz(&self) -> f64 {
        self.fs / self.n as f64
    }

    /// Feeds samples; returns how many new frames were analysed.
    pub fn push(&mut self, x: &[f32]) -> usize {
        let mut made = 0;
        for &v in x {
            self.ring[self.head] = v;
            self.head = (self.head + 1) % self.n;
            self.filled = (self.filled + 1).min(self.n);
            self.since += 1;
            if self.since >= self.hop && self.filled == self.n {
                self.since = 0;
                self.analyse();
                made += 1;
            }
        }
        made
    }

    fn analyse(&mut self) {
        let n = self.n;
        // Hann coherent gain is 0.5: amplitude scale 2 / (n * 0.5).
        let scale = 4.0 / n as f64;
        for i in 0..n {
            let v = f64::from(self.ring[(self.head + i) % n]);
            self.frame[i] = C::new(v * self.window[i] * scale, 0.0);
        }
        self.fft.forward(&mut self.frame);
        let k = if self.frames == 0 { 0.0 } else { self.avg };
        for b in 0..=n / 2 {
            // Peak-amplitude squared of a sine in this bin; halve for RMS
            // power-like sums (bands use `band_power`).
            let p = self.frame[b].norm2();
            self.latest[b] = p;
            self.power[b] = k * self.power[b] + (1.0 - k) * p;
        }
        self.frames += 1;
    }

    /// Averaged level of bin `b` in dBFS (a full-scale sine at the bin: 0).
    pub fn bin_db(&self, b: usize) -> f64 {
        10.0 * self.power[b].max(1e-30).log10()
    }

    /// Power (full-scale sine = 1) between `lo` and `hi` Hz, from the
    /// averaged spectrum, splitting edge bins proportionally. Hann's energy
    /// spread is undone (equivalent noise bandwidth 1.5 bins).
    pub fn band_power(&self, lo: f64, hi: f64) -> f64 {
        let df = self.bin_hz();
        let mut p = 0.0;
        for (b, &v) in self.power.iter().enumerate() {
            let (a, z) = ((b as f64 - 0.5) * df, (b as f64 + 0.5) * df);
            let overlap = (z.min(hi) - a.max(lo)).max(0.0) / df;
            p += v * overlap;
        }
        p / 1.5
    }
}

/// The number of RTA bands: 121 = 1/12-octave, 20 Hz .. 20 kHz.
pub const RTA_BANDS: usize = 121;
/// Half a band in decades (40 bands per decade = 1/12 octave).
const HALF_BAND_DEC: f64 = 1.0 / 80.0;

/// 1/12-octave band centres (IEC 61260, base ten), 20 Hz .. 20 kHz.
pub fn band_centres() -> [f64; RTA_BANDS] {
    core::array::from_fn(|i| 20.0 * 10f64.powf(i as f64 / 40.0))
}

/// The real-time analyser: 121 1/12-octave bands, a full-rate analyser for
/// bands from 160 Hz up and a decimated one below.
#[derive(Debug, Clone)]
pub struct Rta {
    hi: Analyzer,
    lo: Analyzer,
    /// Anti-alias low-pass for the decimated path (4 cascaded biquads at
    /// 1.2 kHz) and its state.
    lp: [[f64; 5]; 4],
    lp_z: [[f64; 2]; 4],
    decim: usize,
    count: usize,
    lo_buf: Vec<f32>,
}

const DECIM: usize = 16;
const SPLIT_HZ: f64 = 150.0;

impl Rta {
    pub fn new(fs: f64, avg_s: f64) -> Rta {
        // Butterworth-ish low-pass at 1.2 kHz: four second-order sections
        // with the Qs of an eighth-order Butterworth.
        let qs = [0.5098, 0.6013, 0.9000, 2.5629];
        let fc = 1_200.0;
        let w0 = core::f64::consts::TAU * fc / fs;
        let lp = qs.map(|q| {
            let (c, s) = (w0.cos(), w0.sin());
            let al = s / (2.0 * q);
            let a0 = 1.0 + al;
            [
                (1.0 - c) / 2.0 / a0,
                (1.0 - c) / a0,
                (1.0 - c) / 2.0 / a0,
                -2.0 * c / a0,
                (1.0 - al) / a0,
            ]
        });
        Rta {
            hi: Analyzer::new(fs, 8_192, 2_048, avg_s),
            lo: Analyzer::new(fs / DECIM as f64, 8_192, 512, avg_s),
            lp,
            lp_z: [[0.0; 2]; 4],
            decim: DECIM,
            count: 0,
            lo_buf: vec![0.0; 1],
        }
    }

    pub fn push(&mut self, x: &[f32]) {
        self.hi.push(x);
        for &v in x {
            let mut y = f64::from(v);
            for (c, z) in self.lp.iter().zip(self.lp_z.iter_mut()) {
                let o = c[0] * y + z[0];
                z[0] = c[1] * y - c[3] * o + z[1];
                z[1] = c[2] * y - c[4] * o;
                y = o;
            }
            self.count += 1;
            if self.count == self.decim {
                self.count = 0;
                self.lo_buf[0] = y as f32;
                let b = core::mem::take(&mut self.lo_buf);
                self.lo.push(&b);
                self.lo_buf = b;
            }
        }
    }

    /// Band levels in dB (a full-scale sine in a band reads 0; bands' powers
    /// sum to the signal's).
    pub fn bands_db(&self) -> [f64; RTA_BANDS] {
        let c = band_centres();
        core::array::from_fn(|i| {
            let (lo, hi) = (
                c[i] * 10f64.powf(-HALF_BAND_DEC),
                c[i] * 10f64.powf(HALF_BAND_DEC),
            );
            let a = if c[i] < SPLIT_HZ { &self.lo } else { &self.hi };
            10.0 * a.band_power(lo, hi).max(1e-30).log10()
        })
    }

    pub fn analyser(&self) -> &Analyzer {
        &self.hi
    }
}

/// The lock-free tap between the audio thread and the spectrum analysis
/// (design 18's "next" item).
///
/// The audio thread calls [`Tap::write`]/[`Tap::write_stereo`] once per
/// period: a plain copy into a bounded ring, no lock, no allocation, no FFT —
/// it cannot add latency to the audio path. A separate analysis step calls
/// [`Tap::drain`], which feeds whatever is pending to the [`Rta`] (where the
/// FFT actually runs), and then reads [`Tap::bands_db`]. Single-producer,
/// single-consumer; if the consumer falls behind, the oldest samples are
/// dropped rather than ever growing the ring.
#[derive(Debug, Clone)]
pub struct Tap {
    rta: Rta,
    ring: Vec<f32>,
    write: usize,
    read: usize,
}

/// Ring capacity: ~0.7 s at 48 kHz, so the 100 ms analysis step never
/// overruns in the steady state.
const TAP_RING: usize = 32_768;

impl Tap {
    pub fn new(fs: f64) -> Tap {
        Tap {
            rta: Rta::new(fs, 0.5),
            ring: vec![0.0; TAP_RING],
            write: 0,
            read: 0,
        }
    }

    /// Audio thread: copies `x` into the ring. No lock, no allocation.
    pub fn write(&mut self, x: &[f32]) {
        for &v in x {
            self.ring[self.write] = v;
            self.write += 1;
            if self.write == self.ring.len() {
                self.write = 0;
            }
            if self.write == self.read {
                self.read += 1;
                if self.read == self.ring.len() {
                    self.read = 0;
                }
            }
        }
    }

    /// Audio thread: copies the mono sum of a stereo period into the ring
    /// (the spectrum is one channel), without a scratch buffer.
    pub fn write_stereo(&mut self, a: &[f32], b: &[f32]) {
        for (&l, &r) in a.iter().zip(b.iter()) {
            self.ring[self.write] = (l + r) * 0.5;
            self.write += 1;
            if self.write == self.ring.len() {
                self.write = 0;
            }
            if self.write == self.read {
                self.read += 1;
                if self.read == self.ring.len() {
                    self.read = 0;
                }
            }
        }
    }

    /// Analysis step: feeds the pending samples to the RTA (runs the FFT).
    pub fn drain(&mut self) {
        while self.read != self.write {
            if self.read < self.write {
                self.rta.push(&self.ring[self.read..self.write]);
                self.read = self.write;
            } else {
                self.rta.push(&self.ring[self.read..]);
                self.read = 0;
            }
        }
    }

    /// The 1/12-octave band levels (20 Hz .. 20 kHz) in dB.
    pub fn bands_db(&self) -> [f64; RTA_BANDS] {
        self.rta.bands_db()
    }
}

/// A scrolling spectrogram: rows of `width` log-spaced bins (20 Hz ..
/// 20 kHz) in dB, newest last. Allocates once.
#[derive(Debug, Clone)]
pub struct Spectrogram {
    width: usize,
    rows: usize,
    data: Vec<f32>,
    head: usize,
    count: usize,
}

impl Spectrogram {
    pub fn new(width: usize, rows: usize) -> Spectrogram {
        Spectrogram {
            width,
            rows,
            data: vec![-140.0; width * rows],
            head: 0,
            count: 0,
        }
    }

    /// Adds a row from an analyser's latest frame.
    pub fn add(&mut self, a: &Analyzer) {
        let df = a.bin_hz();
        let row = &mut self.data[self.head * self.width..(self.head + 1) * self.width];
        let (f0, f1) = (20f64.ln(), 20_000f64.ln());
        for (x, cell) in row.iter_mut().enumerate() {
            let lo = (f0 + (f1 - f0) * x as f64 / self.width as f64).exp();
            let hi = (f0 + (f1 - f0) * (x + 1) as f64 / self.width as f64).exp();
            let (b0, b1) = (
                (lo / df).floor() as usize,
                ((hi / df).ceil() as usize).max(1),
            );
            let p = a.latest[b0.min(a.latest.len() - 1)..b1.min(a.latest.len())]
                .iter()
                .cloned()
                .fold(0.0, f64::max);
            *cell = (10.0 * p.max(1e-14).log10()) as f32;
        }
        self.head = (self.head + 1) % self.rows;
        self.count += 1;
    }

    /// Row `k` back from the newest (0 = newest), if that many exist.
    pub fn row(&self, k: usize) -> Option<&[f32]> {
        if k >= self.rows || k >= self.count {
            return None;
        }
        let r = (self.head + self.rows - 1 - k) % self.rows;
        Some(&self.data[r * self.width..(r + 1) * self.width])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    fn sine(hz: f64, db: f64, n: usize) -> Vec<f32> {
        let a = 10f64.powf(db / 20.0);
        (0..n)
            .map(|i| (a * (core::f64::consts::TAU * hz * i as f64 / FS).sin()) as f32)
            .collect()
    }

    /// Pink noise (Paul Kellet's filter on white noise: +-0.05 dB from
    /// 9 Hz to Nyquist at 44.1 kHz, close enough here).
    fn pink(n: usize) -> Vec<f32> {
        let mut s = 0x2545_F491_4F6C_DD1Du64;
        let mut b = [0.0f64; 7];
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let w = (s >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
                b[0] = 0.99886 * b[0] + w * 0.0555179;
                b[1] = 0.99332 * b[1] + w * 0.0750759;
                b[2] = 0.96900 * b[2] + w * 0.1538520;
                b[3] = 0.86650 * b[3] + w * 0.3104856;
                b[4] = 0.55000 * b[4] + w * 0.5329522;
                b[5] = -0.7616 * b[5] - w * 0.0168980;
                let p = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
                b[6] = w * 0.115926;
                (p * 0.05) as f32
            })
            .collect()
    }

    #[test]
    fn a_sine_reads_its_level_at_its_bin() {
        let mut a = Analyzer::new(FS, 8_192, 2_048, 0.0);
        let bin = 171; // exactly on a bin
        let hz = bin as f64 * FS / 8_192.0;
        a.push(&sine(hz, -6.0, 16_384));
        assert!((a.bin_db(bin) + 6.0).abs() < 0.05, "{:.3}", a.bin_db(bin));
    }

    #[test]
    fn rta_pink_noise_is_flat_and_a_tone_lands_in_its_band() {
        let x = pink(48_000 * 40);
        let mut r = Rta::new(FS, 10.0);
        for c in x.chunks(4_800) {
            r.push(c);
        }
        let b = r.bands_db();
        let mid: Vec<f64> = b[4..117].to_vec(); // 25 Hz .. 16 kHz
        let mean = mid.iter().sum::<f64>() / mid.len() as f64;
        for (i, v) in mid.iter().enumerate() {
            assert!(
                (v - mean).abs() < 1.5,
                "band {:.0} Hz: {:.2} dB vs mean {:.2}",
                band_centres()[i + 4],
                v,
                mean
            );
        }
        // A 1 kHz tone at -10 dB: its band reads -10 (bands sum power), the
        // neighbours are far below.
        let mut r = Rta::new(FS, 0.0);
        r.push(&sine(1_000.0, -10.0, 48_000));
        let b = r.bands_db();
        let k = 68; // 1 kHz
        assert!((b[k] + 10.0).abs() < 0.3, "1 kHz band {:.2}", b[k]);
        assert!(
            b[k - 1] < b[k] - 20.0 && b[k + 1] < b[k] - 20.0,
            "{:?}",
            &b[k - 1..=k + 1]
        );
        // And a 40 Hz tone uses the decimated path, in the 40 Hz band.
        let mut r = Rta::new(FS, 0.0);
        r.push(&sine(40.0, -10.0, 48_000 * 4));
        let b = r.bands_db();
        let k = 12; // 40 Hz
        assert!((b[k] + 10.0).abs() < 0.5, "40 Hz band {:.2}", b[k]);
        assert!(
            b[k - 1] < b[k] - 15.0 && b[k + 1] < b[k] - 15.0,
            "{:?}",
            &b[k - 1..=k + 1]
        );
    }

    #[test]
    fn the_tap_streams_stereo_periods_to_the_rta() {
        // The audio thread writes stereo periods (no FFT); the analysis step
        // drains (FFT). A 1 kHz tone at -10 dB in both channels reads -10 dB
        // in its band.
        let mut t = Tap::new(FS);
        let n = 48_000 * 2;
        let l = sine(1_000.0, -10.0, n);
        let r = sine(1_000.0, -10.0, n);
        for c in 0..(n / 192) {
            t.write_stereo(&l[c * 192..(c + 1) * 192], &r[c * 192..(c + 1) * 192]);
            t.drain();
        }
        let b = t.bands_db();
        let k = 68; // 1 kHz
        assert!((b[k] + 10.0).abs() < 1.0, "1 kHz band {:.2}", b[k]);
        assert!(
            b[k - 1] < b[k] - 20.0,
            "neighbour too high: {:.2}",
            b[k - 1]
        );
    }

    #[test]
    fn spectrogram_rows_scroll() {
        let mut a = Analyzer::new(FS, 4_096, 1_024, 0.0);
        let mut s = Spectrogram::new(128, 50);
        a.push(&sine(1_000.0, -6.0, 4_096));
        s.add(&a);
        a.push(&sine(5_000.0, -6.0, 4_096));
        s.add(&a);
        let (new, old) = (s.row(0).unwrap_or(&[]), s.row(1).unwrap_or(&[]));
        let peak = |r: &[f32]| {
            r.iter()
                .enumerate()
                .fold((0, f32::MIN), |m, (i, &v)| if v > m.1 { (i, v) } else { m })
                .0
        };
        // Log axis: 1 kHz sits at x = ln(50)/ln(1000) of the width, 5 kHz further right.
        let x1k = (128.0 * (1_000f64 / 20.0).ln() / (1_000f64).ln()) as usize;
        assert!(
            peak(old).abs_diff(x1k) <= 1,
            "old row peak {} want {x1k}",
            peak(old)
        );
        assert!(peak(new) > peak(old), "newest row shows the higher tone");
        assert!(s.row(2).is_none());
    }
}
