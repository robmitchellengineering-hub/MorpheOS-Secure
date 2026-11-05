//! A receiving node: network thread → lock-free queue → audio thread.
//!
//! * [`ReceiverNet`] (network thread): decodes datagrams into period copies and
//!   queues them.
//! * [`ReceiverAudio`] (audio thread): stores copies in the jitter buffer, renders
//!   network-time audio into the resampler, and reads it out at the DAC's own
//!   rate, locked so the sample captured at network time `T` leaves the DAC at
//!   `T + latency`.
//!
//! **Latency budget.** The audio thread renders one DAC block plus the kernel's
//! look-ahead before that audio plays, so a period must have *arrived* by its play
//! time minus (block + kernel). The latency target must cover network delay,
//! jitter, that render-ahead, and the sender's own block and kernel.

use crate::prelude::*;

use crate::asrc::Asrc;
use crate::depacketize::{DepacketStats, Depacketizer};
use crate::flow::{Chunk, FlowFormat, Layer, CHANNELS};
use crate::interp::Quality;
use crate::jitter::{JitterBuffer, PlayoutStats};
use crate::latency::{slew, AutoConfig, AutoLatency};
use crate::paths::Histogram;
use crate::pll::TrackerConfig;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};
use rob_core::correction::{self, CORRECTION_PT};
use rob_core::{rtp, timestamp};

#[derive(Debug, Clone, Copy)]
pub struct ReceiverConfig {
    pub fmt: FlowFormat,
    /// Capture to DAC output, on network time.
    pub latency_ns: u64,
    pub quality: Quality,
    /// Jitter buffer size in periods.
    pub jitter_periods: usize,
    /// Accept only this flow (`None` = the first one heard).
    pub ssrc: Option<u32>,
    pub ext_id: u8,
    /// Phase-locked loop settings for the DAC-side resampler.
    pub tracker: TrackerConfig,
    /// Lower the latency while there is room, raise it when a period is
    /// late (`latency_ns` is then the starting point). `None`: fixed.
    pub auto_latency: Option<AutoConfig>,
    /// A backup engine's flow carrying the same mix (on the same network
    /// time): played instead, with a crossfade, whenever the main flow is
    /// missing audio that the backup has. Needs `ssrc` set for the main one.
    pub backup_ssrc: Option<u32>,
}

impl ReceiverConfig {
    pub fn new(fmt: FlowFormat, latency_ns: u64) -> ReceiverConfig {
        ReceiverConfig {
            fmt,
            latency_ns,
            quality: Quality::Standard,
            jitter_periods: 64,
            ssrc: None,
            ext_id: rob_core::rtp::DEFAULT_ROB_EXT_ID,
            tracker: TrackerConfig {
                output_rate: f64::from(fmt.sample_rate),
                ..TrackerConfig::default()
            },
            auto_latency: None,
            backup_ssrc: None,
        }
    }
}

/// A gap this long with no audio packet arriving at all is an outage (a roam
/// between access points, a radio dropping off), not lateness: lateness from
/// a too-low latency never stops arrivals. Automatic latency does not learn
/// from what follows an outage.
pub const OUTAGE_GAP_NS: u64 = 20_000_000;

/// Longest correction a receiver takes (frames).
pub const MAX_CORRECTION_FRAMES: usize = 512;

/// A late-audio correction on its way to the audio thread (PRIVATE: patent
/// candidate, see `docs/ip/`): add `data` (network time, interleaved) to the
/// audio from `first` on, wherever it has not been played yet.
#[derive(Clone, Copy)]
pub struct CorrectionPacket {
    pub first: i64,
    pub frames: usize,
    /// For the backup flow (else the main one).
    pub backup: bool,
    pub data: [f32; MAX_CORRECTION_FRAMES * CHANNELS],
}

