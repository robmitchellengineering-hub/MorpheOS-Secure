//! The audio-thread half: applies queued changes at block boundaries and mixes.
//!
//! No allocation, no locks, no freeing: old plans go back to the control thread
//! through a queue sized so it can never be full.

use crate::prelude::*;

use crate::plan::{Job, Msg, Plan, Ramp, Shared, Src, Term, Update};
use alloc::sync::Arc;
use core::sync::atomic::Ordering;

/// What the audio thread did, for telemetry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioStats {
    pub blocks: u64,
    pub frames: u64,
    pub plans_adopted: u64,
    pub messages: u64,
}

pub struct MatrixAudio {
    pub(crate) rx: rtrb::Consumer<Msg>,
    pub(crate) ret: rtrb::Producer<Box<Plan>>,
    pub(crate) shared: Arc<Shared>,
    pub(crate) plan: Box<Plan>,
    pub(crate) ramps: Vec<Ramp>,
    /// `max_buses × max_block` samples.
    pub(crate) bus_buf: Vec<f32>,
    /// One block of scratch for the bus being summed.
    pub(crate) acc: Vec<f32>,
    pub(crate) max_block: usize,
    pub(crate) n_outputs: usize,
    /// Late-input corrections (PRIVATE, see `correct.rs`).
    pub(crate) corr: crate::correct::Corrections,
    pub stats: AudioStats,
}

impl MatrixAudio {
    /// Mixes one block: `inputs[i]` is input channel `i`, `outputs[o]` is output
    /// channel `o`, both mono, at least `frames` long. Missing inputs are silent;
    /// outputs with no routes are written with silence. Any block size works
    /// (longer blocks are processed in `max_block` pieces). No allocation.
    pub fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]], frames: usize) {
        self.apply_messages();
        let base = self.stats.frames;
        let mut off = 0;
        while off < frames {
            let len = (frames - off).min(self.max_block);
            self.chunk(inputs, outputs, off, len);
            // Before the ramps move on: record the exact gains concealed
            // inputs had to every output in this piece.
            self.probe(base + off as u64, len);
            for &s in &self.plan.slots {
                self.ramps[s as usize].advance(len);
            }
            off += len;
        }
        self.stats.blocks += 1;
        self.stats.frames += frames as u64;
        self.shared
            .frames
            .store(self.stats.frames, Ordering::Release);
    }

    fn apply_messages(&mut self) {
        let mut applied = 0u64;
        loop {
            let is_plan = match self.rx.peek() {
                Ok(Msg::Plan(_)) => true,
                Ok(_) => false,
                Err(_) => break,
            };
            // Keep order: a plan waits (and everything after it) until the old
            // one can be handed back. Cannot happen with the queue sizes used.
            if is_plan && self.ret.is_full() {
                break;
            }
            let Ok(msg) = self.rx.pop() else { break };
            applied += 1;
            match msg {
                Msg::Gain {
                    slot,
                    target,
                    frames,
                } => {
                    if let Some(r) = self.ramps.get_mut(slot as usize) {
                        r.set(target, frames);
                    }
                }
                Msg::Plan(p) => {
                    for u in &p.updates {
                        match *u {
                            Update::Reset { slot, gain } => {
                                if let Some(r) = self.ramps.get_mut(slot as usize) {
                                    r.reset(gain);
                                }
                            }
                            Update::Gain {
                                slot,
                                target,
                                frames,
                            } => {
                                if let Some(r) = self.ramps.get_mut(slot as usize) {
                                    r.set(target, frames);
                                }
                            }
                        }
                    }
                    let old = core::mem::replace(&mut self.plan, p);
                    self.stats.plans_adopted += 1;
                    self.shared
                        .generation
                        .store(self.plan.generation, Ordering::Release);
                    // Checked above: there is room.
                    let _ = self.ret.push(old);
                }
            }
        }
        if applied > 0 {
            self.stats.messages += applied;
            self.shared
                .consumed
                .store(self.stats.messages, Ordering::Release);
        }
    }

    fn chunk(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]], off: usize, len: usize) {
        let MatrixAudio {
            plan,
            ramps,
            bus_buf,
            acc,
            max_block,
            n_outputs,
            ..
        } = self;
        let mb = *max_block;
        for o in outputs.iter_mut().take(*n_outputs) {
            if let Some(s) = o.get_mut(off..off + len) {
                s.fill(0.0);
            }
        }
        let inp = In::Real(inputs);
        for job in &plan.buses {
            let acc = &mut acc[..len];
            acc.fill(0.0);
            sum_terms(acc, job, &plan.terms, ramps, inp, bus_buf, mb, off);
            let d = job.dst as usize * mb;
            bus_buf[d..d + len].copy_from_slice(acc);
        }
        for job in &plan.outputs {
            let Some(out) = outputs
                .get_mut(job.dst as usize)
                .and_then(|o| o.get_mut(off..off + len))
            else {
                continue;
            };
            sum_terms(out, job, &plan.terms, ramps, inp, bus_buf, mb, off);
        }
    }

    /// For every concealed input overlapping engine frames `abs..abs + len`:
    /// run the plan with that input at a constant 1.0 and nothing else, which
    /// (the mix being linear and memoryless) yields the exact per-sample gain
    /// from that input to each output, and keep it for a later correction.
    fn probe(&mut self, abs: u64, len: usize) {
        let MatrixAudio {
            plan,
            ramps,
            bus_buf,
            acc,
            max_block,
            corr,
            ..
        } = self;
        let mb = *max_block;
        // Taken out for the pass (no allocation) so gains can be stored.
        let ones = core::mem::take(&mut corr.ones);
        for r in 0..corr.records.len() {
            let rec = corr.records[r];
            if !rec.active {
                continue;
            }
            let lo = rec.first.max(abs);
            let hi = (rec.first + u64::from(rec.len)).min(abs + len as u64);
            if lo >= hi {
                continue;
            }
            let inp = In::Probe {
                input: rec.input,
                ones: &ones[..len],
            };
            for job in &plan.buses {
                let a = &mut acc[..len];
                a.fill(0.0);
                sum_terms(a, job, &plan.terms, ramps, inp, bus_buf, mb, 0);
                let d = job.dst as usize * mb;
                bus_buf[d..d + len].copy_from_slice(a);
            }
            for job in &plan.outputs {
                let a = &mut acc[..len];
                a.fill(0.0);
                sum_terms(a, job, &plan.terms, ramps, inp, bus_buf, mb, 0);
                let (from, to) = ((lo - abs) as usize, (hi - abs) as usize);
                corr.store(r, job.dst as usize, (lo - rec.first) as usize, &a[from..to]);
            }
            corr.records[r].probed += (hi - lo) as u32;
        }
        corr.ones = ones;
    }
}

