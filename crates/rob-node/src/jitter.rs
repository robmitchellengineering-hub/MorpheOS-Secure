//! The receiver's jitter buffer: holds every decoded copy of every period until
//! its turn, then plays the best copy it has (spec, "Receiver decision").
//!
//! Rendering is allocation-free and runs on the audio thread; chunks arrive from
//! the network thread through a lock-free queue (see `receiver`).
//!
//! **Layer changes need no crossfade.** All copies of a period are the same
//! waveform at different bit depths, so moving between them changes only the
//! noise floor, and 16-bit copies are dithered. (The spec's 32-sample crossfade
//! was written before this was thought through; it is needed only around
//! concealment, where the waveform itself is missing.)
//!
//! **Concealment v0.1.** When no copy arrived, the waveform is continued by point
//! reflection about the last sample (value and slope stay continuous) under a
//! 32-sample raised-cosine fade to silence; the next real audio fades back in over
//! 32 samples. Inaudible for single lost periods on most material, never a click.
//! Burg-predictive concealment (idea 27) is the planned upgrade.
//!
//! **Late repair (PRIVATE: patent candidate, see `docs/ip/`).** The buffer
//! remembers what it played, and which periods it concealed or played from a
//! 16- or 10-bit copy. It reports those spans through
//! [`JitterBuffer::drain_concealed`], so an engine can mark them before mixing.
//! When a copy arrives late that is better than what played, it builds a
//! **repair** (`truth − played`, interleaved, network time;
//! [`JitterBuffer::take_repair`]): for a concealed period, the period plus the
//! next [`FADE`] frames; for an upgrade, the period.
//!
//! Repairs compose exactly in any order: "played" is updated with every
//! repair issued, so a later repair of a neighbour computes exactly what is
//! still owed. A repair next to a still-concealed period never clicks: it
//! fades in at a concealed predecessor (that neighbour's repair pays the rest)
//! and, into a concealed successor, continues the true waveform under a fade to
//! silence, as the concealment itself would have. Late copies are kept, so a
//! neighbour's repair can use them as the truth for the shared edge.

use crate::prelude::*;

use crate::flow::{Chunk, FlowFormat, Layer, CHANNELS, MAX_PERIOD_FRAMES, MAX_PERIOD_SAMPLES};
use rob_core::layer::Dither;

/// Fade length for concealment, in frames.
pub const FADE: usize = 32;
const SCALE: f32 = 1.0 / 8_388_608.0;

/// What the buffer did, for telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlayoutStats {
    /// Periods rendered.
    pub periods: u64,
    /// Periods played from each source: 24-bit, 16-bit, 10-bit, concealed.
    pub by_layer: [u64; 4],
    /// Separate runs of concealed periods (the audible events).
    pub conceal_events: u64,
    /// Full-quality copies that arrived after their period had started playing:
    /// the latency budget is too small for the network.
    pub late: u64,
    /// Redundant copies that arrived after their period started. Normal: packet n
    /// repeats periods n-1 and n-2, which usually played from their own packets.
    pub late_redundant: u64,
    /// Copies off the flow's period grid.
    pub misaligned: u64,
    /// Copies too far ahead to hold.
    pub too_early: u64,
    /// Periods rendered as silence before the flow's first audio arrived (not
    /// counted as concealment).
    pub waiting: u64,
    /// Late copies of concealed periods turned into repairs.
    pub repairs: u64,
    /// Late better copies of periods played at 16 or 10 bits, turned into
    /// repairs (upgrades to what should have played).
    pub upgrades: u64,
    /// Repairs dropped because the queue was full.
    pub repairs_dropped: u64,
}

/// Concealed periods remembered for repair.
const CONCEAL_LOG: usize = 16;
/// Repairs and concealed-span events waiting to be collected.
const QUEUE: usize = 8;
/// Longest repair: a period plus the fade back in.
const REPAIR_SAMPLES: usize = (MAX_PERIOD_FRAMES + FADE) * CHANNELS;

#[derive(Clone, Copy, Default)]
struct Concealed {
    period: i64,
    valid: bool,
    /// `None`: concealed. `Some(layer)`: played from a reduced copy, which a
    /// late better copy can upgrade.
    played: Option<Layer>,
}

impl Concealed {
    fn concealed(&self, p: i64) -> bool {
        self.valid && self.period == p && self.played.is_none()
    }
    /// A late copy of `layer` would improve what was played for period `p`.
    fn improvable_by(&self, p: i64, layer: Layer) -> bool {
        self.valid && self.period == p && self.played.map_or(true, |l| (layer as u8) < (l as u8))
    }
}