/// What the receiver did with corrections.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CorrectionStats {
    /// Network side.
    pub received: u64,
    pub repeats: u64,
    pub malformed: u64,
    pub foreign: u64,
    /// Audio side: applied in full, applied after a fade (part already
    /// played), too late (all played), and frames that had nothing to correct
    /// (packet not arrived, or concealed here).
    pub exact: u64,
    pub partial: u64,
    pub too_late: u64,
    pub unheld_frames: u64,
}

/// Network-thread half.
pub struct ReceiverNet {
    depacketizer: Depacketizer,
    queue: rtrb::Producer<Chunk>,
    corrections: rtrb::Producer<CorrectionPacket>,
    /// The backup flow's depacketizer and queue, if configured.
    backup: Option<(Depacketizer, rtrb::Producer<Chunk>)>,
    /// Recently seen corrections (first sample, frames, backup): repeats are
    /// dropped.
    seen: [(i64, usize, bool); 16],
    seen_head: usize,
    fmt_rate: u32,
    ext_id: u8,
    pub correction_stats: CorrectionStats,
    /// Copies dropped because the audio thread fell behind.
    pub queue_full: u64,
    /// Arrival of the last new audio packet (network time).
    last_audio_ns: Option<u64>,
    /// Outages seen (shared with the audio thread, lock-free).
    outages: Arc<AtomicU64>,
}

impl ReceiverNet {
    /// Handles one datagram; `now_net_ns` is network time on arrival.
    pub fn on_datagram(&mut self, datagram: &[u8], now_net_ns: u64) {
        if let Ok(p) = rtp::parse(datagram, self.ext_id) {
            if p.header.payload_type == CORRECTION_PT {
                self.on_correction(&p, now_net_ns);
                return;
            }
            if let Some((d, q)) = self.backup.as_mut() {
                if d.ssrc() == Some(p.header.ssrc) {
                    let full = &mut self.queue_full;
                    d.accept(datagram, now_net_ns, |c| {
                        if q.push(*c).is_err() {
                            *full += 1;
                        }
                    });
                    return;
                }
            }
        }
        let queue = &mut self.queue;
        let full = &mut self.queue_full;
        let before = self.depacketizer.stats.packets;
        self.depacketizer.accept(datagram, now_net_ns, |c| {
            if queue.push(*c).is_err() {
                *full += 1;
            }
        });
        if self.depacketizer.stats.packets > before {
            if self
                .last_audio_ns
                .is_some_and(|t| now_net_ns.saturating_sub(t) > OUTAGE_GAP_NS)
            {
                self.outages.fetch_add(1, Ordering::Relaxed);
            }
            self.last_audio_ns = Some(now_net_ns);
        }
    }

    /// Outages seen: gaps of more than [`OUTAGE_GAP_NS`] with no audio packet.
    pub fn outages(&self) -> u64 {
        self.outages.load(Ordering::Relaxed)
    }

    pub fn stats(&self) -> DepacketStats {
        self.depacketizer.stats
    }

    /// The flow accepted (fixed by config, or the first one heard).
    pub fn ssrc(&self) -> Option<u32> {
        self.depacketizer.ssrc()
    }

    fn on_correction(&mut self, p: &rtp::Packet<'_>, now_net_ns: u64) {
        let st = &mut self.correction_stats;
        let Ok((h, body)) = correction::parse_payload(p.payload) else {
            st.malformed += 1;
            return;
        };
        let backup = self
            .backup
            .as_ref()
            .is_some_and(|(d, _)| d.ssrc() == Some(h.target_ssrc));
        if !backup && self.depacketizer.ssrc().is_some_and(|s| s != h.target_ssrc) {
            st.foreign += 1;
            return;
        }
        let frames = usize::from(h.frames);
        if usize::from(h.channels) != CHANNELS || frames > MAX_CORRECTION_FRAMES {
            st.malformed += 1;
            return;
        }
        let now = timestamp::samples_at(now_net_ns, self.fmt_rate);
        let first = timestamp::unwrap(p.header.timestamp, now) as i64;
        if self.seen.contains(&(first, frames, backup)) {
            st.repeats += 1;
            return;
        }
        let mut c = CorrectionPacket {
            first,
            frames,
            backup,
            data: [0.0; MAX_CORRECTION_FRAMES * CHANNELS],
        };
        if correction::read_delta(body, &mut c.data).is_err() {
            st.malformed += 1;
            return;
        }
        self.seen[self.seen_head] = (first, frames, backup);
        self.seen_head = (self.seen_head + 1) % self.seen.len();
        st.received += 1;
        if self.corrections.push(c).is_err() {
            self.queue_full += 1;
        }
    }
}

