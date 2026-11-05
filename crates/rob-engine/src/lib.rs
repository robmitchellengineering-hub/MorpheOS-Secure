//! The mix engine as a network node: input flows → jitter buffers → matrix →
//! output flows, one period at a time, on network time.
//!
//! **Commit early, correct late (PRIVATE: patent candidate, see `docs/ip/`).**
//! Each period is mixed at a fixed time with whatever has arrived; an input
//! period that has not is concealed by its jitter buffer, and the engine marks
//! it in the matrix before mixing. When that input's audio arrives late, the
//! jitter buffer builds a repair (truth − played); the matrix turns it into
//! the exact difference for every output that hears the input; the engine
//! sends those as correction packets (`rob-core::correction`), each twice, so
//! receivers can patch audio they have not played yet.
//!
//! **Built-in processing** (`rob-dsp`): every input flow has a stereo-linked
//! channel strip (trim, gate, EQ, compressor) between its jitter buffer and
//! the matrix; reverbs and delays sit on aux sends (a pair of matrix outputs
//! in, a pair of matrix inputs back, one period later). A correction for a
//! late input runs through its strip's linear part: exact for a flat strip,
//! close otherwise. Effect returns are not corrected (their tails are
//! seconds long; a concealed 2 ms in a reverb tail is inaudible).
//!
//! This is the engine's core loop, driven by the caller with network time
//! (`run_due`) and incoming datagrams (`on_datagram`); sockets and threads
//! live outside. The steady-state loop does not allocate.

#![cfg_attr(not(test), no_std)]

#[macro_use]
extern crate alloc;

use crate::prelude::*;
use alloc::sync::Arc;
use rob_core::correction::{self, CorrectionHeader, CORRECTION_PT};
use rob_core::rtp::{self, RobCore, RtpHeader, ROB_VERSION};
use rob_core::timestamp;
use rob_dsp::delay::{Delay, DelayParams};
use rob_dsp::output::{OutputParams, OutputProc};
use rob_dsp::reverb::{Reverb, ReverbParams};
use rob_dsp::strip::{ChannelStrip, StripParams};
use rob_matrix::{matrix, Correction, MatrixAudio, MatrixConfig, MatrixControl};
use rob_meter::level::{PeakMeter, VuMeter};
use rob_meter::loudness::{Loudness, LoudnessMeter};
use rob_meter::spectrum::Tap;
use rob_meter::stereo::Correlation;
use rob_node::aes67::Aes67Depacketizer;
use rob_node::depacketize::Depacketizer;
use rob_node::flow::{FlowFormat, CHANNELS};
use rob_node::jitter::JitterBuffer;
use rob_node::packetize::{Packetizer, MAX_PACKET};
use rob_sync::inputclock::InputClock;
use spin::Mutex;

/// f64/f32 float methods that live in `std`, re-provided over `libm` so the
/// crate links without std. In a std build the inherent methods win; in no_std
/// these are the only ones. Files bring it in via `use crate::prelude::*;`.
pub trait FloatExt: Sized {
    fn sin(self) -> Self;
    fn cos(self) -> Self;
    fn tan(self) -> Self;
    fn round(self) -> Self;
    fn floor(self) -> Self;
    fn ceil(self) -> Self;
    fn trunc(self) -> Self;
    fn exp(self) -> Self;
    fn ln(self) -> Self;
    fn log10(self) -> Self;
    fn powf(self, e: Self) -> Self;
    fn sqrt(self) -> Self;
    fn powi(self, e: i32) -> Self;
    fn rem_euclid(self, rhs: Self) -> Self;
    fn div_euclid(self, rhs: Self) -> Self;
    fn atan2(self, rhs: Self) -> Self;
}

impl FloatExt for f64 {
    fn sin(self) -> f64 {
        libm::sin(self)
    }
    fn cos(self) -> f64 {
        libm::cos(self)
    }
    fn tan(self) -> f64 {
        libm::tan(self)
    }
    fn round(self) -> f64 {
        libm::round(self)
    }
    fn floor(self) -> f64 {
        libm::floor(self)
    }
    fn ceil(self) -> f64 {
        libm::ceil(self)
    }
    fn trunc(self) -> f64 {
        libm::trunc(self)
    }
    fn exp(self) -> f64 {
        libm::exp(self)
    }
    fn ln(self) -> f64 {
        libm::log(self)
    }
    fn log10(self) -> f64 {
        libm::log10(self)
    }
    fn powf(self, e: f64) -> f64 {
        libm::pow(self, e)
    }
    fn sqrt(self) -> f64 {
        libm::sqrt(self)
    }
    fn powi(self, e: i32) -> f64 {
        let mut b = self;
        let mut n = e.unsigned_abs();
        let mut r = 1.0f64;
        while n > 0 {
            if n & 1 == 1 {
                r *= b;
            }
            b *= b;
            n >>= 1;
        }
        if e < 0 {
            1.0 / r
        } else {
            r
        }
    }
    fn rem_euclid(self, rhs: f64) -> f64 {
        let r = self % rhs;
        if r < 0.0 {
            r + rhs.abs()
        } else {
            r
        }
    }
    fn div_euclid(self, rhs: f64) -> f64 {
        let r = self % rhs;
        let r = if r < 0.0 { r + rhs.abs() } else { r };
        (self - r) / rhs
    }
    fn atan2(self, rhs: f64) -> f64 {
        libm::atan2(self, rhs)
    }
}

impl FloatExt for f32 {
    fn sin(self) -> f32 {
        libm::sinf(self)
    }
    fn cos(self) -> f32 {
        libm::cosf(self)
    }
    fn tan(self) -> f32 {
        libm::tanf(self)
    }
    fn round(self) -> f32 {
        libm::roundf(self)
    }
    fn floor(self) -> f32 {
        libm::floorf(self)
    }
    fn ceil(self) -> f32 {
        libm::ceilf(self)
    }
    fn trunc(self) -> f32 {
        libm::truncf(self)
    }
    fn exp(self) -> f32 {
        libm::expf(self)
    }
    fn ln(self) -> f32 {
        libm::logf(self)
    }
    fn log10(self) -> f32 {
        libm::log10f(self)
    }
    fn powf(self, e: f32) -> f32 {
        libm::powf(self, e)
    }
    fn sqrt(self) -> f32 {
        libm::sqrtf(self)
    }
    fn powi(self, e: i32) -> f32 {
        let mut b = self;
        let mut n = e.unsigned_abs();
        let mut r = 1.0f32;
        while n > 0 {
            if n & 1 == 1 {
                r *= b;
            }
            b *= b;
            n >>= 1;
        }
        if e < 0 {
            1.0 / r
        } else {
            r
        }
    }
    fn rem_euclid(self, rhs: f32) -> f32 {
        let r = self % rhs;
        if r < 0.0 {
            r + rhs.abs()
        } else {
            r
        }
    }
    fn div_euclid(self, rhs: f32) -> f32 {
        let r = self % rhs;
        let r = if r < 0.0 { r + rhs.abs() } else { r };
        (self - r) / rhs
    }
    fn atan2(self, rhs: f32) -> f32 {
        libm::atan2f(self, rhs)
    }
}

