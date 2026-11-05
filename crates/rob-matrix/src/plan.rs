//! What the control thread hands the audio thread: compiled plans, gain ramps and
//! the counters the audio thread publishes back.

use crate::prelude::*;

use core::sync::atomic::AtomicU64;

/// One gain, moving linearly to its target. Lives in a fixed slot on the audio
/// thread, so its state survives plan swaps.
///
/// `cur` is the gain of the last sample rendered; sample `i` of the next block
/// gets `cur + step * (i + 1)` while the ramp lasts, then `target` exactly.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Ramp {
    pub cur: f32,
    pub target: f32,
    pub step: f32,
    pub left: u32,
}

impl Ramp {
    /// Starts a ramp from wherever the gain is now.
    pub fn set(&mut self, target: f32, frames: u32) {
        self.target = target;
        if frames == 0 || target == self.cur {
            self.cur = target;
            self.step = 0.0;
            self.left = 0;
        } else {
            self.step = (target - self.cur) / frames as f32;
            self.left = frames;
        }
    }

    /// Jumps to `gain` with no ramp (only for slots no plan is playing yet).
    pub fn reset(&mut self, gain: f32) {
        *self = Ramp {
            cur: gain,
            target: gain,
            step: 0.0,
            left: 0,
        };
    }

    /// Gain for sample `i` of the current block. Every reader of a slot in a
    /// block gets bit-identical values, which is what makes mix-minus exact.
    #[inline]
    pub fn at(&self, i: usize) -> f32 {
        if (i as u64) < u64::from(self.left) {
            self.cur + self.step * (i + 1) as f32
        } else {
            self.target
        }
    }

    #[inline]
    pub fn is_const(&self) -> bool {
        self.left == 0
    }

    pub fn advance(&mut self, n: usize) {
        if (n as u64) < u64::from(self.left) {
            self.cur += self.step * n as f32;
            self.left -= n as u32;
        } else {
            self.cur = self.target;
            self.step = 0.0;
            self.left = 0;
        }
    }
}

/// A signal a term reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Src {
    Input(u32),
    Bus(u32),
}

/// `acc += gain(i) * src[i]`, or with an exclusion
/// `acc += gain(i) * (src[i] - depth(i) * minus_gain(i) * minus_src[i])`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Term {
    pub src: Src,
    pub slot: u32,
    pub minus: Option<MinusTerm>,
}

/// The participant's own contribution to the bus `src`, taken back out.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MinusTerm {
    pub src: Src,
    /// Slot of the participant → bus crosspoint (the exact gain the bus used).
    pub slot: u32,
    /// Slot fading the exclusion in and out (0 = not excluded, 1 = excluded).
    pub depth: u32,
}

/// One bus or output: the sum of `terms[start..end]`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Job {
    pub dst: u32,
    pub start: u32,
    pub end: u32,
}

/// A compiled routing: buses in dependency order, then outputs.
#[derive(Debug, Default)]
pub(crate) struct Plan {
    pub generation: u64,
    pub buses: Vec<Job>,
    pub outputs: Vec<Job>,
    pub terms: Vec<Term>,
    /// Every slot the plan reads (ramps advance once per block).
    pub slots: Vec<u32>,
    /// Gain changes applied the moment the plan is adopted, so a whole batch
    /// (a salvo, a scene) switches on the same sample.
    pub updates: Vec<Update>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Update {
    /// Set a slot's gain immediately (a slot no plan has been playing).
    Reset { slot: u32, gain: f32 },
    /// Ramp a slot to `target` over `frames`.
    Gain { slot: u32, target: f32, frames: u32 },
}

pub(crate) enum Msg {
    /// One gain change on its own (a fader move).
    Gain { slot: u32, target: f32, frames: u32 },
    /// A new routing and the gain changes that go with it.
    Plan(Box<Plan>),
}

/// Counters the audio thread publishes for the control thread.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    /// Messages applied so far.
    pub consumed: AtomicU64,
    /// Frames rendered so far.
    pub frames: AtomicU64,
    /// Generation of the plan now playing.
    pub generation: AtomicU64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_reaches_target_exactly_and_continues_smoothly() {
        let mut r = Ramp::default();
        r.reset(0.0);
        r.set(1.0, 100);
        let mut prev = 0.0;
        let mut max_step = 0.0f32;
        for _ in 0..3 {
            for i in 0..64 {
                let g = r.at(i);
                max_step = max_step.max((g - prev).abs());
                prev = g;
            }
            r.advance(64);
        }
        assert_eq!(r.at(0), 1.0);
        assert!(r.is_const());
        assert!(max_step <= 0.010_001, "{max_step}");
    }

    #[test]
    fn retarget_mid_ramp_starts_from_the_current_gain() {
        let mut r = Ramp::default();
        r.reset(0.0);
        r.set(1.0, 100);
        r.advance(50); // at 0.5
        let before = r.cur;
        r.set(0.0, 10);
        assert!((r.at(0) - (before - before / 10.0)).abs() < 1e-6);
    }
}