/// What one audio callback did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcessReport {
    /// Output frames that had no input (silence).
    pub starved: usize,
    /// The resampler's position jumped.
    pub resynced: bool,
    /// The buffers were restarted (first call, or a large time jump).
    pub restarted: bool,
}

/// Audio-thread half.
pub struct ReceiverAudio {
    cfg: ReceiverConfig,
    queue: rtrb::Consumer<Chunk>,
    jitter: JitterBuffer,
    asrc: Asrc,
    started: bool,
    corrections: rtrb::Consumer<CorrectionPacket>,
    pub restarts: u64,
    pub correction_stats: CorrectionStats,
    /// How long before it was needed each period's full-quality copy reached
    /// the jitter buffer: the latency could be this much lower and the
    /// period would still have been on time (conservative by up to one
    /// output block). Late periods count as zero. Clear it to start a new
    /// interval.
    pub headroom: Histogram,
    /// Latency in effect (ns; moves smoothly towards `latency_goal`).
    latency_now: f64,
    latency_goal: f64,
    auto: Option<AutoLatency>,
    /// Output frames since the last once-a-second tick, and the smallest
    /// headroom and the late count seen since.
    tick_frames: usize,
    tick_min: Option<i64>,
    last_late: u64,
    /// Outages reported by the network thread, and how many were seen.
    outages: Arc<AtomicU64>,
    seen_outages: u64,
    backup: Option<Backup>,
}

/// The backup flow on the audio side.
struct Backup {
    queue: rtrb::Consumer<Chunk>,
    jitter: JitterBuffer,
    /// Both flows' frames for one render, before mixing.
    a: Vec<[f32; CHANNELS]>,
    b: Vec<[f32; CHANNELS]>,
    /// 0: all main, 1: all backup; moves towards `on_backup`.
    mix: f32,
    on_backup: bool,
    /// Main flow complete (looking ahead) for this many frames in a row.
    main_ok_frames: usize,
    switches: u64,
}

/// Crossfade between main and backup (frames): it starts before the gap.
const SWITCH_FADE: usize = 96;
/// Back to the main flow after it has been complete this long (frames).
const RETURN_AFTER: usize = 96_000;

/// Where a receiver with a backup flow is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupStatus {
    pub on_backup: bool,
    pub switches: u64,
    pub backup_playout: PlayoutStats,
}