/// The alloc items the crate's modules need unqualified. `use crate::prelude::*;`
/// is a glob import, so an unused member does not warn.
pub mod prelude {
    pub use super::FloatExt;
    pub use alloc::boxed::Box;
    pub use alloc::collections::VecDeque;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec::Vec;
}

/// Most phone beltpacks one engine hosts (design 29). Each phone reserves two
/// matrix inputs (its mic) and two outputs (its listen mix).
pub const PHONE_CAP: usize = 8;

/// The full-duplex audio link to one phone beltpack (design 29): the browser
/// writes the mic (uplink, mono f32) and reads the listen mix (downlink,
/// stereo interleaved f32); the engine reads the mic and writes the listen
/// mix, once a period. A `Mutex<VecDeque>` is enough for a first cut — the
/// WebSocket thread holds it only to copy a frame, the audio thread briefly.
#[derive(Clone, Default)]
pub struct PhoneAudio {
    /// Browser → engine: mono samples the mic captured.
    pub uplink: Arc<Mutex<VecDeque<f32>>>,
    /// Engine → browser: stereo interleaved samples of the listen mix.
    pub downlink: Arc<Mutex<VecDeque<f32>>>,
}

impl PhoneAudio {
    pub fn new() -> PhoneAudio {
        PhoneAudio {
            uplink: Arc::new(Mutex::new(VecDeque::new())),
            downlink: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}

/// A phone beltpack's clock-following state (design 29, slice 9). The browser's
/// AudioContext runs on the phone's own crystal, free-running against the
/// engine's, so a fixed upsample ratio slowly drifts (the uplink queue grows or
/// starves). The audio thread reads the mic through a fractional position and
/// slews the ratio with a first-order PLL on the queue fill — the same idea as
/// a node's PLL, in miniature — so the phone follows the engine clock.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PhoneClock {
    /// Fractional read position in the uplink queue (phone-rate samples).
    pos: f64,
    /// Phone-rate samples consumed per 48 kHz output frame (≈ phone_rate / fs).
    step: f64,
    /// Smoothed uplink queue fill (phone-rate samples).
    fill: f64,
}

impl PhoneClock {
    fn new(base: f64) -> PhoneClock {
        PhoneClock {
            pos: 0.0,
            step: base,
            fill: 0.0,
        }
    }
}

/// Target uplink queue fill (phone-rate samples): ~21 ms of headroom.
const PHONE_TARGET_FILL: f64 = 512.0;
/// How fast the fill estimate follows the live queue (per period).
const PHONE_FILL_SMOOTH: f64 = 0.01;
/// Proportional clock-recovery gain (per sample of fill error). Small on
/// purpose: a clock drifts ~100 ppm, so a 10-sample fill error is plenty.
const PHONE_PLL_GAIN: f64 = 1e-5;
/// The slew the PLL may apply, as a fraction of the base ratio (±1%).
const PHONE_PLL_SLEW: f64 = 0.01;

/// The matrix channels a phone beltpack uses (design 29).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhoneRoutes {
    /// Its mic (mono, mapped to both channels) — the last two matrix inputs.
    pub mic: [usize; 2],
    /// Its listen mix — the last two matrix outputs.
    pub listen: [usize; 2],
}

/// Most mono matrix channels the engine core handles (stack-allocated slices).
pub const MAX_CHANNELS: usize = 128;
/// Most frames one correction can cover (period + fade back in, with room
/// for an EQ's ringing).
const MAX_CORR_FRAMES: usize = 512;
/// Ringing carried after a concealed span on an input whose EQ is active
/// (8 ms: an 80 Hz high-pass has rung down by ~-30 dB).
const EQ_TAIL: usize = 384;

/// A stereo input flow and the matrix inputs it feeds: `channels` is the
/// post-strip signal, `pre_channels` the raw signal before the strip (design
/// 30 pre/post tap).
#[derive(Debug, Clone, Copy)]
pub struct InputFlow {
    pub ssrc: u32,
    pub channels: [u32; CHANNELS],
    pub pre_channels: [u32; CHANNELS],
}

/// A stereo output flow: the two matrix outputs it carries, its own SSRC, and
/// the SSRC of its correction stream.
#[derive(Debug, Clone, Copy)]
pub struct OutputFlow {
    pub ssrc: u32,
    pub correction_ssrc: u32,
    pub channels: [u32; CHANNELS],
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub fmt: FlowFormat,
    pub inputs: Vec<InputFlow>,
    pub outputs: Vec<OutputFlow>,
    /// How long after a period's last sample the engine mixes it.
    pub mix_delay_ns: u64,
    /// Receivers' playout deadline after capture, for the `budget` field.
    pub latency_ns: u64,
    pub payload_type: u8,
    pub ext_id: u8,
    /// Jitter-buffer size per input, periods.
    pub jitter_periods: usize,
    /// Send each correction twice (the repeat one period later).
    pub repeat_corrections: bool,
    /// Inputs are stamped in the sender's own (monotonic) clock, not network
    /// time: unwrap their 32-bit timestamps against that clock and map them to
    /// engine time with the stream clock (design 26, S4). Off keeps the
    /// pre-S4 behaviour (inputs are network-time stamped).
    pub sender_clock_inputs: bool,
}

/// What the engine did, for telemetry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineStats {
    pub periods: u64,
    pub packets_sent: u64,
    /// Input periods concealed in the mix.
    pub concealed: u64,
    /// Input periods mixed from a 16- or 10-bit copy (upgradable if the full
    /// copy arrives late).
    pub degraded: u64,
    /// Repairs turned into corrections, and correction packets sent.
    pub repairs: u64,
    pub corrections_sent: u64,
    /// Repairs the matrix had no record for (too old, or never marked).
    pub repairs_unknown: u64,
}

/// Which channel or bus the spectrum tap follows (design 18): one at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpectrumSource {
    Input(usize),
    Output(usize),
}

struct Input {
    flow: InputFlow,
    /// Peak after the channel strip (left, right), for the input meters.
    peak: [PeakMeter; CHANNELS],
    /// VU level after the channel strip (left, right), for the analogue meter
    /// bridge (design 30 live desk).
    vu: [VuMeter; CHANNELS],
    /// ROB flows: their depacketizer. AES67 pairs: none (their stream's
    /// depacketizer feeds them, see `Aes67In`).
    depack: Option<Depacketizer>,
    jitter: JitterBuffer,
    /// Maps this input's mono-stamped timestamps onto engine time (design 26,
    /// S4). Unused for AES67 inputs, which are PTP-timed.
    clock: InputClock,
    strip: ChannelStrip,
}