struct Repair {
    first: i64,
    len: usize, // samples (interleaved)
    data: [f32; REPAIR_SAMPLES],
}

struct Slot {
    period: i64,
    have: [bool; 3],
    data: [[i32; MAX_PERIOD_SAMPLES]; 3],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Layer(Layer),
    Conceal,
}

pub struct JitterBuffer {
    fmt: FlowFormat,
    slots: Vec<Slot>,
    /// `first_sample mod period_frames` for this flow.
    phase: Option<i64>,
    /// Absolute network sample index of the next frame to render.
    next: Option<i64>,
    current: Option<(i64, Source)>,
    dither: Dither,
    /// The last `FADE` frames played, oldest first (for concealment).
    tail: [[f32; CHANNELS]; FADE],
    /// Frozen copy of `tail` while concealing.
    frozen: [[f32; CHANNELS]; FADE],
    conceal_pos: usize,
    fade_in_left: usize,
    /// Real audio has been played at least once.
    primed: bool,
    /// What was played, by absolute frame (ring).
    history: Vec<[f32; CHANNELS]>,
    concealed: [Concealed; CONCEAL_LOG],
    conceal_head: usize,
    /// Concealed spans not yet collected: (first frame, frames).
    spans: [(i64, u32, bool); QUEUE],
    n_spans: usize,
    repairs: Vec<Repair>,
    n_repairs: usize,
    /// Late copies of concealed periods still being played (or within the
    /// fade back in): repaired once the damage is fully known.
    deferred: [(Chunk, i64, bool); 2],
    pub stats: PlayoutStats,
}

impl JitterBuffer {
    /// `capacity_periods` must exceed the latency target in periods, plus the
    /// redundancy depth.
    pub fn new(fmt: FlowFormat, capacity_periods: usize) -> JitterBuffer {
        let slots = (0..capacity_periods)
            .map(|_| Slot {
                period: i64::MIN,
                have: [false; 3],
                data: [[0; MAX_PERIOD_SAMPLES]; 3],
            })
            .collect();
        JitterBuffer {
            fmt,
            slots,
            phase: None,
            next: None,
            current: None,
            dither: Dither::new(0x2545_F491),
            tail: [[0.0; CHANNELS]; FADE],
            frozen: [[0.0; CHANNELS]; FADE],
            conceal_pos: 0,
            fade_in_left: 0,
            primed: false,
            history: vec![[0.0; CHANNELS]; capacity_periods.max(1) * fmt.period_frames],
            concealed: [Concealed::default(); CONCEAL_LOG],
            conceal_head: 0,
            spans: [(0, 0, false); QUEUE],
            n_spans: 0,
            repairs: (0..QUEUE)
                .map(|_| Repair {
                    first: 0,
                    len: 0,
                    data: [0.0; REPAIR_SAMPLES],
                })
                .collect(),
            n_repairs: 0,
            deferred: [(Chunk::empty(), 0, false); 2],
            stats: PlayoutStats::default(),
        }
    }

    /// Adds a correction (`delta × weight`, one frame, network time) to frame
    /// `n` if it has not been rendered yet and its period's copy is held.
    /// Returns false otherwise (rendered already, or nothing held to correct:
    /// the packet has not arrived, or the period will be concealed). No
    /// allocation.
    pub fn add_correction(&mut self, n: i64, delta: &[f32; CHANNELS], weight: f32) -> bool {
        let (Some(next), Some(phase)) = (self.next, self.phase) else {
            return false;
        };
        if n < next {
            return false;
        }
        let f = self.fmt.period_frames as i64;
        let period = (n - phase).div_euclid(f);
        let k = (n - phase - period * f) as usize;
        let cap = self.slots.len() as i64;
        let slot = &mut self.slots[period.rem_euclid(cap) as usize];
        if slot.period != period || !slot.have.iter().any(|&h| h) {
            return false;
        }
        for l in 0..3 {
            if !slot.have[l] {
                continue;
            }
            for (s, &d) in slot.data[l][k * CHANNELS..(k + 1) * CHANNELS]
                .iter_mut()
                .zip(delta)
            {
                let v = f64::from(*s) + f64::from(d * weight) * 8_388_608.0;
                *s = v.round().clamp(-8_388_608.0, 8_388_607.0) as i32;
            }
        }
        true
    }

    /// Reports provisional spans since the last call, oldest first: (first
    /// frame, frames, concealed). Concealed spans cover the period plus the fade
    /// back in; spans played from a 16- or 10-bit copy cover the period. Call
    /// before mixing them.
    pub fn drain_concealed(&mut self, mut f: impl FnMut(i64, u32, bool)) {
        for &(first, len, concealed) in &self.spans[..self.n_spans] {
            f(first, len, concealed);
        }
        self.n_spans = 0;
    }