/// Builds the two halves of a receiver. Allocates; call before audio starts.
pub fn receiver(cfg: ReceiverConfig) -> (ReceiverNet, ReceiverAudio) {
    let (tx, rx) = rtrb::RingBuffer::new(cfg.jitter_periods * 4);
    let (ctx, crx) = rtrb::RingBuffer::new(16);
    let ring = 1 << 14; // 341 ms at 48 kHz: latency + block + kernel, with room
    let outages = Arc::new(AtomicU64::new(0));
    let (backup_net, backup_audio) = match cfg.backup_ssrc {
        Some(s) => {
            let (btx, brx) = rtrb::RingBuffer::new(cfg.jitter_periods * 4);
            (
                Some((Depacketizer::new(cfg.fmt, Some(s), cfg.ext_id), btx)),
                Some(Backup {
                    queue: brx,
                    jitter: JitterBuffer::new(cfg.fmt, cfg.jitter_periods),
                    a: vec![[0.0; CHANNELS]; 1 << 14],
                    b: vec![[0.0; CHANNELS]; 1 << 14],
                    mix: 0.0,
                    on_backup: false,
                    main_ok_frames: 0,
                    switches: 0,
                }),
            )
        }
        None => (None, None),
    };
    (
        ReceiverNet {
            depacketizer: Depacketizer::new(cfg.fmt, cfg.ssrc, cfg.ext_id),
            queue: tx,
            corrections: ctx,
            backup: backup_net,
            seen: [(i64::MIN, 0, false); 16],
            seen_head: 0,
            fmt_rate: cfg.fmt.sample_rate,
            ext_id: cfg.ext_id,
            correction_stats: CorrectionStats::default(),
            queue_full: 0,
            last_audio_ns: None,
            outages: Arc::clone(&outages),
        },
        ReceiverAudio {
            cfg,
            queue: rx,
            jitter: JitterBuffer::new(cfg.fmt, cfg.jitter_periods),
            asrc: Asrc::new(CHANNELS, cfg.quality, cfg.tracker, ring),
            started: false,
            corrections: crx,
            restarts: 0,
            correction_stats: CorrectionStats::default(),
            headroom: Histogram::default(),
            latency_now: cfg.latency_ns as f64,
            latency_goal: cfg.latency_ns as f64,
            auto: cfg.auto_latency.map(AutoLatency::new),
            tick_frames: 0,
            tick_min: None,
            last_late: 0,
            outages,
            seen_outages: 0,
            backup: backup_audio,
        },
    )
}

impl ReceiverAudio {
    /// Fills `out` (interleaved stereo) for the DAC. `dac_net_ns` is the network
    /// time at which `out`'s first frame will leave the DAC; `None` (no sync)
    /// free-runs. No allocation, no locks.
    pub fn process(&mut self, out: &mut [f32], dac_net_ns: Option<u64>) -> ProcessReport {
        while let Ok(c) = self.queue.pop() {
            if c.layer == Layer::Full24 {
                if let (true, Some(next)) = (self.started, self.jitter.next_sample()) {
                    let ahead = (i128::from(c.first_sample - next) * 1_000_000_000
                        / i128::from(self.cfg.fmt.sample_rate))
                        as i64;
                    self.headroom.add(ahead);
                    self.tick_min = Some(self.tick_min.map_or(ahead, |m| m.min(ahead)));
                }
            }
            self.jitter.insert(&c);
        }
        if let Some(b) = self.backup.as_mut() {
            while let Ok(c) = b.queue.pop() {
                b.jitter.insert(&c);
            }
        }
        while let Ok(c) = self.corrections.pop() {
            self.apply_correction(&c);
        }
        let mut rep = ProcessReport::default();
        let frames = out.len() / CHANNELS;
        // Network sample to read, as an exact whole number plus a fraction:
        // as one f64 it would lose the fraction at network-time magnitudes.
        let latency = self.latency_now.round() as i128;
        let target = dac_net_ns.map(|t| {
            let x = (i128::from(t) - latency) * i128::from(self.cfg.fmt.sample_rate);
            (
                x.div_euclid(1_000_000_000) as i64,
                x.rem_euclid(1_000_000_000) as f64 / 1e9,
            )
        });

        let history = (self.asrc.kernel_latency() + 1) as i64;
        if let Some((whole, _)) = target {
            // Restart on the first call, or if the target has left the window
            // the buffers can serve (a clock step).
            let needs_restart = !self.started
                || self
                    .jitter
                    .next_sample()
                    .is_some_and(|n| (whole - n).abs() > (1 << 13));
            if needs_restart {
                let first = whole - history - 64;
                self.asrc.reset_input(first);
                self.jitter.start_at(first);
                if let Some(b) = self.backup.as_mut() {
                    b.jitter.start_at(first);
                }
                self.started = true;
                self.restarts += 1;
                rep.restarted = true;
            }
        }
        if !self.started {
            out.fill(0.0);
            return rep;
        }
        let mut needed = self.asrc.input_needed(frames);
        if let Some((whole, frac)) = target {
            let worst = whole
                + (frac + frames as f64 * 1.003).ceil() as i64
                + self.asrc.kernel_latency() as i64
                + 1;
            needed = needed.max(worst);
        }
        // Where the latency will be at the end of this block: tell the
        // resampler the planned ramp so it follows it exactly.
        let next_latency = self.planned_latency(frames);
        let ff = -(next_latency - self.latency_now) * f64::from(self.cfg.fmt.sample_rate)
            / 1e9
            / frames.max(1) as f64;
        self.asrc.set_feed_forward(ff);
        match self.backup.as_mut() {
            None => {
                let asrc = &mut self.asrc;
                self.jitter.render_until(needed, |f| asrc.push_frame(f));
            }
            Some(b) => Self::render_with_backup(&mut self.jitter, b, &mut self.asrc, needed),
        }
        let origin = self.asrc.origin();
        let rel = target.map(|(whole, frac)| (whole - origin) as f64 + frac);
        let r = self.asrc.render(out, rel);
        rep.starved = r.starved;
        rep.resynced = r.resynced;
        self.advance_latency(frames);
        rep
    }