/// An AES67 stream the engine sends (to a stage box's outputs): matrix
/// outputs in channel order, packets of the stream's packet time.
struct Aes67Out {
    stream: rob_core::aes67::Stream,
    outputs: Vec<usize>,
    ssrc: u32,
    seq: u16,
    pcm: Vec<i32>,
}

/// An AES67 stream (a stage box) and the engine inputs its pairs feed.
struct Aes67In {
    rx: Aes67Depacketizer,
    inputs: Vec<usize>,
}

/// A built-in effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Effect {
    Reverb(ReverbParams),
    Delay(DelayParams),
}

enum Unit {
    Reverb(Box<Reverb>),
    Delay(Box<Delay>),
}

/// An effect on an aux send: matrix outputs `send` feed it, its output comes
/// back on matrix inputs `ret` (one period later).
struct Fx {
    unit: Unit,
    send: [usize; CHANNELS],
    ret: [usize; CHANNELS],
    /// Its last output, mixed in the next period.
    out: [Vec<f32>; CHANNELS],
}

/// Two distinct `p`-long channels of a planar buffer, both mutable.
fn two_mut(buf: &mut [f32], a: usize, b: usize, p: usize) -> Option<(&mut [f32], &mut [f32])> {
    if a == b || (a.max(b) + 1) * p > buf.len() {
        return None;
    }
    let (lo, hi) = (a.min(b), a.max(b));
    let (x, y) = buf.split_at_mut(hi * p);
    let (first, second) = (&mut x[lo * p..lo * p + p], &mut y[..p]);
    Some(if a < b {
        (first, second)
    } else {
        (second, first)
    })
}

struct Output {
    flow: OutputFlow,
    pack: Packetizer,
    /// Output-bus processing (EQ and limiter), applied before the loudness.
    proc: OutputProc,
    /// Loudness and true peak of what it carries (the on-time mix).
    loudness: LoudnessMeter,
    /// VU level of the output bus (left, right), for the analogue meter bridge
    /// (design 30 live desk).
    vu: [VuMeter; CHANNELS],
    corr_seq: u16,
    /// The last correction, kept for its repeat: (first sample, frames, data).
    repeat: Option<(i64, usize)>,
    repeat_data: Vec<f32>,
}

pub struct Engine {
    cfg: EngineConfig,
    ctl: MatrixControl,
    audio: MatrixAudio,
    inputs: Vec<Input>,
    outputs: Vec<Output>,
    /// Matrix output index → (output flow, channel).
    out_map: Vec<Option<(usize, usize)>>,
    /// Network sample index of matrix frame 0.
    base: Option<i64>,
    next: i64,
    in_buf: Vec<f32>,
    out_buf: Vec<f32>,
    pcm: Vec<i32>,
    packet: Vec<u8>,
    repair: Vec<f32>,
    /// Per output flow: correction being accumulated (interleaved) and whether
    /// anything landed in it.
    corr_acc: Vec<f32>,
    corr_hit: Vec<bool>,
    fx: Vec<Fx>,
    aes: Vec<Aes67In>,
    aes_out: Vec<Aes67Out>,
    /// The spectrum tap: the audio thread writes the selected source's period
    /// (see `mix_period`), the analysis step drains it into the RTA.
    spectrum: Tap,
    spectrum_src: Option<SpectrumSource>,
    /// The stereo phase correlation of the tapped source (design 18): +1 mono
    /// compatible, 0 unrelated, -1 out of phase. Fed alongside the spectrum.
    correlation: Correlation,
    /// The phone beltpacks (design 29), in slot order, and the matrix channel
    /// where slot 0's mic/listen begin (each slot is the next two channels).
    phones: Vec<PhoneAudio>,
    phone_base_in: usize,
    phone_base_out: usize,
    /// The beltpack sample rate (design 29): speech runs lower than the 48 kHz
    /// engine, and the audio thread resamples at the ring boundary.
    phone_rate: usize,
    /// Clock-following state per phone (design 29 slice 9), parallel to `phones`.
    phone_clocks: Vec<PhoneClock>,
    pub stats: EngineStats,
}

impl Engine {
    /// Builds the engine. Allocates; call before running.
    pub fn new(cfg: EngineConfig, mut mcfg: MatrixConfig) -> Engine {
        let p = cfg.fmt.period_frames;
        mcfg.max_block = mcfg.max_block.max(p);
        mcfg.correction_span = mcfg.correction_span.max(p + rob_node::jitter::FADE);
        // Spare matrix channels for the phone beltpacks (design 29): each phone
        // takes two inputs (its mic) and two outputs (its listen mix).
        let phone_base_in = mcfg.inputs as usize;
        let phone_base_out = mcfg.outputs as usize;
        mcfg.inputs += 2 * PHONE_CAP as u32;
        mcfg.outputs += 2 * PHONE_CAP as u32;
        let n_in = mcfg.inputs as usize;
        let n_out = mcfg.outputs as usize;
        assert!(
            n_in <= MAX_CHANNELS && n_out <= MAX_CHANNELS,
            "engine core limit"
        );
        let (ctl, audio) = matrix(mcfg);
        let mut out_map = vec![None; n_out];
        for (k, o) in cfg.outputs.iter().enumerate() {
            for (c, &ch) in o.channels.iter().enumerate() {
                if let Some(m) = out_map.get_mut(ch as usize) {
                    *m = Some((k, c));
                }
            }
        }
        let inputs = cfg
            .inputs
            .iter()
            .map(|&flow| Input {
                flow,
                peak: core::array::from_fn(|_| {
                    PeakMeter::new(f64::from(cfg.fmt.sample_rate), 1.5, 20.0)
                }),
                vu: core::array::from_fn(|_| VuMeter::new(f64::from(cfg.fmt.sample_rate), -18.0)),
                depack: Some(Depacketizer::new(cfg.fmt, Some(flow.ssrc), cfg.ext_id)),
                jitter: JitterBuffer::new(cfg.fmt, cfg.jitter_periods),
                clock: InputClock::new(Default::default()),
                strip: ChannelStrip::new(StripParams::default(), f64::from(cfg.fmt.sample_rate)),
            })
            .collect();
        let outputs = cfg
            .outputs
            .iter()
            .map(|&flow| Output {
                flow,
                pack: Packetizer::new(cfg.fmt, flow.ssrc, cfg.payload_type, cfg.ext_id),
                proc: OutputProc::new(OutputParams::default(), f64::from(cfg.fmt.sample_rate)),
                loudness: LoudnessMeter::new(f64::from(cfg.fmt.sample_rate)),
                vu: core::array::from_fn(|_| VuMeter::new(f64::from(cfg.fmt.sample_rate), -18.0)),
                corr_seq: 0,
                repeat: None,
                repeat_data: vec![0.0; MAX_CORR_FRAMES * CHANNELS],
            })
            .collect();
        let n_flows = cfg.outputs.len();
        Engine {
            ctl,
            audio,
            inputs,
            outputs,
            out_map,
            base: None,
            next: 0,
            in_buf: vec![0.0; n_in * p],
            out_buf: vec![0.0; n_out * p],
            pcm: vec![0; p * CHANNELS],
            packet: vec![0; MAX_PACKET + 64],
            repair: vec![0.0; MAX_CORR_FRAMES],
            corr_acc: vec![0.0; n_flows * MAX_CORR_FRAMES * CHANNELS],
            corr_hit: vec![false; n_flows],
            fx: Vec::new(),
            aes: Vec::new(),
            aes_out: Vec::new(),
            spectrum: Tap::new(f64::from(cfg.fmt.sample_rate)),
            spectrum_src: None,
            correlation: Correlation::new(f64::from(cfg.fmt.sample_rate), 0.5),
            phones: Vec::new(),
            phone_base_in,
            phone_base_out,
            phone_rate: 24_000,
            phone_clocks: Vec::new(),
            cfg,
            stats: EngineStats::default(),
        }
    }

