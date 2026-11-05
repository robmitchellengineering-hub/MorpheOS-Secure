//! The control half: the routing model, validation, and compiling it into plans
//! for the audio thread. Runs on a normal thread (UI, network control); may
//! allocate. Every change is all-or-nothing: either it is valid and queued, or
//! nothing changes and the error says why.
//!
//! **Fades.** A removed route or exclusion is not dropped at once: its gain
//! ramps to zero while it stays in the playing plan, and only when the audio
//! thread has certainly finished the ramp does a new plan leave it out
//! ([`MatrixControl::tidy`]). While a route is fading it still counts for loop
//! and mix-minus checks; a change that conflicts only with a fading route gets
//! [`MatrixError::Fading`] and can be retried a few milliseconds later.

use crate::prelude::*;

use crate::audio::{AudioStats, MatrixAudio};
use crate::plan::{Job, MinusTerm, Msg, Plan, Ramp, Shared, Src, Term, Update};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::Arc;
use core::fmt;
use core::sync::atomic::Ordering;

/// Highest crosspoint gain accepted.
pub const MAX_GAIN_DB: f32 = 24.0;

/// A point in the matrix. Inputs and buses are sources; buses and outputs are
/// destinations. Every channel is mono; stereo is a pair at the UI level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Node {
    Input(u32),
    Bus(u32),
    Output(u32),
}