    /// Renders the main and backup flows over the same frames and mixes them,
    /// switching (with a crossfade that finishes before the gap) when the
    /// main flow is missing audio just ahead that the backup has, and back
    /// once the main flow has been complete for [`RETURN_AFTER`] frames.
    fn render_with_backup(main: &mut JitterBuffer, b: &mut Backup, asrc: &mut Asrc, needed: i64) {
        if let Some(next) = main.next_sample() {
            let ahead = needed + SWITCH_FADE as i64;
            let main_ok = main.holds_span(next, ahead);
            let backup_ok = b.jitter.holds_span(next, ahead);
            b.main_ok_frames = if main_ok {
                b.main_ok_frames
                    .saturating_add((needed - next).max(0) as usize)
            } else {
                0
            };
            let want = if b.on_backup {
                !(main_ok && (b.main_ok_frames >= RETURN_AFTER || !backup_ok))
            } else {
                !main_ok && backup_ok
            };
            if want != b.on_backup {
                b.on_backup = want;
                b.switches += 1;
            }
        }
        let (mut ka, mut kb) = (0usize, 0usize);
        let (av, bv) = (&mut b.a, &mut b.b);
        main.render_until(needed, |f| {
            if let Some(s) = av.get_mut(ka) {
                *s = *f;
            }
            ka += 1;
        });
        b.jitter.render_until(needed, |f| {
            if let Some(s) = bv.get_mut(kb) {
                *s = *f;
            }
            kb += 1;
        });
        let n = ka.min(kb).min(b.a.len());
        let target = if b.on_backup { 1.0 } else { 0.0 };
        let step = 1.0 / SWITCH_FADE as f32;
        for k in 0..n {
            b.mix = if b.mix < target {
                (b.mix + step).min(target)
            } else {
                (b.mix - step).max(target)
            };
            // Equal-gain crossfade: the two carry the same mix, in phase.
            let (x, y) = (b.a[k], b.b[k]);
            let w = b.mix;
            asrc.push_frame(&[x[0] * (1.0 - w) + y[0] * w, x[1] * (1.0 - w) + y[1] * w]);
        }
    }

    /// The backup flow's state, if one is configured.
    pub fn backup_status(&self) -> Option<BackupStatus> {
        self.backup.as_ref().map(|b| BackupStatus {
            on_backup: b.on_backup,
            switches: b.switches,
            backup_playout: b.jitter.stats,
        })
    }