    /// Hands the oldest pending repair to `f` as (first frame, interleaved
    /// `truth − played`). Returns false when there is none. No allocation.
    pub fn take_repair(&mut self, f: impl FnOnce(i64, &[f32])) -> bool {
        if self.n_repairs == 0 {
            return false;
        }
        let r = &self.repairs[0];
        f(r.first, &r.data[..r.len]);
        self.repairs[..self.n_repairs].rotate_left(1);
        self.n_repairs -= 1;
        true
    }

    fn played(&self, n: i64) -> Option<[f32; CHANNELS]> {
        let next = self.next?;
        let h = self.history.len() as i64;
        (n < next && next - n <= h).then(|| self.history[n.rem_euclid(h) as usize])
    }

    /// Best copy held for `period`, as floats, frame `k`.
    fn held(&self, period: i64, k: usize) -> Option<[f32; CHANNELS]> {
        let cap = self.slots.len() as i64;
        let slot = &self.slots[period.rem_euclid(cap) as usize];
        if slot.period != period {
            return None;
        }
        let l = [Layer::Full24, Layer::Pcm16, Layer::Bc10]
            .into_iter()
            .find(|l| slot.have[*l as usize])?;
        let mut out = [0.0; CHANNELS];
        for (c, v) in out.iter_mut().enumerate() {
            *v = slot.data[l as usize][k * CHANNELS + c] as f32 * SCALE;
        }
        Some(out)
    }

    /// A late copy of a concealed (or reduced-quality) period: build its repair.
    fn repair(&mut self, c: &Chunk, period: i64, phase: i64) {
        let Some(ix) = self
            .concealed
            .iter()
            .position(|e| e.improvable_by(period, c.layer))
        else {
            return;
        };
        let entry = self.concealed[ix];
        if c.layer == Layer::Full24 {
            self.concealed[ix].valid = false; // nothing better can come
        } else {
            // Repaired from a reduced copy (often the first to arrive): the
            // period now holds real audio at that quality, and a later better
            // copy can still upgrade it.
            self.concealed[ix].played = Some(c.layer);
        }
        if self.n_repairs == QUEUE {
            self.stats.repairs_dropped += 1;
            return;
        }
        let f = self.fmt.period_frames;
        // Keep the late copy, so a later repair of a neighbour can use it as
        // the truth for their shared edge.
        {
            let cap = self.slots.len() as i64;
            let slot = &mut self.slots[period.rem_euclid(cap) as usize];
            if slot.period != period {
                slot.period = period;
                slot.have = [false; 3];
            }
            let n = self.fmt.period_samples();
            slot.data[c.layer as usize][..n].copy_from_slice(&c.samples[..n]);
            slot.have[c.layer as usize] = true;
        }
        let first = phase + period * f as i64;
        let upgrade = entry.played.is_some();
        // A concealed period's damage reaches FADE frames into the next one
        // (the fade back in, or more concealment): repair that too.
        let len = if upgrade { f } else { f + FADE };
        let neighbour = |p: i64| self.concealed.iter().any(|e| e.concealed(p));
        let (fade_head, next_concealed) =
            (!upgrade && neighbour(period - 1), neighbour(period + 1));
        let truth_at = |k: usize| -> [f32; CHANNELS] {
            let mut t = [0.0; CHANNELS];
            for (ch, v) in t.iter_mut().enumerate() {
                *v = c.samples[k * CHANNELS + ch] as f32 * SCALE;
            }
            t
        };
        let h = self.history.len() as i64;
        let idx = self.n_repairs;
        for k in 0..len {
            let n = first + k as i64;
            let Some(played) = self.played(n) else {
                return; // too old to know what was played: no repair
            };
            let target = if k < f {
                truth_at(k)
            } else if next_concealed {
                // The next period is still concealed: continue the true
                // waveform from this period's end (point reflection) under a
                // fade to silence, as the concealment itself would have.
                let j = k - f;
                let last = truth_at(f - 1);
                let mirror = truth_at(f - 2 - j.min(f - 2));
                let w =
                    0.5 * (1.0 + (core::f32::consts::PI * (j as f32 + 1.0) / FADE as f32).cos());
                [
                    (2.0 * last[0] - mirror[0]) * w,
                    (2.0 * last[1] - mirror[1]) * w,
                ]
            } else {
                self.held(period + 1, k - f).unwrap_or(played)
            };
            // Fade in at the head if the previous period is still concealed
            // (its repair, when it comes, pays the rest: see below).
            let w = if fade_head && k < FADE {
                0.5 - 0.5 * (core::f32::consts::PI * (k as f32 + 1.0) / (FADE + 1) as f32).cos()
            } else {
                1.0
            };
            let hist = n.rem_euclid(h) as usize;
            for ch in 0..CHANNELS {
                let d = w * (target[ch] - played[ch]);
                self.repairs[idx].data[k * CHANNELS + ch] = d;
                // "Played" now means played after this correction, so later
                // repairs of neighbours compute exactly what is still owed.
                self.history[hist][ch] += d;
            }
        }
        self.repairs[idx].first = first;
        self.repairs[idx].len = len * CHANNELS;
        self.n_repairs += 1;
        if upgrade {
            self.stats.upgrades += 1;
        } else {
            self.stats.repairs += 1;
        }
    }

