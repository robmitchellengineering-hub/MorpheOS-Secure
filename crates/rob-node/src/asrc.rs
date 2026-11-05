//! An asynchronous sample-rate converter: an input stream, a phase-locked read
//! position, and a band-limited interpolator. Used both ways in a node:
//!
//! * **receiver:** input = network-time samples, output = the DAC's frames;
//! * **sender:** input = the ADC's frames, output = network-time samples.
//!
//! [`Asrc::render`] does no allocation and takes no locks.

use crate::prelude::*;

use crate::interp::{Interpolator, Quality};
use crate::pll::{Tracker, TrackerConfig};
use crate::ring::PlanarRing;

/// What a render call ran into.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RenderReport {
    /// Output frames that could not be computed (input missing): written as 0.
    pub starved: usize,
    /// The read position jumped (a short fade-in was applied).
    pub resynced: bool,
    /// Input position read by output frame `k` is `origin + start + k * step`
    /// (what a late correction needs to land in the same place). `start` is
    /// relative to `origin`, see [`Asrc::origin`].
    pub origin: i64,
    pub start: f64,
    pub step: f64,
}

/// Frames over which a resync fades back in.
const RESYNC_FADE: usize = 64;

/// Positions are kept **relative to an integer origin** (the input index given
/// to [`Asrc::reset_input`]). Absolute network sample indices are huge (about
/// 10^10 to 10^14), and at that size an f64 cannot hold the fraction of a
/// sample the tracker needs: measured, a 0.25 ppm rate offset was tracked as
/// 0.29 ppm with 0.05 ppm wobble at 4.8e10, and exactly at 1e4.
pub struct Asrc {
    origin: i64,
    channels: usize,
    interp: Interpolator,
    tracker: Tracker,
    ring: PlanarRing,
    fade_left: usize,
}

impl Asrc {
    /// `ring_frames` must cover the latency budget plus a block plus the kernel.
    pub fn new(
        channels: usize,
        quality: Quality,
        tracker: TrackerConfig,
        ring_frames: usize,
    ) -> Asrc {
        Asrc {
            origin: 0,
            channels,
            interp: Interpolator::new(quality),
            tracker: Tracker::new(tracker),
            ring: PlanarRing::new(channels, ring_frames),
            fade_left: 0,
        }
    }

    /// Clears the input and makes `first` the next input index, and the
    /// origin that targets and positions are relative to.
    pub fn reset_input(&mut self, first: i64) {
        self.ring.reset(first);
        self.origin = first;
    }

    /// The absolute input index that positions and targets are relative to.
    pub fn origin(&self) -> i64 {
        self.origin
    }

    /// Appends one interleaved input frame.
    #[inline]
    pub fn push_frame(&mut self, frame: &[f32]) {
        self.ring.push_frame(frame);
    }

    /// One past the newest input index held.
    pub fn input_end(&self) -> i64 {
        self.ring.end()
    }

    /// The input index the *next* render will need up to (exclusive), for a
    /// block of `n` frames: the caller must have pushed input that far.
    pub fn input_needed(&self, n: usize) -> i64 {
        let worst_step = 1.0 + 3e-3;
        self.origin
            + (self.tracker.position() + n as f64 * worst_step).ceil() as i64
            + self.interp.lookahead() as i64
            + 1
    }

    /// Fills `out` (interleaved, `channels` per frame). `target` is the input
    /// position the first output frame should read, **relative to
    /// [`Asrc::origin`]**; see [`Tracker::plan`].
    pub fn render(&mut self, out: &mut [f32], target: Option<f64>) -> RenderReport {
        let frames = out.len() / self.channels;
        let plan = self.tracker.plan(frames, target);
        let mut rep = RenderReport {
            resynced: plan.resynced,
            origin: self.origin,
            start: plan.start,
            step: plan.step,
            ..RenderReport::default()
        };
        if plan.resynced {
            self.fade_left = RESYNC_FADE;
        }
        let history = self.interp.history() as i64;
        let taps = self.interp.taps();
        for k in 0..frames {
            let p = plan.start + k as f64 * plan.step;
            let i = p.floor();
            let frac = p - i;
            let first = self.origin + i as i64 - history;
            let gain = if self.fade_left > 0 {
                let g = 1.0 - self.fade_left as f32 / RESYNC_FADE as f32;
                self.fade_left -= 1;
                g
            } else {
                1.0
            };
            let mut ok = true;
            for c in 0..self.channels {
                out[k * self.channels + c] = match self.ring.window(c, first, taps) {
                    Some(w) => self.interp.read(w, frac) * gain,
                    None => {
                        ok = false;
                        0.0
                    }
                };
            }
            if !ok {
                rep.starved += 1;
            }
        }
        rep
    }

    pub fn tracker(&self) -> &Tracker {
        &self.tracker
    }

    /// See [`Tracker::set_feed_forward`].
    pub fn set_feed_forward(&mut self, ff: f64) {
        self.tracker.set_feed_forward(ff);
    }

    /// The first input index no rendered output has read yet: anything from
    /// here on can still be changed and every future read sees the change.
    pub fn first_unread(&self) -> i64 {
        self.origin + self.tracker.position().floor() as i64 + self.interp.lookahead() as i64 + 1
    }

    /// Adds `frame × weight` to input `index` if it is held and unread.
    /// No allocation.
    pub fn add_input(&mut self, index: i64, frame: &[f32], weight: f32) -> bool {
        index >= self.first_unread() && self.ring.add(index, frame, weight)
    }

    /// Frames still fading in after a resync (their gain is not 1).
    pub fn fading(&self) -> bool {
        self.fade_left > 0
    }

    /// Input samples of latency the interpolator adds.
    pub fn kernel_latency(&self) -> usize {
        self.interp.lookahead()
    }
}