/// Where a term's input samples come from: the real inputs, or (for a
/// correction probe) a constant 1.0 on one input and silence elsewhere.
#[derive(Clone, Copy)]
enum In<'a> {
    Real(&'a [&'a [f32]]),
    Probe { input: u32, ones: &'a [f32] },
}

/// The signal `src` for this piece of the block, if it exists.
#[inline]
fn signal<'a>(
    src: Src,
    inputs: In<'a>,
    bus_buf: &'a [f32],
    mb: usize,
    off: usize,
    len: usize,
) -> Option<&'a [f32]> {
    match src {
        Src::Input(i) => match inputs {
            In::Real(v) => v.get(i as usize)?.get(off..off + len),
            In::Probe { input, ones } => (i == input).then(|| &ones[..len]),
        },
        Src::Bus(b) => bus_buf.get(b as usize * mb..b as usize * mb + len),
    }
}

#[allow(clippy::too_many_arguments)]
fn sum_terms(
    acc: &mut [f32],
    job: &Job,
    terms: &[Term],
    ramps: &[Ramp],
    inputs: In<'_>,
    bus_buf: &[f32],
    mb: usize,
    off: usize,
) {
    let len = acc.len();
    for t in &terms[job.start as usize..job.end as usize] {
        let g = &ramps[t.slot as usize];
        if g.is_const() && g.target == 0.0 {
            continue; // only routes that sound cost anything
        }
        let Some(x) = signal(t.src, inputs, bus_buf, mb, off, len) else {
            continue;
        };
        let minus = t.minus.and_then(|m| {
            let p = signal(m.src, inputs, bus_buf, mb, off, len)?;
            let (gp, depth) = (&ramps[m.slot as usize], &ramps[m.depth as usize]);
            if depth.is_const() && depth.target == 0.0 {
                return None;
            }
            Some((p, gp, depth))
        });
        match minus {
            None if g.is_const() => {
                let g = g.target;
                for (a, &v) in acc.iter_mut().zip(x) {
                    *a += g * v;
                }
            }
            None => {
                for (i, (a, &v)) in acc.iter_mut().zip(x).enumerate() {
                    *a += g.at(i) * v;
                }
            }
            Some((p, gp, depth)) => {
                // The bus summed `gp.at(i) * p[i]`; the same expression,
                // evaluated the same way, takes it back out.
                for i in 0..len {
                    let own = gp.at(i) * p[i];
                    acc[i] += g.at(i) * (x[i] - depth.at(i) * own);
                }
            }
        }
    }
}