    /// Where rendering is (absolute network sample index of the next frame).
    pub fn next_sample(&self) -> Option<i64> {
        self.next
    }

    /// Starts (or restarts) rendering at absolute network sample `first`.
    pub fn start_at(&mut self, first: i64) {
        self.next = Some(first);
        self.current = None;
    }

    /// Whether any copy of the flow has arrived yet.
    pub fn has_phase(&self) -> bool {
        self.phase.is_some()
    }

    /// Stores one decoded copy. No allocation.
    pub fn insert(&mut self, c: &Chunk) {
        let f = self.fmt.period_frames as i64;
        let phase = *self.phase.get_or_insert(c.first_sample.rem_euclid(f));
        if (c.first_sample - phase).rem_euclid(f) != 0 {
            self.stats.misaligned += 1;
            return;
        }
        if let Some(next) = self.next {
            if c.first_sample < next {
                let period = (c.first_sample - phase).div_euclid(f);
                // Still counted as late (the budget was too small), and, if
                // the period was concealed, turned into a repair.
                if self
                    .concealed
                    .iter()
                    .any(|e| e.improvable_by(period, c.layer))
                {
                    let done = phase + (period + 1) * f + FADE as i64;
                    if next >= done {
                        self.repair(c, period, phase);
                    } else if let Some(d) = self.deferred.iter_mut().find(|d| !d.2) {
                        *d = (*c, period, true);
                    } else {
                        self.stats.repairs_dropped += 1;
                    }
                }
                if c.layer == Layer::Full24 {
                    self.stats.late += 1;
                } else {
                    self.stats.late_redundant += 1;
                }
                return;
            }
            if c.first_sample >= next + (self.slots.len() as i64 - 1) * f {
                self.stats.too_early += 1;
                return;
            }
        }
        let period = (c.first_sample - phase).div_euclid(f);
        let cap = self.slots.len() as i64;
        let n = self.fmt.period_samples();
        let slot = &mut self.slots[period.rem_euclid(cap) as usize];
        if slot.period != period {
            slot.period = period;
            slot.have = [false; 3];
        }
        let l = c.layer as usize;
        slot.data[l][..n].copy_from_slice(&c.samples[..n]);
        slot.have[l] = true;
    }

    /// Whether every period overlapping network frames `from .. to` is held
    /// (in any layer): a receiver with a backup source looks ahead with it.
    pub fn holds_span(&self, from: i64, to: i64) -> bool {
        let Some(phase) = self.phase else {
            return false;
        };
        if to <= from {
            return true;
        }
        let f = self.fmt.period_frames as i64;
        let cap = self.slots.len() as i64;
        let (mut period, last) = ((from - phase).div_euclid(f), (to - 1 - phase).div_euclid(f));
        while period <= last {
            let slot = &self.slots[period.rem_euclid(cap) as usize];
            if slot.period != period || !slot.have.iter().any(|&h| h) {
                return false;
            }
            period += 1;
        }
        true
    }

    fn choose(&mut self, period: i64) -> Source {
        let cap = self.slots.len() as i64;
        let slot = &self.slots[period.rem_euclid(cap) as usize];
        if slot.period == period {
            for l in [Layer::Full24, Layer::Pcm16, Layer::Bc10] {
                if slot.have[l as usize] {
                    return Source::Layer(l);
                }
            }
        }
        Source::Conceal
    }