    /// Enables one more phone beltpack (design 29): the browser's mic lands on
    /// the next two matrix inputs, and the next two outputs are its listen
    /// mix (route sources to them as usual). Returns those channels.
    pub fn enable_phone(&mut self, audio: PhoneAudio) -> PhoneRoutes {
        let slot = self.phones.len();
        let base = self.phone_rate as f64 / self.cfg.fmt.sample_rate as f64;
        self.phones.push(audio);
        self.phone_clocks.push(PhoneClock::new(base));
        PhoneRoutes {
            mic: [
                self.phone_base_in + 2 * slot,
                self.phone_base_in + 2 * slot + 1,
            ],
            listen: [
                self.phone_base_out + 2 * slot,
                self.phone_base_out + 2 * slot + 1,
            ],
        }
    }

    /// Sets the beltpack sample rate (design 29). It must divide 48 kHz and the
    /// period so each period carries a whole number of phone samples (24 000,
    /// 12 000, …). The audio thread resamples at the ring boundary.
    pub fn set_phone_rate(&mut self, rate: usize) -> Result<(), String> {
        let fs = self.cfg.fmt.sample_rate as usize;
        if rate == 0 || fs % rate != 0 || self.cfg.fmt.period_frames % (fs / rate) != 0 {
            return Err(format!(
                "phone rate {rate}: must divide {fs} and a period of {} frames",
                self.cfg.fmt.period_frames
            ));
        }
        self.phone_rate = rate;
        Ok(())
    }

    /// The beltpack sample rate.
    pub fn phone_rate(&self) -> usize {
        self.phone_rate
    }

    /// The routing: crosspoints, buses, mix-minus (see `rob-matrix`).
    pub fn matrix(&mut self) -> &mut MatrixControl {
        &mut self.ctl
    }

    /// Input flow `k`'s channel strip settings (applied click-free, no
    /// allocation).
    pub fn set_strip(&mut self, k: usize, p: StripParams) {
        if let Some(i) = self.inputs.get_mut(k) {
            i.strip.set(p);
        }
    }

    pub fn strip(&self, k: usize) -> Option<&ChannelStrip> {
        self.inputs.get(k).map(|i| &i.strip)
    }

    /// Output flow `j`'s output-bus processing (EQ + limiter), applied
    /// click-free, no allocation.
    pub fn set_output(&mut self, j: usize, p: OutputParams) {
        if let Some(o) = self.outputs.get_mut(j) {
            o.proc.set(p);
        }
    }

    pub fn output_params(&self, j: usize) -> Option<OutputParams> {
        self.outputs.get(j).map(|o| o.proc.params())
    }

    /// Output flow `j`'s limiter gain reduction (dB, <= 0) and on/off, for the
    /// gain-reduction meter.
    pub fn output_limiter(&self, j: usize) -> Option<(f32, bool)> {
        self.outputs
            .get(j)
            .map(|o| (o.proc.limiter_gr_db(), o.proc.limiter_on()))
    }

    /// Adds an effect fed by matrix outputs `send` and returning on matrix
    /// inputs `ret` (route sources to `send` and `ret` to destinations with
    /// the matrix as usual). Allocates: call before running. Returns its index.
    pub fn add_fx(&mut self, effect: Effect, send: [u32; CHANNELS], ret: [u32; CHANNELS]) -> usize {
        let fs = f64::from(self.cfg.fmt.sample_rate);
        let p = self.cfg.fmt.period_frames;
        let unit = match effect {
            Effect::Reverb(r) => Unit::Reverb(Box::new(Reverb::new(r, fs))),
            Effect::Delay(d) => Unit::Delay(Box::new(Delay::new(d, fs))),
        };
        self.fx.push(Fx {
            unit,
            send: send.map(|c| c as usize),
            ret: ret.map(|c| c as usize),
            out: [vec![0.0; p], vec![0.0; p]],
        });
        self.fx.len() - 1
    }

    /// Adds an AES67 stream (a stage box): each of `rx`'s pairs becomes an
    /// engine input on the matrix inputs `channels[k]` (route them as usual;
    /// strips are numbered after the ROB inputs, in order). The engine must
    /// follow PTP for the stream's timestamps to be its network time.
    /// Allocates: call before running. Returns the stream's index.
    pub fn add_aes67(&mut self, rx: Aes67Depacketizer, channels: &[[u32; CHANNELS]]) -> usize {
        let fmt = rx.format();
        // Keep about as much as an ROB input's buffer holds.
        let periods =
            (self.cfg.jitter_periods * self.cfg.fmt.period_frames).div_ceil(fmt.period_frames);
        let fs = f64::from(self.cfg.fmt.sample_rate);
        let mut inputs = Vec::with_capacity(rx.pairs());
        for ch in channels.iter().take(rx.pairs()) {
            let mut jitter = JitterBuffer::new(fmt, periods);
            if self.base.is_some() {
                jitter.start_at(self.next);
            }
            self.inputs.push(Input {
                peak: core::array::from_fn(|_| PeakMeter::new(fs, 1.5, 20.0)),
                vu: core::array::from_fn(|_| VuMeter::new(fs, -18.0)),
                flow: InputFlow {
                    ssrc: 0,
                    channels: *ch,
                    pre_channels: *ch,
                },
                depack: None,
                jitter,
                clock: InputClock::new(Default::default()),
                strip: ChannelStrip::new(StripParams::default(), fs),
            });
            inputs.push(self.inputs.len() - 1);
        }
        self.aes.push(Aes67In { rx, inputs });
        self.aes.len() - 1
    }

