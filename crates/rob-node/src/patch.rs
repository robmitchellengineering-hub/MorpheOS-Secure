//! "The DAC is the deadline" (PRIVATE: patent candidate, see
//! `docs/ip/patent-candidates.md`; never describe in public documents).
//!
//! The receiver writes audio for the sound card well ahead of the hardware
//! read pointer, remembering for every card frame which network-time position
//! the resampler read. A correction that arrives late (the true audio for a
//! period that was concealed, or an engine correction for a late input) is a
//! network-time *difference* signal `delta = truth - what was rendered`.
//! Because the resampler is linear, resampling `delta` at the same positions
//! gives exactly the change to make in the card buffer, so the correction is
//! added in place to every frame the hardware has not read yet (beyond a safety
//! margin):
//!
//! * if no affected frame has been read yet, the result is **exactly** what
//!   would have played had the audio been on time;
//! * if the hardware is already inside the affected span, the correction fades
//!   in over [`FADE`] frames from the first frame it may touch, so the switch
//!   from concealment to truth never clicks.
//!
//! On hardware this buffer *is* the device buffer (ALSA mmap on Linux and
//! Raspberry Pi, AAudio MMAP on Android, our own firmware); here it is a model
//! of it with an explicit read pointer. No allocation after `new`.

use crate::prelude::*;

use crate::interp::{Interpolator, Quality};

/// Frames over which a correction fades in when the hardware is already
/// inside the affected span.
pub const FADE: usize = 32;

/// What a correction did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Patch {
    /// Card frames changed.
    pub frames: usize,
    /// Affected card frames the hardware had already read (or that were inside
    /// the safety margin): the part of the correction that came too late.
    pub missed: usize,
}

impl Patch {
    /// Every affected frame was corrected: the output is as if on time.
    pub fn exact(&self) -> bool {
        self.missed == 0 && self.frames > 0
    }
}

/// Totals, for telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PatchStats {
    pub corrections: u64,
    pub exact: u64,
    pub partial: u64,
    /// Corrections that arrived after every affected frame had been read.
    pub too_late: u64,
    /// Corrections touching frames marked not patchable (resync fades).
    pub refused: u64,
}

pub struct PatchableOutput {
    channels: usize,
    interp: Interpolator,
    cap: usize,
    /// Interleaved card frames, ring of `cap`.
    buf: Vec<f32>,
    /// Network-time read position of each card frame: a whole sample index
    /// and a fraction (one f64 would lose the fraction at these magnitudes).
    pos_whole: Vec<i64>,
    pos_frac: Vec<f64>,
    patchable: Vec<bool>,
    /// Absolute card frame indices: next to write, next the hardware reads.
    write: u64,
    read: u64,
    margin: u64,
    /// One interpolation window per channel.
    window: Vec<f32>,
    pub stats: PatchStats,
}

impl PatchableOutput {
    /// `capacity` frames of device buffer; `margin` frames ahead of the read
    /// pointer are treated as already gone (DMA burst, scheduling safety).
    /// `quality` must match the resampler's.
    pub fn new(channels: usize, quality: Quality, capacity: usize, margin: usize) -> Self {
        let interp = Interpolator::new(quality);
        let taps = interp.taps();
        PatchableOutput {
            channels,
            interp,
            cap: capacity,
            buf: vec![0.0; capacity * channels],
            pos_whole: vec![0; capacity],
            pos_frac: vec![0.0; capacity],
            patchable: vec![false; capacity],
            write: 0,
            read: 0,
            margin: margin as u64,
            window: vec![0.0; taps],
            stats: PatchStats::default(),
        }
    }

    /// Frames written but not yet read.
    pub fn queued(&self) -> usize {
        (self.write - self.read) as usize
    }

    /// Room for more frames.
    pub fn space(&self) -> usize {
        self.cap - self.queued()
    }

    /// Appends a block the resampler rendered, with its read positions
    /// (`origin + start + k * step` for frame `k`, from the resampler's
    /// `RenderReport`). Mark `patchable = false` for blocks that went through
    /// a resync fade. Returns frames accepted.
    pub fn push_block(
        &mut self,
        frames: &[f32],
        origin: i64,
        start: f64,
        step: f64,
        patchable: bool,
    ) -> usize {
        let n = (frames.len() / self.channels).min(self.space());
        for k in 0..n {
            let at = (self.write % self.cap as u64) as usize;
            self.buf[at * self.channels..(at + 1) * self.channels]
                .copy_from_slice(&frames[k * self.channels..(k + 1) * self.channels]);
            let rel = start + k as f64 * step;
            let w = rel.floor();
            self.pos_whole[at] = origin + w as i64;
            self.pos_frac[at] = rel - w;
            self.patchable[at] = patchable;
            self.write += 1;
        }
        n
    }

    /// The hardware reads frames (here: copies them out). Returns frames read.
    pub fn read(&mut self, out: &mut [f32]) -> usize {
        let n = (out.len() / self.channels).min(self.queued());
        for k in 0..n {
            let at = (self.read % self.cap as u64) as usize;
            out[k * self.channels..(k + 1) * self.channels]
                .copy_from_slice(&self.buf[at * self.channels..(at + 1) * self.channels]);
            self.read += 1;
        }
        n
    }

    /// Adds a late correction. `delta` is interleaved network-time samples
    /// starting at network index `first`: what should have been rendered minus
    /// what was (zero outside). No allocation.
    pub fn correct(&mut self, first: i64, delta: &[f32]) -> Patch {
        let ch = self.channels;
        let len = (delta.len() / ch) as i64;
        let history = self.interp.history() as i64;
        let lookahead = self.interp.lookahead() as i64;
        // A card frame at position (i + frac) reads network samples
        // i - history ..= i + lookahead.
        let touches = |i: i64| i + lookahead >= first && i - history < first + len;
        let mut patch = Patch::default();
        let mut refused = false;
        // Frames the correction would touch but can no longer: already read,
        // or inside the safety margin. Positions only increase, so scan back
        // from the margin until they stop touching.
        let open = (self.read + self.margin).min(self.write);
        let oldest = self.write.saturating_sub(self.cap as u64);
        let mut f = open;
        while f > oldest {
            f -= 1;
            let at = (f % self.cap as u64) as usize;
            if touches(self.pos_whole[at]) {
                patch.missed += 1;
            } else if self.pos_whole[at] < first {
                break;
            }
        }
        let fade = patch.missed > 0;
        let mut faded = 0usize;
        for f in open..self.write {
            let at = (f % self.cap as u64) as usize;
            let i = self.pos_whole[at];
            if !touches(i) {
                if i >= first + len + history {
                    break;
                }
                continue;
            }
            if !self.patchable[at] {
                refused = true;
                continue;
            }
            let frac = self.pos_frac[at];
            let w0 = i - history;
            let gain = if fade {
                faded += 1;
                (faded as f32 / FADE as f32).min(1.0)
            } else {
                1.0
            };
            for c in 0..ch {
                for (t, w) in self.window.iter_mut().enumerate() {
                    let n = w0 + t as i64 - first;
                    *w = if (0..len).contains(&n) {
                        delta[n as usize * ch + c]
                    } else {
                        0.0
                    };
                }
                self.buf[at * ch + c] += gain * self.interp.read(&self.window, frac);
            }
            patch.frames += 1;
        }
        self.stats.corrections += 1;
        if refused {
            self.stats.refused += 1;
        }
        if patch.frames == 0 {
            self.stats.too_late += u64::from(patch.missed > 0);
        } else if patch.exact() {
            self.stats.exact += 1;
        } else {
            self.stats.partial += 1;
        }
        patch
    }
}