    /// Renders frames up to (not including) absolute index `until`, handing
    /// each frame to `push`. No allocation. Before any audio has arrived, renders
    /// silence.
    pub fn render_until(&mut self, until: i64, mut push: impl FnMut(&[f32; CHANNELS])) {
        let Some(mut next) = self.next else { return };
        let f = self.fmt.period_frames as i64;
        while next < until {
            let Some(phase) = self.phase else {
                push(&[0.0; CHANNELS]);
                next += 1;
                continue;
            };
            let period = (next - phase).div_euclid(f);
            let offset = ((next - phase) - period * f) as usize;
            let source = match self.current {
                Some((p, s)) if p == period => s,
                _ => {
                    let s = self.choose(period);
                    self.begin_period(period, s);
                    s
                }
            };
            let mut frame = [0.0f32; CHANNELS];
            match source {
                Source::Layer(l) => {
                    let cap = self.slots.len() as i64;
                    let slot = &self.slots[period.rem_euclid(cap) as usize];
                    for (c, v) in frame.iter_mut().enumerate() {
                        let s = slot.data[l as usize][offset * CHANNELS + c];
                        let s = if l == Layer::Pcm16 {
                            self.dither.apply(s)
                        } else {
                            s
                        };
                        *v = s as f32 * SCALE;
                    }
                    if self.fade_in_left > 0 {
                        let k = FADE - self.fade_in_left;
                        let w = 0.5
                            * (1.0
                                - (core::f32::consts::PI * (k as f32 + 1.0) / FADE as f32).cos());
                        frame.iter_mut().for_each(|v| *v *= w);
                        self.fade_in_left -= 1;
                    }
                    self.tail.rotate_left(1);
                    self.tail[FADE - 1] = frame;
                }
                Source::Conceal => {
                    if self.conceal_pos < FADE {
                        let k = self.conceal_pos;
                        let w = 0.5
                            * (1.0
                                + (core::f32::consts::PI * (k as f32 + 1.0) / FADE as f32).cos());
                        for (c, v) in frame.iter_mut().enumerate() {
                            // Point reflection about the last real sample.
                            let last = self.frozen[FADE - 1][c];
                            let mirror = self.frozen[FADE - 2 - k.min(FADE - 2)][c];
                            *v = ((2.0 * last - mirror) * w).clamp(-1.0, 1.0);
                        }
                        self.conceal_pos += 1;
                    }
                }
            }
            let h = self.history.len() as i64;
            self.history[next.rem_euclid(h) as usize] = frame;
            push(&frame);
            next += 1;
        }
        self.next = Some(next);
        // Deferred repairs whose period and fade back in have now played.
        if let Some(phase) = self.phase {
            for i in 0..self.deferred.len() {
                let (c, period, live) = self.deferred[i];
                if live && next >= phase + (period + 1) * f + FADE as i64 {
                    self.deferred[i].2 = false;
                    self.repair(&c, period, phase);
                }
            }
        }
    }

    fn begin_period(&mut self, period: i64, s: Source) {
        let was_concealing = matches!(self.current, Some((_, Source::Conceal)));
        self.current = Some((period, s));
        if !self.primed {
            if s == Source::Conceal {
                // Nothing has played yet: this is start-up silence, not a loss.
                self.stats.waiting += 1;
                self.conceal_pos = FADE; // silent, nothing to fade
                return;
            }
            self.primed = true;
            self.fade_in_left = FADE;
        }
        self.stats.periods += 1;
        match s {
            Source::Layer(l) => {
                self.stats.by_layer[l as usize] += 1;
                if was_concealing {
                    self.fade_in_left = FADE;
                }
                if l != Layer::Full24 {
                    // Played from a reduced copy: a late full copy can upgrade it.
                    self.concealed[self.conceal_head] = Concealed {
                        period,
                        valid: true,
                        played: Some(l),
                    };
                    self.conceal_head = (self.conceal_head + 1) % CONCEAL_LOG;
                    if self.n_spans < QUEUE {
                        let f = self.fmt.period_frames as i64;
                        let phase = self.phase.unwrap_or(0);
                        self.spans[self.n_spans] = (phase + period * f, f as u32, false);
                        self.n_spans += 1;
                    }
                }
            }
            Source::Conceal => {
                self.stats.by_layer[3] += 1;
                if !was_concealing {
                    self.stats.conceal_events += 1;
                    self.frozen = self.tail;
                    self.conceal_pos = 0;
                }
                self.concealed[self.conceal_head] = Concealed {
                    period,
                    valid: true,
                    played: None,
                };
                self.conceal_head = (self.conceal_head + 1) % CONCEAL_LOG;
                if self.n_spans < QUEUE {
                    let f = self.fmt.period_frames as i64;
                    let phase = self.phase.unwrap_or(0);
                    self.spans[self.n_spans] =
                        (phase + period * f, (f as usize + FADE) as u32, true);
                    self.n_spans += 1;
                }
            }
        }
    }
}