impl Node {
    fn is_source(self) -> bool {
        matches!(self, Node::Input(_) | Node::Bus(_))
    }
    fn is_dest(self) -> bool {
        matches!(self, Node::Bus(_) | Node::Output(_))
    }
    fn src(self) -> Option<Src> {
        match self {
            Node::Input(i) => Some(Src::Input(i)),
            Node::Bus(b) => Some(Src::Bus(b)),
            Node::Output(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MatrixConfig {
    pub inputs: u32,
    pub outputs: u32,
    pub max_buses: u32,
    /// Gain slots: one per crosspoint plus one per mix-minus exclusion.
    pub max_crosspoints: u32,
    /// Largest piece processed at once (longer blocks are split).
    pub max_block: usize,
    /// Length of every gain change, route change and exclusion change.
    pub ramp_frames: u32,
    /// Changes that can wait for the audio thread at once.
    pub queue: usize,
    /// Concealed input spans remembered for late corrections, and the longest
    /// span in frames (a period plus the concealment fade-in, with room).
    pub correction_records: usize,
    pub correction_span: usize,
}

impl MatrixConfig {
    pub fn new(inputs: u32, outputs: u32) -> MatrixConfig {
        MatrixConfig {
            inputs,
            outputs,
            max_buses: 64,
            max_crosspoints: 16_384,
            max_block: 256,
            ramp_frames: 480, // 10 ms at 48 kHz
            queue: 4_096,
            correction_records: 32,
            correction_span: 512,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MatrixError {
    NoSuchNode(Node),
    NotASource(Node),
    NotADestination(Node),
    SelfRoute(Node),
    NotConnected(Node, Node),
    BadGain(f32),
    /// The route would close a feedback loop; the loop, by name.
    WouldLoop(Vec<String>),
    /// A mix-minus could not take the participant out: it reaches the bus by
    /// another path too (named), so the listener would hear themselves.
    MixMinusLeak {
        participant: String,
        bus: String,
        via: Vec<String>,
    },
    /// Mix-minus is taken from a bus (an input has nothing to remove).
    MixMinusNeedsBus(Node),
    /// The crosspoint already excludes someone else.
    MixMinusAlreadySet(Node),
    /// Conflicts with a route or exclusion that is still fading out; retry in
    /// about one ramp time.
    Fading,
    Full(&'static str),
    /// The audio thread has not taken earlier changes (is it running?).
    Busy,
    /// Change number `index` of a batch was refused; nothing was applied.
    InChange {
        index: usize,
        error: Box<MatrixError>,
    },
}

impl fmt::Display for MatrixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MatrixError::NoSuchNode(n) => write!(f, "{n:?} does not exist"),
            MatrixError::NotASource(n) => write!(f, "{n:?} cannot feed anything"),
            MatrixError::NotADestination(n) => write!(f, "{n:?} cannot be fed"),
            MatrixError::SelfRoute(n) => write!(f, "{n:?} cannot feed itself"),
            MatrixError::NotConnected(a, b) => write!(f, "{a:?} is not routed to {b:?}"),
            MatrixError::BadGain(g) => {
                write!(f, "gain {g} dB is not allowed (max +{MAX_GAIN_DB} dB)")
            }
            MatrixError::WouldLoop(path) => {
                write!(f, "that route would make a feedback loop: {}", path.join(" → "))
            }
            MatrixError::MixMinusLeak {
                participant,
                bus,
                via,
            } => write!(
                f,
                "{participant} also reaches {bus} through {}, so a mix-minus could not take them out",
                via.join(" → ")
            ),
            MatrixError::MixMinusNeedsBus(n) => {
                write!(f, "mix-minus is taken from a bus, and {n:?} is not one")
            }
            MatrixError::MixMinusAlreadySet(n) => {
                write!(f, "that feed already excludes {n:?}; clear it first")
            }
            MatrixError::Fading => {
                write!(f, "a route being removed is still fading out; try again in a moment")
            }
            MatrixError::Full(what) => write!(f, "no room for more {what}"),
            MatrixError::Busy => write!(f, "the audio engine has not taken earlier changes"),
            MatrixError::InChange { index, error } => {
                write!(f, "change {} of the batch: {error}", index + 1)
            }
        }
    }
}

impl core::error::Error for MatrixError {}

/// One routing change, for [`MatrixControl::apply`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change {
    Connect {
        src: Node,
        dst: Node,
        gain_db: f32,
    },
    Disconnect {
        src: Node,
        dst: Node,
    },
    Exclude {
        bus: Node,
        dst: Node,
        participant: Node,
    },
    Include {
        bus: Node,
        dst: Node,
    },
}

/// A change being built: the new routing and the gain moves that go with it.
struct Stage {
    edges: Edges,
    /// Slots taken from the end of the free list.
    taken: usize,
    updates: Vec<Update>,
    /// The routing itself changes (a new plan is needed).
    structural: bool,
    /// Routes created in this change (still silent).
    fresh: BTreeSet<(Node, Node)>,
}

/// A crosspoint as the UI sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrosspointInfo {
    pub gain_db: f32,
    /// Mix-minus: this participant is taken out of the feed.
    pub excludes: Option<Node>,
    /// Being removed (ramping to silence).
    pub fading: bool,
}

/// A pending removal: its zero-gain ramp is message `seq`; once the audio thread
/// has applied it, `until` is the frame by which the ramp has certainly ended.
#[derive(Debug, Clone, Copy)]
struct Fade {
    seq: u64,
    until: Option<u64>,
}

#[derive(Debug, Clone)]
struct Minus {
    node: Node,
    depth: u32,
    fade: Option<Fade>,
}

#[derive(Debug, Clone)]
struct Edge {
    slot: u32,
    gain_db: f32,
    minus: Option<Minus>,
    fade: Option<Fade>,
}

#[derive(Debug, Clone)]
struct BusInfo {
    name: String,
    retiring: bool,
}

type Edges = BTreeMap<(Node, Node), Edge>;

pub struct MatrixControl {
    cfg: MatrixConfig,
    in_names: Vec<String>,
    out_names: Vec<String>,
    buses: Vec<Option<BusInfo>>,
    edges: Edges,
    free: Vec<u32>,
    generation: u64,
    sent: u64,
    tx: rtrb::Producer<Msg>,
    ret: rtrb::Consumer<Box<Plan>>,
    shared: Arc<Shared>,
}

/// Creates a matrix: the control half for the UI/network thread and the audio
/// half for the audio thread. All audio-side memory is allocated here.
pub fn matrix(cfg: MatrixConfig) -> (MatrixControl, MatrixAudio) {
    let (tx, rx) = rtrb::RingBuffer::new(cfg.queue);
    // Every plan in the message queue can come back before the control thread
    // next looks, so this queue can never be full.
    let (ret_tx, ret) = rtrb::RingBuffer::new(cfg.queue + 1);
    let shared = Arc::new(Shared::default());
    let audio = MatrixAudio {
        rx,
        ret: ret_tx,
        shared: Arc::clone(&shared),
        plan: Box::default(),
        ramps: vec![Ramp::default(); cfg.max_crosspoints as usize],
        bus_buf: vec![0.0; cfg.max_buses as usize * cfg.max_block],
        acc: vec![0.0; cfg.max_block],
        max_block: cfg.max_block,
        n_outputs: cfg.outputs as usize,
        corr: crate::correct::Corrections::new(
            cfg.correction_records,
            cfg.outputs as usize,
            cfg.correction_span,
            cfg.max_block,
        ),
        stats: AudioStats::default(),
    };
    let control = MatrixControl {
        cfg,
        in_names: (1..=cfg.inputs).map(|i| format!("In {i}")).collect(),
        out_names: (1..=cfg.outputs).map(|i| format!("Out {i}")).collect(),
        buses: vec![None; cfg.max_buses as usize],
        edges: BTreeMap::new(),
        free: (0..cfg.max_crosspoints).rev().collect(),
        generation: 0,
        sent: 0,
        tx,
        ret,
        shared,
    };
    (control, audio)
}

fn lin(db: f32) -> f32 {
    if db == f32::NEG_INFINITY {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

impl MatrixControl {
    // ---- names and queries -------------------------------------------------

    pub fn name(&self, n: Node) -> String {
        match n {
            Node::Input(i) => self.in_names.get(i as usize).cloned(),
            Node::Output(o) => self.out_names.get(o as usize).cloned(),
            Node::Bus(b) => self
                .buses
                .get(b as usize)
                .and_then(|x| x.as_ref())
                .map(|x| x.name.clone()),
        }
        .unwrap_or_else(|| format!("{n:?}"))
    }

    pub fn set_name(&mut self, n: Node, name: &str) -> Result<(), MatrixError> {
        let slot = match n {
            Node::Input(i) => self.in_names.get_mut(i as usize),
            Node::Output(o) => self.out_names.get_mut(o as usize),
            Node::Bus(b) => self
                .buses
                .get_mut(b as usize)
                .and_then(|x| x.as_mut())
                .map(|x| &mut x.name),
        };
        *slot.ok_or(MatrixError::NoSuchNode(n))? = name.to_string();
        Ok(())
    }

    pub fn crosspoint(&self, src: Node, dst: Node) -> Option<CrosspointInfo> {
        self.edges.get(&(src, dst)).map(|e| CrosspointInfo {
            gain_db: e.gain_db,
            excludes: e
                .minus
                .as_ref()
                .filter(|m| m.fade.is_none())
                .map(|m| m.node),
            fading: e.fade.is_some(),
        })
    }

    /// Routes and exclusions still fading out.
    pub fn fading(&self) -> usize {
        self.edges
            .values()
            .map(|e| {
                usize::from(e.fade.is_some())
                    + usize::from(e.minus.as_ref().is_some_and(|m| m.fade.is_some()))
            })
            .sum()
    }

    /// Generation of the plan the audio thread is playing (tests, telemetry).
    pub fn audio_generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    /// Generation of the latest plan sent.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    // ---- buses -------------------------------------------------------------

    pub fn add_bus(&mut self, name: &str) -> Result<Node, MatrixError> {
        self.tidy();
        let i = self
            .buses
            .iter()
            .position(Option::is_none)
            .ok_or(MatrixError::Full("buses"))?;
        self.buses[i] = Some(BusInfo {
            name: name.to_string(),
            retiring: false,
        });
        // No plan yet: the bus joins the next one, which any route to or from
        // it will send.
        Ok(Node::Bus(i as u32))
    }

    /// Fades out every route to and from the bus (and any exclusion of it),
    /// then frees it.
    pub fn remove_bus(&mut self, bus: Node) -> Result<(), MatrixError> {
        self.tidy();
        self.check_node(bus)?;
        let Node::Bus(b) = bus else {
            return Err(MatrixError::NoSuchNode(bus));
        };
        let mut st = self.stage();
        let fade = self.next_fade();
        for ((s, d), e) in st.edges.iter_mut() {
            if (*s == bus || *d == bus) && e.fade.is_none() {
                st.updates.push(Update::Gain {
                    slot: e.slot,
                    target: 0.0,
                    frames: self.cfg.ramp_frames,
                });
                e.fade = Some(fade);
            }
            if let Some(m) = e
                .minus
                .as_mut()
                .filter(|m| m.node == bus && m.fade.is_none())
            {
                st.updates.push(Update::Gain {
                    slot: m.depth,
                    target: 0.0,
                    frames: self.cfg.ramp_frames,
                });
                m.fade = Some(fade);
            }
        }
        self.commit(st)?;
        if let Some(info) = self.buses[b as usize].as_mut() {
            info.retiring = true;
        }
        Ok(())
    }

    // ---- crosspoints -------------------------------------------------------

    /// Applies several changes as one: validated as a whole (only the final
    /// state has to be valid) and switched on the same sample. Scenes, salvos
    /// and bulk set-ups use this. All or nothing.
    pub fn apply(&mut self, changes: &[Change]) -> Result<(), MatrixError> {
        self.tidy();
        let mut st = self.stage();
        for (index, c) in changes.iter().enumerate() {
            self.stage_change(&mut st, *c)
                .map_err(|error| MatrixError::InChange {
                    index,
                    error: Box::new(error),
                })?;
        }
        self.commit(st)
    }

    /// One change on its own (errors unwrapped).
    fn one(&mut self, c: Change) -> Result<(), MatrixError> {
        self.apply(&[c]).map_err(|e| match e {
            MatrixError::InChange { error, .. } => *error,
            e => e,
        })
    }

    /// Routes `src` to `dst` at `gain_db` (`f32::NEG_INFINITY` = silent but
    /// routed), fading in. On an existing route, changes its gain.
    pub fn connect(&mut self, src: Node, dst: Node, gain_db: f32) -> Result<(), MatrixError> {
        self.one(Change::Connect { src, dst, gain_db })
    }

    /// Moves the gain of an existing route (a fader).
    pub fn set_gain_db(&mut self, src: Node, dst: Node, gain_db: f32) -> Result<(), MatrixError> {
        match self.edges.get(&(src, dst)) {
            Some(e) if e.fade.is_none() => self.connect(src, dst, gain_db),
            _ => Err(MatrixError::NotConnected(src, dst)),
        }
    }

    /// Fades the route out; it leaves the plan once the fade has played.
    pub fn disconnect(&mut self, src: Node, dst: Node) -> Result<(), MatrixError> {
        self.one(Change::Disconnect { src, dst })
    }

    /// Mix-minus: the feed from `bus` to `dst` leaves out `participant`'s own
    /// contribution to the bus. Fades in (immediate on a route that is new).
    pub fn exclude(&mut self, bus: Node, dst: Node, participant: Node) -> Result<(), MatrixError> {
        self.one(Change::Exclude {
            bus,
            dst,
            participant,
        })
    }

    /// Ends a mix-minus: the participant fades back into the feed.
    pub fn include(&mut self, bus: Node, dst: Node) -> Result<(), MatrixError> {
        self.one(Change::Include { bus, dst })
    }

    /// One call for the common case: `listener` hears `bus` without
    /// `participant` (an IFB, a caller, a remote contributor). Route and
    /// exclusion start together, so the listener never hears themselves.
    pub fn mix_minus(
        &mut self,
        bus: Node,
        participant: Node,
        listener: Node,
        gain_db: f32,
    ) -> Result<(), MatrixError> {
        self.apply(&[
            Change::Connect {
                src: bus,
                dst: listener,
                gain_db,
            },
            Change::Exclude {
                bus,
                dst: listener,
                participant,
            },
        ])
        .map_err(|e| match e {
            MatrixError::InChange { error, .. } => *error,
            e => e,
        })
    }

    fn stage(&self) -> Stage {
        Stage {
            edges: self.edges.clone(),
            taken: 0,
            updates: Vec::new(),
            structural: false,
            fresh: BTreeSet::new(),
        }
    }

    /// Every change is one message, so its fades all complete with it.
    fn next_fade(&self) -> Fade {
        Fade {
            seq: self.sent + 1,
            until: None,
        }
    }

    fn take_slot(&self, st: &mut Stage) -> Result<u32, MatrixError> {
        let n = self.free.len();
        if st.taken >= n {
            return Err(MatrixError::Full("crosspoints"));
        }
        st.taken += 1;
        Ok(self.free[n - st.taken])
    }

    fn stage_change(&self, st: &mut Stage, c: Change) -> Result<(), MatrixError> {
        let ramp = self.cfg.ramp_frames;
        match c {
            Change::Connect { src, dst, gain_db } => {
                self.check_node(src)?;
                self.check_node(dst)?;
                if !src.is_source() {
                    return Err(MatrixError::NotASource(src));
                }
                if !dst.is_dest() {
                    return Err(MatrixError::NotADestination(dst));
                }
                if src == dst {
                    return Err(MatrixError::SelfRoute(src));
                }
                let g = Self::check_gain(gain_db)?;
                if let Some(e) = st.edges.get_mut(&(src, dst)) {
                    // Live: a gain change. Fading: revive from wherever it is.
                    e.fade = None;
                    e.gain_db = gain_db;
                    st.updates.push(Update::Gain {
                        slot: e.slot,
                        target: g,
                        frames: ramp,
                    });
                } else {
                    let slot = self.take_slot(st)?;
                    st.edges.insert(
                        (src, dst),
                        Edge {
                            slot,
                            gain_db,
                            minus: None,
                            fade: None,
                        },
                    );
                    st.fresh.insert((src, dst));
                    st.updates.push(Update::Reset { slot, gain: 0.0 });
                    st.updates.push(Update::Gain {
                        slot,
                        target: g,
                        frames: ramp,
                    });
                    st.structural = true;
                }
            }
            Change::Disconnect { src, dst } => {
                let fade = self.next_fade();
                match st.edges.get_mut(&(src, dst)) {
                    Some(e) if e.fade.is_none() => {
                        e.fade = Some(fade);
                        st.updates.push(Update::Gain {
                            slot: e.slot,
                            target: 0.0,
                            frames: ramp,
                        });
                    }
                    _ => return Err(MatrixError::NotConnected(src, dst)),
                }
            }
            Change::Exclude {
                bus,
                dst,
                participant,
            } => {
                if !matches!(bus, Node::Bus(_)) {
                    return Err(MatrixError::MixMinusNeedsBus(bus));
                }
                self.check_node(participant)?;
                if !participant.is_source() {
                    return Err(MatrixError::NotASource(participant));
                }
                if participant == bus {
                    return Err(MatrixError::SelfRoute(bus));
                }
                let fresh = st.fresh.contains(&(bus, dst));
                let depth = match st.edges.get(&(bus, dst)) {
                    Some(e) if e.fade.is_none() => match &e.minus {
                        Some(m) if m.node == participant && m.fade.is_none() => return Ok(()),
                        Some(m) if m.node == participant => Some(m.depth),
                        Some(m) if m.fade.is_some() => return Err(MatrixError::Fading),
                        Some(m) => return Err(MatrixError::MixMinusAlreadySet(m.node)),
                        None => None,
                    },
                    _ => return Err(MatrixError::NotConnected(bus, dst)),
                };
                let depth = match depth {
                    Some(d) => {
                        // Fading back out: bring it back from where it is.
                        st.updates.push(Update::Gain {
                            slot: d,
                            target: 1.0,
                            frames: ramp,
                        });
                        d
                    }
                    None => {
                        let d = self.take_slot(st)?;
                        st.structural = true;
                        if fresh {
                            // The route itself starts silent: exclude at once.
                            st.updates.push(Update::Reset { slot: d, gain: 1.0 });
                        } else {
                            st.updates.push(Update::Reset { slot: d, gain: 0.0 });
                            st.updates.push(Update::Gain {
                                slot: d,
                                target: 1.0,
                                frames: ramp,
                            });
                        }
                        d
                    }
                };
                if let Some(e) = st.edges.get_mut(&(bus, dst)) {
                    e.minus = Some(Minus {
                        node: participant,
                        depth,
                        fade: None,
                    });
                }
            }
            Change::Include { bus, dst } => {
                let fade = self.next_fade();
                if let Some(m) = st
                    .edges
                    .get_mut(&(bus, dst))
                    .and_then(|e| e.minus.as_mut())
                    .filter(|m| m.fade.is_none())
                {
                    m.fade = Some(fade);
                    st.updates.push(Update::Gain {
                        slot: m.depth,
                        target: 0.0,
                        frames: ramp,
                    });
                }
            }
        }
        Ok(())
    }

    /// Validates the staged state and sends it as one message.
    fn commit(&mut self, st: Stage) -> Result<(), MatrixError> {
        if st.updates.is_empty() && !st.structural {
            return Ok(());
        }
        self.validate(&st.edges)?;
        let msg = match st.updates.as_slice() {
            [Update::Gain {
                slot,
                target,
                frames,
            }] if !st.structural => Msg::Gain {
                slot: *slot,
                target: *target,
                frames: *frames,
            },
            _ => {
                let mut plan = self.compile(&st.edges);
                plan.updates = st.updates;
                Msg::Plan(plan)
            }
        };
        let is_plan = matches!(msg, Msg::Plan(_));
        self.send(msg)?;
        if is_plan {
            self.generation += 1;
        }
        self.free.truncate(self.free.len() - st.taken);
        self.edges = st.edges;
        Ok(())
    }

    // ---- housekeeping ------------------------------------------------------

    /// Returns old plans for freeing and drops routes whose fade has finished.
    /// Called by every change; call it on a timer (every 10–50 ms) as well so
    /// faded routes do not linger.
    pub fn tidy(&mut self) {
        while self.ret.pop().is_ok() {}
        let consumed = self.shared.consumed.load(Ordering::Acquire);
        // Messages are applied at the start of a block (frame F0) and `consumed`
        // is published before that block's end; `frames` was published as F0 at
        // the end of the block before. So `frames`, read after `consumed`, is at
        // least F0, and a ramp applied at F0 has played its last (zero) sample
        // by F0 + ramp.
        let frames = self.shared.frames.load(Ordering::Acquire);
        let margin = u64::from(self.cfg.ramp_frames);
        let mut expired = false;
        let mut stamp = |f: &mut Fade| {
            if f.until.is_none() && consumed >= f.seq {
                f.until = Some(frames + margin);
            }
            if f.until.is_some_and(|u| frames >= u) {
                expired = true;
            }
        };
        for e in self.edges.values_mut() {
            if let Some(f) = e.fade.as_mut() {
                stamp(f);
            }
            if let Some(f) = e.minus.as_mut().and_then(|m| m.fade.as_mut()) {
                stamp(f);
            }
        }
        let retiring_free = self.buses.iter().enumerate().any(|(b, x)| {
            x.as_ref().is_some_and(|x| x.retiring)
                && !self
                    .edges
                    .keys()
                    .any(|(s, d)| *s == Node::Bus(b as u32) || *d == Node::Bus(b as u32))
        });
        if !expired && !retiring_free {
            return;
        }
        let done = |f: &Option<Fade>| f.is_some_and(|f| f.until.is_some_and(|u| frames >= u));
        let mut edges = self.edges.clone();
        let mut freed = Vec::new();
        edges.retain(|_, e| {
            if done(&e.fade) {
                freed.push(e.slot);
                if let Some(m) = &e.minus {
                    freed.push(m.depth);
                }
                return false;
            }
            if e.minus.as_ref().is_some_and(|m| done(&m.fade)) {
                if let Some(m) = e.minus.take() {
                    freed.push(m.depth);
                }
            }
            true
        });
        let mut buses = self.buses.clone();
        for (b, x) in buses.iter_mut().enumerate() {
            let node = Node::Bus(b as u32);
            if x.as_ref().is_some_and(|x| x.retiring)
                && !edges.keys().any(|(s, d)| *s == node || *d == node)
            {
                *x = None;
            }
        }
        let saved = core::mem::replace(&mut self.buses, buses);
        let plan = self.compile(&edges);
        if self.send(Msg::Plan(plan)).is_err() {
            self.buses = saved; // try again next time
            return;
        }
        self.generation += 1;
        self.edges = edges;
        // Safe to reuse: anything that uses these slots again is queued after
        // the plan that stopped reading them.
        self.free.extend(freed);
    }

    // ---- internals ---------------------------------------------------------

    fn check_node(&self, n: Node) -> Result<(), MatrixError> {
        let ok = match n {
            Node::Input(i) => i < self.cfg.inputs,
            Node::Output(o) => o < self.cfg.outputs,
            Node::Bus(b) => self
                .buses
                .get(b as usize)
                .and_then(|x| x.as_ref())
                .is_some_and(|x| !x.retiring),
        };
        if ok {
            Ok(())
        } else {
            Err(MatrixError::NoSuchNode(n))
        }
    }

    fn check_gain(db: f32) -> Result<f32, MatrixError> {
        if db.is_nan() || db > MAX_GAIN_DB {
            return Err(MatrixError::BadGain(db));
        }
        Ok(lin(db))
    }

    fn send(&mut self, msg: Msg) -> Result<(), MatrixError> {
        self.tx.push(msg).map_err(|_| MatrixError::Busy)?;
        self.sent += 1;
        Ok(())
    }

    /// Valid with live routes only (the real error), then with fading ones too.
    fn validate(&self, edges: &Edges) -> Result<(), MatrixError> {
        self.check_graph(edges, false)?;
        self.check_graph(edges, true)
            .map_err(|_| MatrixError::Fading)
    }

    fn check_graph(&self, edges: &Edges, with_fading: bool) -> Result<(), MatrixError> {
        let live = |e: &Edge| with_fading || e.fade.is_none();
        let mut adj: BTreeMap<Node, Vec<Node>> = BTreeMap::new();
        for ((s, d), e) in edges {
            if live(e) {
                adj.entry(*s).or_default().push(*d);
            }
        }
        if let Some(cycle) = find_cycle(&adj) {
            return Err(MatrixError::WouldLoop(
                cycle.iter().map(|n| self.name(*n)).collect(),
            ));
        }
        for ((x, _), e) in edges {
            let Some(m) = e.minus.as_ref().filter(|m| m.fade.is_none() && live(e)) else {
                continue;
            };
            let p = m.node;
            for &y in adj.get(&p).into_iter().flatten() {
                if y == *x {
                    continue; // the direct contribution: the one we subtract
                }
                if let Some(path) = path(&adj, y, *x) {
                    let via: Vec<String> = core::iter::once(p)
                        .chain(path)
                        .map(|n| self.name(n))
                        .collect();
                    return Err(MatrixError::MixMinusLeak {
                        participant: self.name(p),
                        bus: self.name(*x),
                        via,
                    });
                }
            }
        }
        Ok(())
    }

    fn compile(&self, edges: &Edges) -> Box<Plan> {
        let mut inbound: BTreeMap<Node, Vec<(Node, &Edge)>> = BTreeMap::new();
        for ((s, d), e) in edges {
            inbound.entry(*d).or_default().push((*s, e));
        }
        let mut plan = Plan {
            generation: self.generation + 1,
            ..Plan::default()
        };
        let mut slots = BTreeSet::new();
        let mut add_job = |plan: &mut Plan, dst: Node, idx: u32| {
            let start = plan.terms.len() as u32;
            for &(s, e) in inbound.get(&dst).into_iter().flatten() {
                let Some(src) = s.src() else { continue };
                slots.insert(e.slot);
                let minus = e.minus.as_ref().and_then(|m| {
                    slots.insert(m.depth);
                    let own = edges.get(&(m.node, s))?;
                    Some(MinusTerm {
                        src: m.node.src()?,
                        slot: own.slot,
                        depth: m.depth,
                    })
                });
                plan.terms.push(Term {
                    src,
                    slot: e.slot,
                    minus,
                });
            }
            Job {
                dst: idx,
                start,
                end: plan.terms.len() as u32,
            }
        };
        for b in self.bus_order(edges) {
            let job = add_job(&mut plan, Node::Bus(b), b);
            plan.buses.push(job);
        }
        for o in 0..self.cfg.outputs {
            let job = add_job(&mut plan, Node::Output(o), o);
            if job.end > job.start {
                plan.outputs.push(job);
            }
        }
        plan.slots = slots.into_iter().collect();
        Box::new(plan)
    }

    /// Buses in dependency order (feeders first). The graph is acyclic: every
    /// change was validated with fading routes included.
    fn bus_order(&self, edges: &Edges) -> Vec<u32> {
        let buses: Vec<u32> = (0..self.buses.len() as u32)
            .filter(|&b| self.buses[b as usize].is_some())
            .collect();
        let mut indeg: BTreeMap<u32, usize> = buses.iter().map(|&b| (b, 0)).collect();
        let mut next: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for (s, d) in edges.keys() {
            if let (Node::Bus(a), Node::Bus(b)) = (s, d) {
                next.entry(*a).or_default().push(*b);
                *indeg.entry(*b).or_default() += 1;
            }
        }
        let mut ready: BTreeSet<u32> = indeg
            .iter()
            .filter(|(_, &n)| n == 0)
            .map(|(&b, _)| b)
            .collect();
        let mut order = Vec::with_capacity(buses.len());
        while let Some(b) = ready.pop_first() {
            order.push(b);
            for &c in next.get(&b).into_iter().flatten() {
                if let Some(n) = indeg.get_mut(&c) {
                    *n -= 1;
                    if *n == 0 {
                        ready.insert(c);
                    }
                }
            }
        }
        debug_assert_eq!(order.len(), buses.len(), "bus graph has a cycle");
        order
    }
}

/// Shortest path `from` ⇝ `to` (both included), if any.
fn path(adj: &BTreeMap<Node, Vec<Node>>, from: Node, to: Node) -> Option<Vec<Node>> {
    let mut parent: BTreeMap<Node, Node> = BTreeMap::new();
    let mut queue = alloc::collections::VecDeque::from([from]);
    let mut seen = BTreeSet::from([from]);
    while let Some(n) = queue.pop_front() {
        if n == to {
            let mut p = vec![to];
            let mut c = to;
            while let Some(&q) = parent.get(&c) {
                p.push(q);
                c = q;
            }
            p.reverse();
            return Some(p);
        }
        for &m in adj.get(&n).into_iter().flatten() {
            if seen.insert(m) {
                parent.insert(m, n);
                queue.push_back(m);
            }
        }
    }
    None
}

/// A cycle, as `a → b → … → a`, if the graph has one.
fn find_cycle(adj: &BTreeMap<Node, Vec<Node>>) -> Option<Vec<Node>> {
    // Iterative DFS with colours: 1 = on the stack, 2 = finished.
    let mut colour: BTreeMap<Node, u8> = BTreeMap::new();
    for &start in adj.keys() {
        if colour.contains_key(&start) {
            continue;
        }
        let mut stack: Vec<(Node, usize)> = vec![(start, 0)];
        colour.insert(start, 1);
        while let Some(top) = stack.last_mut() {
            let (n, i) = *top;
            top.1 += 1;
            let succ = adj.get(&n).map(Vec::as_slice).unwrap_or(&[]);
            if let Some(&m) = succ.get(i) {
                match colour.get(&m) {
                    Some(1) => {
                        let at = stack.iter().position(|&(k, _)| k == m).unwrap_or(0);
                        let mut cyc: Vec<Node> = stack[at..].iter().map(|&(k, _)| k).collect();
                        cyc.push(m);
                        return Some(cyc);
                    }
                    Some(_) => {}
                    None => {
                        colour.insert(m, 1);
                        stack.push((m, 0));
                    }
                }
            } else {
                colour.insert(n, 2);
                stack.pop();
            }
        }
    }
    None
}