    /// One datagram of AES67 stream `k`.
    pub fn on_aes67(&mut self, k: usize, datagram: &[u8], now_ns: u64) {
        let Some(a) = self.aes.get_mut(k) else { return };
        let (inputs, ins) = (&mut self.inputs, &a.inputs);
        a.rx.accept(datagram, now_ns, |pair, c| {
            if let Some(i) = ins.get(pair).and_then(|&i| inputs.get_mut(i)) {
                i.jitter.insert(c);
            }
        });
    }

    /// Adds an AES67 stream the engine sends: `stream`'s channels carry
    /// matrix outputs `outputs` (in order), as SSRC `ssrc`, its packets go
    /// to the `send` callback like everything else. The packet time must
    /// divide the engine's period (1 or 2 ms). Late-input corrections cannot
    /// reach a stage box, so these outputs carry the on-time mix. Allocates:
    /// call before running.
    pub fn add_aes67_output(
        &mut self,
        stream: rob_core::aes67::Stream,
        outputs: &[u32],
        ssrc: u32,
    ) -> Result<usize, String> {
        let p = self.cfg.fmt.period_frames;
        let f = stream.frames();
        if f == 0 || p % f != 0 {
            return Err(format!(
                "packet time {} ms must divide the engine period",
                stream.ptime_ms
            ));
        }
        if outputs.len() != stream.channels {
            return Err(format!(
                "{} outputs for {} channels",
                outputs.len(),
                stream.channels
            ));
        }
        if 12 + f * stream.channels * 3 > MAX_PACKET {
            return Err("too many channels for one packet".into());
        }
        self.aes_out.push(Aes67Out {
            pcm: vec![0; f * stream.channels],
            outputs: outputs.iter().map(|&o| o as usize).collect(),
            stream,
            ssrc,
            seq: 0,
        });
        Ok(self.aes_out.len() - 1)
    }

    /// Output flow `j`'s loudness (EBU R128) and true peak.
    pub fn output_loudness(&self, j: usize) -> Option<Loudness> {
        self.outputs.get(j).map(|o| o.loudness.read())
    }

    /// Input `k`'s peak after its strip (left, right; bar and held, dBFS).
    /// Input `k`'s jitter-buffer playout counts (concealed periods and
    /// events): what the desk's grade is computed from (design 24).
    pub fn input_playout(&self, k: usize) -> Option<rob_node::jitter::PlayoutStats> {
        self.inputs.get(k).map(|i| i.jitter.stats)
    }

    /// Whether input `k`'s path constant is learned yet (design 26, S4): true
    /// once enough `SYNC_REPORT`s have been averaged. The desk's Network panel
    /// shows this as "calibrated" vs "uncalibrated" (S5).
    pub fn input_calibrated(&self, k: usize) -> bool {
        self.inputs
            .get(k)
            .is_some_and(|i| i.clock.status().0.is_some())
    }

    pub fn input_peak_db(&self, k: usize) -> Option<[(f64, f64); CHANNELS]> {
        self.inputs
            .get(k)
            .map(|i| [i.peak[0].read_db(), i.peak[1].read_db()])
    }

    /// Output `j`'s VU level (left, right; dB relative to the -18 dBFS ref).
    pub fn output_vu(&self, j: usize) -> Option<[f64; CHANNELS]> {
        self.outputs
            .get(j)
            .map(|o| [o.vu[0].read_vu(), o.vu[1].read_vu()])
    }

    /// Input `k`'s VU level after its strip (left, right; dB relative to -18).
    pub fn input_vu(&self, k: usize) -> Option<[f64; CHANNELS]> {
        self.inputs
            .get(k)
            .map(|i| [i.vu[0].read_vu(), i.vu[1].read_vu()])
    }

    /// Which channel or bus the spectrum tap follows (`None`: off). This is
    /// metering only: it changes nothing on the audio path. Rare control
    /// event, so it may allocate (a fresh tap clears the old source's data).
    pub fn set_spectrum_source(&mut self, src: Option<SpectrumSource>) {
        if src != self.spectrum_src {
            self.spectrum = Tap::new(f64::from(self.cfg.fmt.sample_rate));
            self.correlation = Correlation::new(f64::from(self.cfg.fmt.sample_rate), 0.5);
            self.spectrum_src = src;
        }
    }

    /// Runs the analysis step: feeds the tapped samples to the RTA (the FFT
    /// runs here, off the packet path). Call from the metering step.
    pub fn spectrum_drain(&mut self) {
        if self.spectrum_src.is_some() {
            self.spectrum.drain();
        }
    }

    /// The 1/12-octave band levels (20 Hz .. 20 kHz) in dB, or `None`
    /// when no source is selected.
    pub fn spectrum_bands_db(&self) -> Option<[f64; rob_meter::spectrum::RTA_BANDS]> {
        self.spectrum_src.map(|_| self.spectrum.bands_db())
    }

    /// The stereo phase correlation of the tapped source (-1 .. +1), or
    /// `None` when no source is selected (design 18).
    pub fn correlation(&self) -> Option<f64> {
        self.spectrum_src.map(|_| self.correlation.read())
    }

    /// The tapped source's left/right balance in dB (positive = right louder).
    pub fn balance_db(&self) -> Option<f64> {
        self.spectrum_src.map(|_| self.correlation.balance_db())
    }

    /// AES67 stream `k`'s receive counters.
    pub fn aes67_stats(&self, k: usize) -> Option<rob_node::aes67::Aes67Stats> {
        self.aes.get(k).map(|a| a.rx.stats)
    }

    /// New settings for effect `k` (same kind), applied without allocation.
    pub fn set_fx(&mut self, k: usize, effect: Effect) {
        match (self.fx.get_mut(k).map(|f| &mut f.unit), effect) {
            (Some(Unit::Reverb(r)), Effect::Reverb(p)) => r.set(p),
            (Some(Unit::Delay(d)), Effect::Delay(p)) => d.set(p),
            _ => {}
        }
    }

    fn samples(&self, ns: u64) -> i64 {
        timestamp::samples_at(ns, self.cfg.fmt.sample_rate) as i64
    }

