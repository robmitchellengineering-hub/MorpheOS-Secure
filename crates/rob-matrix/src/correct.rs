//! Late-input corrections: the engine half of "commit early, correct late"
//! (PRIVATE: patent candidate, see `docs/ip/patent-candidates.md`; never
//! describe in public documents such as `08-matrix-engine.md`).
//!
//! The engine mixes on time. If an input's audio for some frames has not
//! arrived, the caller conceals it, mixes, and marks those frames with
//! [`MatrixAudio::mark_concealed`]. While mixing them the engine also runs the
//! plan once more with that input at a constant 1.0 and every other input
//! silent. The mix is linear and memoryless, so that pass yields the exact
//! per-sample gain `G_o(t)` from the input to every output `o`, through
//! crosspoints, buses, gain ramps and mix-minus alike. When the real audio
//! arrives, [`MatrixAudio::correct`] hands back, per output that hears the
//! input, `G_o(t) × (truth(t) − concealed(t))`: exactly what the output should
//! have had added. The receiver adds it where its hardware has not played yet.
//!
//! Memory is preallocated (`records × outputs × span` gains); nothing on the
//! audio thread allocates.

use crate::prelude::*;

use crate::audio::MatrixAudio;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Record {
    pub active: bool,
    pub input: u32,
    /// Engine frames `first .. first + len`.
    pub first: u64,
    pub len: u32,
    /// Frames of the record already mixed (gains known).
    pub probed: u32,
}

pub(crate) struct Corrections {
    pub records: Vec<Record>,
    /// `records × outputs × span` gains.
    gains: Vec<f32>,
    /// `records × outputs`: does this output hear the input at all?
    heard: Vec<bool>,
    /// A block of 1.0 for the probe pass.
    pub ones: Vec<f32>,
    /// One correction's worth of output, handed to the caller.
    out: Vec<f32>,
    outputs: usize,
    span: usize,
}

impl Corrections {
    pub fn new(records: usize, outputs: usize, span: usize, max_block: usize) -> Corrections {
        Corrections {
            records: vec![Record::default(); records],
            gains: vec![0.0; records * outputs * span],
            heard: vec![false; records * outputs],
            ones: vec![1.0; max_block],
            out: vec![0.0; span],
            outputs,
            span,
        }
    }

    /// Keeps probe gains for record `r`, output `o`, from record offset `at`.
    pub fn store(&mut self, r: usize, o: usize, at: usize, g: &[f32]) {
        if o >= self.outputs {
            return;
        }
        let base = (r * self.outputs + o) * self.span + at;
        self.gains[base..base + g.len()].copy_from_slice(g);
        if g.iter().any(|&v| v != 0.0) {
            self.heard[r * self.outputs + o] = true;
        }
    }
}

/// What [`MatrixAudio::correct`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Correction {
    /// Corrections handed out for this many outputs (0: nobody hears it).
    Sent { outputs: usize },
    /// No record covers those frames (never marked, or overwritten).
    Unknown,
    /// The frames have not all been mixed yet: nothing to correct.
    NotMixedYet,
}

impl MatrixAudio {
    /// Marks engine frames `first .. first + len` of `input` as concealed in
    /// the mix, before they are processed. Returns false if they are already
    /// mixed or longer than the configured span. Reuses the oldest record when
    /// all are taken. No allocation.
    pub fn mark_concealed(&mut self, input: u32, first: u64, len: u32) -> bool {
        let c = &mut self.corr;
        if first < self.stats.frames || len as usize > c.span || len == 0 {
            return false;
        }
        let r = c
            .records
            .iter()
            .position(|r| !r.active)
            .or_else(|| (0..c.records.len()).min_by_key(|&i| c.records[i].first))
            .unwrap_or(0);
        if c.records.is_empty() {
            return false;
        }
        c.records[r] = Record {
            active: true,
            input,
            first,
            len,
            probed: 0,
        };
        let o = c.outputs;
        c.heard[r * o..(r + 1) * o].fill(false);
        true
    }

    /// The real audio for concealed frames has arrived: `delta[k]` is
    /// `truth − concealed` for engine frame `first + k`. Calls
    /// `emit(output, first, correction)` for every output that hears the input.
    /// No allocation.
    pub fn correct(
        &mut self,
        input: u32,
        first: u64,
        delta: &[f32],
        mut emit: impl FnMut(u32, u64, &[f32]),
    ) -> Correction {
        let c = &mut self.corr;
        let end = first + delta.len() as u64;
        let Some(r) = c.records.iter().position(|r| {
            r.active && r.input == input && r.first <= first && end <= r.first + u64::from(r.len)
        }) else {
            return Correction::Unknown;
        };
        let rec = c.records[r];
        // The frames asked about must be mixed (not the whole record: a
        // record may run on through an EQ's ringing that is not mixed yet).
        if end > rec.first + u64::from(rec.probed) {
            return Correction::NotMixedYet;
        }
        let at = (first - rec.first) as usize;
        let mut outputs = 0;
        for o in 0..c.outputs {
            if !c.heard[r * c.outputs + o] {
                continue;
            }
            let base = (r * c.outputs + o) * c.span + at;
            let g = &c.gains[base..base + delta.len()];
            for ((y, &gk), &d) in c.out.iter_mut().zip(g).zip(delta) {
                *y = gk * d;
            }
            emit(o as u32, first, &c.out[..delta.len()]);
            outputs += 1;
        }
        // The record stays: the same frames can be corrected again (a repair
        // from a reduced copy, then an upgrade). Records are reused oldest
        // first when new spans are marked.
        Correction::Sent { outputs }
    }
}