    /// Slews the latency towards its goal for `frames` of playback, and once
    /// a second lets automatic latency choose a new goal.
    fn advance_latency(&mut self, frames: usize) {
        let rate = self.cfg.fmt.sample_rate as usize;
        self.latency_now = self.planned_latency(frames);
        self.tick_frames += frames;
        if self.tick_frames < rate {
            return;
        }
        self.tick_frames -= rate;
        let late_now = self.jitter.stats.late;
        let late = late_now > self.last_late;
        self.last_late = late_now;
        let min = self.tick_min.take();
        let outages = self.outages.load(Ordering::Relaxed);
        let outage = outages > self.seen_outages;
        self.seen_outages = outages;
        if let Some(a) = self.auto.as_mut() {
            if let Some(g) = a.tick(self.latency_now, self.latency_goal, min, late, outage) {
                self.latency_goal = g;
            }
        }
    }

    /// The latency after `frames` more of playback, slewing towards the goal.
    fn planned_latency(&self, frames: usize) -> f64 {
        if self.latency_now == self.latency_goal {
            return self.latency_now;
        }
        let c = self
            .auto
            .as_ref()
            .map_or_else(AutoConfig::default, |a| a.cfg);
        let dt_ns = frames as f64 * 1e9 / f64::from(self.cfg.fmt.sample_rate);
        slew(self.latency_now, self.latency_goal, &c, dt_ns)
    }

    /// The latency in effect (ns): the configured one, or where automatic
    /// latency has moved it.
    pub fn latency_ns(&self) -> f64 {
        self.latency_now
    }

    /// Where the latency is heading (ns).
    pub fn latency_goal_ns(&self) -> f64 {
        self.latency_goal
    }

    /// Moves the latency to `ns`, smoothly (never a jump).
    pub fn set_latency(&mut self, ns: u64) {
        self.latency_goal = ns as f64;
    }

    /// Automatic latency's counters (lowered, raised), if on.
    pub fn auto_latency_changes(&self) -> Option<(u64, u64)> {
        self.auto.as_ref().map(|a| (a.lowered, a.raised))
    }

    /// Automatic latency needed more than its ceiling in the last minute:
    /// this link needs a second path (or a better spot), not more latency.
    pub fn auto_over_ceiling(&self) -> bool {
        self.auto.as_ref().is_some_and(|a| a.over_ceiling())
    }

    /// Adds a correction to the audio not played yet: in the jitter buffer if
    /// not rendered, in the resampler's input if no output has read it. If
    /// part of it has already been read, the rest fades in, so it never
    /// clicks. No allocation.
    fn apply_correction(&mut self, c: &CorrectionPacket) {
        // The resampler's input holds whichever flow is playing: a
        // correction for the other one only goes into its own buffer.
        let playing = self.backup.as_ref().is_some_and(|b| b.on_backup) == c.backup;
        let first_unread = self.asrc.first_unread();
        let late = (0..c.frames)
            .filter(|&k| c.first + (k as i64) < first_unread)
            .count();
        let st = &mut self.correction_stats;
        if late == c.frames {
            st.too_late += 1;
            return;
        }
        let mut faded = 0usize;
        for k in late..c.frames {
            let n = c.first + k as i64;
            let w = if late > 0 {
                faded += 1;
                (faded as f32 / crate::jitter::FADE as f32).min(1.0)
            } else {
                1.0
            };
            let d = [c.data[k * CHANNELS], c.data[k * CHANNELS + 1]];
            let jitter = match (c.backup, self.backup.as_mut()) {
                (true, Some(b)) => &mut b.jitter,
                _ => &mut self.jitter,
            };
            if !(jitter.add_correction(n, &d, w) || (playing && self.asrc.add_input(n, &d, w))) {
                st.unheld_frames += 1;
            }
        }
        if late > 0 {
            st.partial += 1;
        } else {
            st.exact += 1;
        }
    }

    pub fn playout(&self) -> PlayoutStats {
        self.jitter.stats
    }

    pub fn asrc(&self) -> &Asrc {
        &self.asrc
    }
}