    /// One incoming datagram (any input flow, any order, late ones included).
    ///
    /// With `sender_clock_inputs` (design 26, S4) the timestamps are in the
    /// sender's own (monotonic) clock: the 32-bit RTP timestamp is unwrapped
    /// against that clock, the input's stream clock is fed, and the chunk is
    /// re-stamped in engine time before it reaches the jitter buffer.
    pub fn on_datagram(&mut self, datagram: &[u8], now_ns: u64) {
        let Ok(p) = rtp::parse(datagram, self.cfg.ext_id) else {
            return;
        };
        let ssrc = p.header.ssrc;
        let rate = self.cfg.fmt.sample_rate;
        let sender_clock = self.cfg.sender_clock_inputs;
        if let Some(inp) = self
            .inputs
            .iter_mut()
            .find(|i| i.depack.is_some() && i.flow.ssrc == ssrc)
        {
            if !sender_clock {
                let jb = &mut inp.jitter;
                if let Some(d) = inp.depack.as_mut() {
                    d.accept(datagram, now_ns, |c| jb.insert(c));
                }
                return;
            }
            let Input {
                clock,
                depack,
                jitter,
                ..
            } = inp;
            let sender_now = clock.sender_now(now_ns).unwrap_or(0);
            let sent_ns = timestamp::ns_at(
                timestamp::unwrap(p.header.timestamp, timestamp::samples_at(sender_now, rate)),
                rate,
            );
            clock.on_packet(sent_ns, now_ns);
            if let Some(d) = depack.as_mut() {
                let clock = &*clock;
                let period_ns =
                    1_000_000_000.0 * self.cfg.fmt.period_frames as f64 / f64::from(rate);
                d.accept(datagram, sender_now, |c| {
                    let sent_ns = (c.first_sample.max(0) as u64).saturating_mul(1_000_000_000)
                        / u64::from(rate);
                    if let Some(m) = clock.engine_time(sent_ns) {
                        let mut c2 = *c;
                        // The sender's timestamps are period-aligned; the engine
                        // clock's epoch has an arbitrary sub-period phase, so
                        // re-stamp on the engine's period grid (a < half-period
                        // snap, absorbed by the jitter buffer's mix delay).
                        let snapped = (m.engine_ns / period_ns).round() * period_ns;
                        c2.first_sample =
                            timestamp::samples_at(snapped.max(0.0) as u64, rate) as i64;
                        jitter.insert(&c2);
                    }
                });
            }
        }
    }

    /// One calibration sample from the sync leader: input `ssrc`'s node says
    /// "at its clock reading `sender_ns` the engine's time was about
    /// `engine_ns`" (design 26, S4). Averaged by that input's `InputClock`.
    pub fn on_calibration(&mut self, ssrc: u32, sender_ns: u64, engine_ns: u64) {
        if let Some(inp) = self.inputs.iter_mut().find(|i| i.flow.ssrc == ssrc) {
            inp.clock.on_calibration(sender_ns, engine_ns);
        }
    }

    /// Recomputes each input's learned constant. Call about once a second from
    /// the network thread (it is O(window) per input).
    pub fn refresh_clocks(&mut self) {
        for inp in &mut self.inputs {
            inp.clock.refresh();
        }
    }

    /// Mixes every period that is due at network time `now_ns` and hands each
    /// packet to `send` (audio, then corrections). Returns periods mixed.
    pub fn run_due(&mut self, now_ns: u64, mut send: impl FnMut(&[u8])) -> usize {
        let p = self.cfg.fmt.period_frames as i64;
        let delay = self.samples(self.cfg.mix_delay_ns);
        let now = self.samples(now_ns);
        if self.base.is_none() {
            // Network time near zero would give negative sample numbers,
            // which wrap in the 32-bit RTP timestamp. Real network time never
            // is (the leader offsets it); refuse rather than misbehave.
            if now < delay + 2 * p {
                return 0;
            }
            let first = (now - delay - p).div_euclid(p) * p;
            self.base = Some(first);
            self.next = first;
            for i in &mut self.inputs {
                i.jitter.start_at(first);
            }
        }
        let mut mixed = 0;
        while self.next + p + delay <= now {
            self.mix_period(now_ns, &mut send);
            mixed += 1;
        }
        mixed
    }

    fn mix_period(&mut self, now_ns: u64, send: &mut impl FnMut(&[u8])) {
        let p = self.cfg.fmt.period_frames;
        let n = self.next;
        let base = self.base.unwrap_or(n);
        // Repeats of last period's corrections go first.
        self.send_repeats(now_ns, send);
        // 1. Inputs: render each jitter buffer into the matrix input channels.
        for (ki, inp) in self.inputs.iter_mut().enumerate() {
            let [l, r] = inp.flow.channels.map(|c| c as usize);
            let [pl, pr] = inp.flow.pre_channels.map(|c| c as usize);
            let mut k = 0;
            let buf = &mut self.in_buf;
            inp.jitter.render_until(n + p as i64, |f| {
                if let Some(v) = buf.get_mut(l * p + k) {
                    *v = f[0];
                }
                if let Some(v) = buf.get_mut(r * p + k) {
                    *v = f[1];
                }
                // The raw (pre-strip) copy for the pre/post tap (design 30).
                if let Some(v) = buf.get_mut(pl * p + k) {
                    *v = f[0];
                }
                if let Some(v) = buf.get_mut(pr * p + k) {
                    *v = f[1];
                }
                k += 1;
            });
            // The channel strip (a flat one changes nothing).
            if let Some((a, b)) = two_mut(&mut self.in_buf, l, r, p) {
                inp.strip.process(&mut [a, b]);
                inp.peak[0].process(a);
                inp.peak[1].process(b);
                inp.vu[0].process(a);
                inp.vu[1].process(b);
                // Spectrum tap (design 18): the post-strip input, copied into
                // the lock-free ring — no FFT here, so the packet path is
                // untouched. The correlation is fed the same stereo period.
                if matches!(self.spectrum_src, Some(SpectrumSource::Input(x)) if x == ki) {
                    self.spectrum.write_stereo(a, b);
                    self.correlation.process(a, b);
                }
            }
            // 2. Mark what was concealed, before mixing it.
            let ring = inp.strip.rings();
            let audio = &mut self.audio;
            let stats = &mut self.stats;
            inp.jitter.drain_concealed(|first, len, concealed| {
                if concealed {
                    stats.concealed += 1;
                } else {
                    stats.degraded += 1;
                }
                let m = (first - base) as u64;
                // With an active EQ, keep the record through its ringing too.
                let len = if ring {
                    (len as usize + EQ_TAIL).min(MAX_CORR_FRAMES) as u32
                } else {
                    len
                };
                audio.mark_concealed(l as u32, m, len);
                audio.mark_concealed(r as u32, m, len);
            });
        }
        // Effect returns from last period.
        for fx in &self.fx {
            for (c, &ch) in fx.ret.iter().enumerate() {
                if let Some(dst) = self.in_buf.get_mut(ch * p..ch * p + p) {
                    dst.copy_from_slice(&fx.out[c]);
                }
            }
        }
        // The phone beltpacks' mics (design 29 slice 9): each WebSocket writes
        // mono PCM at `phone_rate`, but on the phone's own clock — free-running
        // against the engine's. The audio thread reads through a fractional
        // position and slews the ratio with a PLL on the queue fill, so the
        // phone follows the engine clock (no slow drift) instead of a fixed
        // whole-divisor upsample. The same sample lands on both channels.
        for (slot, phone) in self.phones.iter().enumerate() {
            let mut uplink = phone.uplink.lock();
            let a = self.phone_base_in + 2 * slot;
            let b = a + 1;
            let clock = &mut self.phone_clocks[slot];
            for k in 0..p {
                clock.pos += clock.step;
                let i0 = clock.pos.floor() as usize;
                let i1 = i0 + 1;
                let frac = (clock.pos - i0 as f64) as f32;
                let v = match (uplink.get(i0), uplink.get(i1)) {
                    (Some(&x), Some(&y)) => x + (y - x) * frac,
                    (Some(&x), None) => x,
                    (None, _) => 0.0,
                };
                if let Some(dst) = self.in_buf.get_mut(a * p + k) {
                    *dst = v;
                }
                if let Some(dst) = self.in_buf.get_mut(b * p + k) {
                    *dst = v;
                }
            }
            let consumed = clock.pos.floor() as usize;
            for _ in 0..consumed {
                uplink.pop_front();
            }
            clock.pos -= consumed as f64;
            // First-order PLL: hold the queue near the target by slewing the
            // read ratio; the slew is tiny (±1%) so speech pitch is unaffected.
            let q = uplink.len() as f64;
            clock.fill += PHONE_FILL_SMOOTH * (q - clock.fill);
            let base = self.phone_rate as f64 / self.cfg.fmt.sample_rate as f64;
            // Too full -> read faster; too empty -> read slower.
            let adj = 1.0 + PHONE_PLL_GAIN * (clock.fill - PHONE_TARGET_FILL);
            clock.step =
                (base * adj).clamp(base * (1.0 - PHONE_PLL_SLEW), base * (1.0 + PHONE_PLL_SLEW));
        }
        // 3. Mix.
        {
            let n_in = self.in_buf.len() / p;
            let mut ins: [&[f32]; MAX_CHANNELS] = [&[]; MAX_CHANNELS];
            for (k, c) in self.in_buf.chunks(p).enumerate() {
                ins[k] = c;
            }
            let mut outs: [&mut [f32]; MAX_CHANNELS] =
                core::array::from_fn(|_| <&mut [f32]>::default());
            let n_out = self.out_buf.len() / p;
            for (k, c) in self.out_buf.chunks_mut(p).enumerate() {
                outs[k] = c;
            }
            self.audio.process(&ins[..n_in], &mut outs[..n_out], p);
        }
        // The phone beltpacks' listen mixes (design 29): each phone's two
        // matrix outputs are summed to mono, downsampled to `phone_rate`, and
        // sent back to the browser. Capped so a slow browser drops the oldest,
        // never the newest. `f` is the whole rate divisor (2 at 24 kHz): the
        // listen mix is downsampled by a fixed factor, while the mic uplink
        // above is clock-followed.
        let f = self.cfg.fmt.sample_rate as usize / self.phone_rate;
        for (slot, phone) in self.phones.iter().enumerate() {
            let mut downlink = phone.downlink.lock();
            const MAX_PHONE_DOWNLINK: usize = 96_000; // ~2 s mono at 48 kHz
            if downlink.len() > MAX_PHONE_DOWNLINK {
                let excess = downlink.len() - MAX_PHONE_DOWNLINK;
                downlink.drain(..excess);
            }
            let a = self.phone_base_out + 2 * slot;
            let b = a + 1;
            // Average `f` frames of L+R down to one mono sample (a crude
            // low-pass that keeps the downsampling alias-free enough for speech).
            for k in 0..(p / f) {
                let mut acc = 0.0f32;
                for g in 0..f {
                    let frame = k * f + g;
                    acc += self.out_buf.get(a * p + frame).copied().unwrap_or(0.0);
                    acc += self.out_buf.get(b * p + frame).copied().unwrap_or(0.0);
                }
                downlink.push_back(acc / (2 * f) as f32);
            }
        }
        // Effects: this period's sends, returned next period.
        for fx in &mut self.fx {
            for (c, &ch) in fx.send.iter().enumerate() {
                if let Some(src) = self.out_buf.get(ch * p..ch * p + p) {
                    fx.out[c].copy_from_slice(src);
                }
            }
            let [a, b] = &mut fx.out;
            match &mut fx.unit {
                Unit::Reverb(r) => r.process(&mut [a, b]),
                Unit::Delay(d) => d.process(&mut [a, b]),
            }
        }
        // 4. Output flows: output-bus processing, then meter and send.
        let deadline = self.deadline_ns(n);
        for (jo, o) in self.outputs.iter_mut().enumerate() {
            let [l, r] = o.flow.channels.map(|c| c as usize);
            // Output-bus EQ and limiter (design 16), in place.
            if let Some((a, b)) = two_mut(&mut self.out_buf, l, r, p) {
                o.proc.process(&mut [a, b]);
                // Spectrum tap (design 18): the post-EQ/limiter bus, copied
                // into the lock-free ring — no FFT on the packet path.
                if matches!(self.spectrum_src, Some(SpectrumSource::Output(x)) if x == jo) {
                    self.spectrum.write_stereo(a, b);
                    self.correlation.process(a, b);
                }
            }
            if let (Some(a), Some(b)) = (
                self.out_buf.get(l * p..l * p + p),
                self.out_buf.get(r * p..r * p + p),
            ) {
                o.loudness.process(&[a, b]);
                o.vu[0].process(a);
                o.vu[1].process(b);
            }
            for k in 0..p {
                for (c, ch) in [l, r].into_iter().enumerate() {
                    let v = self.out_buf.get(ch * p + k).copied().unwrap_or(0.0);
                    self.pcm[k * CHANNELS + c] =
                        (f64::from(v).clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
                }
            }
            if let Ok(b) = o
                .pack
                .build(n, &self.pcm, now_ns, deadline, 0, 0, &mut self.packet)
            {
                send(&self.packet[..b.len]);
                self.stats.packets_sent += 1;
            }
            o.pack.advance(n, &self.pcm);
        }
        // AES67 outputs: the period as packets of the stream's packet time.
        for o in &mut self.aes_out {
            let f = o.stream.frames();
            let ch = o.stream.channels;
            for start in (0..p).step_by(f) {
                for k in 0..f {
                    for (c, &out) in o.outputs.iter().enumerate() {
                        let v = self
                            .out_buf
                            .get(out * p + start + k)
                            .copied()
                            .unwrap_or(0.0);
                        o.pcm[k * ch + c] =
                            (f64::from(v).clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
                    }
                }
                let h = RtpHeader {
                    marker: false,
                    payload_type: o.stream.payload_type,
                    sequence: o.seq,
                    timestamp: ((n + start as i64) as u32).wrapping_add(o.stream.media_offset),
                    ssrc: o.ssrc,
                };
                if let Ok(hl) = rtp::write_header(&mut self.packet, &h, None) {
                    let len = hl + rob_core::aes67::encode_l24(&o.pcm, &mut self.packet[hl..]);
                    send(&self.packet[..len]);
                }
                o.seq = o.seq.wrapping_add(1);
            }
        }
        self.next += p as i64;
        self.stats.periods += 1;
        // 5. Late inputs that arrived since: corrections.
        self.corrections(now_ns, send);
    }

    fn deadline_ns(&self, first: i64) -> u64 {
        // `ns_at` widens to u128: a long-uptime sender's sample index
        // (~4.3 days at 48 kHz) overflows `sample * 1e9` in u64.
        timestamp::ns_at(first.max(0) as u64, self.cfg.fmt.sample_rate) + self.cfg.latency_ns
    }

    fn corrections(&mut self, now_ns: u64, send: &mut impl FnMut(&[u8])) {
        let base = self.base.unwrap_or(0);
        for i in 0..self.inputs.len() {
            loop {
                // Take one repair (interleaved stereo) into scratch.
                let mut got: Option<(i64, usize)> = None;
                let repair = &mut self.repair;
                let mut stereo = [0.0f32; MAX_CORR_FRAMES * CHANNELS];
                let took = self.inputs[i].jitter.take_repair(|first, d| {
                    let len = d.len().min(stereo.len());
                    stereo[..len].copy_from_slice(&d[..len]);
                    got = Some((first, len / CHANNELS));
                });
                if !took {
                    break;
                }
                let Some((first, frames)) = got else { break };
                self.stats.repairs += 1;
                // Through the input's strip (its linear part): exact when
                // flat. An active EQ rings on after the difference ends: carry
                // that ringing too, as far as it has been mixed.
                let frames = {
                    let strip = &self.inputs[i].strip;
                    let total = if strip.rings() {
                        let mixed = (self.next - (first + frames as i64)).max(0) as usize;
                        (frames + mixed.min(EQ_TAIL)).min(MAX_CORR_FRAMES)
                    } else {
                        frames
                    };
                    let mut lin = strip.linear_part();
                    let mut l = [0.0f32; MAX_CORR_FRAMES];
                    let mut r = [0.0f32; MAX_CORR_FRAMES];
                    for k in 0..frames {
                        l[k] = stereo[k * CHANNELS];
                        r[k] = stereo[k * CHANNELS + 1];
                    }
                    lin.process(&mut [&mut l[..total], &mut r[..total]]);
                    for k in 0..total {
                        stereo[k * CHANNELS] = l[k];
                        stereo[k * CHANNELS + 1] = r[k];
                    }
                    total
                };
                self.corr_hit.fill(false);
                let span = MAX_CORR_FRAMES * CHANNELS;
                for (c, &ch) in self.inputs[i].flow.channels.iter().enumerate() {
                    for k in 0..frames {
                        repair[k] = stereo[k * CHANNELS + c];
                    }
                    let (acc, hit, map) = (&mut self.corr_acc, &mut self.corr_hit, &self.out_map);
                    let r = self.audio.correct(
                        ch,
                        (first - base) as u64,
                        &repair[..frames],
                        |out, _at, d| {
                            if let Some(&Some((flow, oc))) = map.get(out as usize) {
                                if !hit[flow] {
                                    acc[flow * span..flow * span + frames * CHANNELS].fill(0.0);
                                    hit[flow] = true;
                                }
                                for (k, v) in d.iter().enumerate() {
                                    acc[flow * span + k * CHANNELS + oc] += v;
                                }
                            }
                        },
                    );
                    if r == Correction::Unknown {
                        self.stats.repairs_unknown += 1;
                    }
                }
                for f in 0..self.outputs.len() {
                    if self.corr_hit[f] {
                        self.send_correction(f, first, frames, false, now_ns, send);
                    }
                }
            }
        }
    }

    /// Sends a correction, split into as many packets as it needs (each one
    /// stands alone at the receiver: its own first sample and length).
    fn send_correction(
        &mut self,
        f: usize,
        first: i64,
        frames: usize,
        repeat: bool,
        now_ns: u64,
        send: &mut impl FnMut(&[u8]),
    ) {
        let per = correction::max_frames(CHANNELS as u8).max(1);
        let mut at = 0;
        while at < frames {
            let n = per.min(frames - at);
            self.send_piece(f, first, at, n, repeat, now_ns, send);
            at += n;
        }
        if !repeat && self.cfg.repeat_corrections {
            let span = MAX_CORR_FRAMES * CHANNELS;
            let src = &self.corr_acc[f * span..f * span + frames * CHANNELS];
            let o = &mut self.outputs[f];
            o.repeat_data[..frames * CHANNELS].copy_from_slice(src);
            o.repeat = Some((first, frames));
        }
    }

    /// One packet: frames `at .. at + frames` of the correction starting at
    /// network sample `first`.
    #[allow(clippy::too_many_arguments)]
    fn send_piece(
        &mut self,
        f: usize,
        first: i64,
        at: usize,
        frames: usize,
        repeat: bool,
        now_ns: u64,
        send: &mut impl FnMut(&[u8]),
    ) {
        let span = MAX_CORR_FRAMES * CHANNELS;
        let (a, b) = (at * CHANNELS, (at + frames) * CHANNELS);
        let data: &[f32] = if repeat {
            &self.outputs[f].repeat_data[a..b]
        } else {
            &self.corr_acc[f * span + a..f * span + b]
        };
        let first = first + at as i64;
        let o = &self.outputs[f];
        let header = RtpHeader {
            marker: false,
            payload_type: CORRECTION_PT,
            sequence: o.corr_seq,
            timestamp: first as u32,
            ssrc: o.flow.correction_ssrc,
        };
        let core = RobCore {
            version: ROB_VERSION,
            mode: 0,
            path_id: 0,
            slot: 0,
            budget_us: 0,
            send_time_us: (now_ns / 1_000) as u32,
        };
        let Ok(h) = rtp::write_header(&mut self.packet, &header, Some((self.cfg.ext_id, &core)))
        else {
            return;
        };
        let ch = CorrectionHeader {
            target_ssrc: o.flow.ssrc,
            frames: frames as u16,
            channels: CHANNELS as u8,
            repeat,
        };
        let Ok(n) = correction::write_payload(&mut self.packet[h..], &ch, data) else {
            return;
        };
        send(&self.packet[..h + n]);
        self.stats.corrections_sent += 1;
        let o = &mut self.outputs[f];
        o.corr_seq = o.corr_seq.wrapping_add(1);
    }

    fn send_repeats(&mut self, now_ns: u64, send: &mut impl FnMut(&[u8])) {
        for f in 0..self.outputs.len() {
            if let Some((first, frames)) = self.outputs[f].repeat.take() {
                self.send_correction(f, first, frames, true, now_ns, send);
            }
        }
    }

    /// Network sample index of the next period to be mixed.
    pub fn next_period(&self) -> i64 {
        self.next
    }
}
