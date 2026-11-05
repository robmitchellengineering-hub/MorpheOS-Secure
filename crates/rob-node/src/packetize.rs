//! Building ROB packets from periods of network-time audio (sender side).
//!
//! Keeps the previous periods so each packet can repeat them at lower bit depth
//! (spec D3/D4: one packet per period, redundancy inside it). Builds into a
//! caller-provided buffer: no allocation once constructed.

use crate::flow::{FlowFormat, CHANNELS, MAX_PERIOD_SAMPLES};
use rob_core::payload::{BlockType, PayloadWriter};
use rob_core::rtp::{self, RobCore, RtpHeader, MODE_REDUNDANCY, ROB_VERSION};
use rob_core::{bc10, pcm, Error};

/// Largest packet this builds (stereo, 4 ms, default layout) fits easily.
pub const MAX_PACKET: usize = 1_500;

pub struct Packetizer {
    fmt: FlowFormat,
    ssrc: u32,
    payload_type: u8,
    ext_id: u8,
    seq: u16,
    /// The last periods, newest first: `history[0]` is the previous period.
    history: [[i32; MAX_PERIOD_SAMPLES]; 2],
    /// First sample index of each history entry (None until filled).
    history_first: [Option<i64>; 2],
}

/// What the network thread needs to send one packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Built {
    pub len: usize,
    pub seq: u16,
}

impl Packetizer {
    pub fn new(fmt: FlowFormat, ssrc: u32, payload_type: u8, ext_id: u8) -> Packetizer {
        Packetizer {
            fmt,
            ssrc,
            payload_type,
            ext_id,
            seq: 0,
            history: [[0; MAX_PERIOD_SAMPLES]; 2],
            history_first: [None, None],
        }
    }

    /// Builds the packet for one period into `out`.
    ///
    /// * `first_sample`: absolute network sample index of the period's first frame
    ///   (its RTP timestamp, modulo 2^32).
    /// * `samples`: the period, interleaved 24-bit.
    /// * `now_ns` / `deadline_ns`: network time now and when the period must be
    ///   playing at the receiver, for `budget_us`.
    /// * `path_id`: which path this copy goes out on (same packet otherwise).
    ///
    /// Call [`Packetizer::advance`] once per period after building every path's copy.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        &self,
        first_sample: i64,
        samples: &[i32],
        now_ns: u64,
        deadline_ns: u64,
        path_id: u8,
        slot: u16,
        out: &mut [u8],
    ) -> Result<Built, Error> {
        let n = self.fmt.period_samples();
        if samples.len() != n {
            return Err(Error::FieldRange);
        }
        let blocks = self.available_blocks(first_sample);
        let header = RtpHeader {
            marker: self.history_first[0].is_none(),
            payload_type: self.payload_type,
            sequence: self.seq,
            timestamp: first_sample as u32,
            ssrc: self.ssrc,
        };
        let core = RobCore {
            version: ROB_VERSION,
            mode: if blocks > 1 { MODE_REDUNDANCY } else { 0 },
            path_id,
            slot,
            budget_us: (deadline_ns.saturating_sub(now_ns) / 1_000).min(u64::from(u16::MAX)) as u16,
            send_time_us: (now_ns / 1_000) as u32,
        };
        let h = rtp::write_header(out, &header, Some((self.ext_id, &core)))?;
        let mut w = PayloadWriter::new(&mut out[h..], blocks)?;
        for &(offset, kind) in &self.fmt.layout.blocks()[..blocks] {
            let src: &[i32] = if offset == 0 {
                samples
            } else {
                &self.history[usize::from(offset) - 1][..n]
            };
            match kind {
                BlockType::Pcm24 => w.push_with(kind, offset, |o| pcm::encode_pcm24(src, o))?,
                BlockType::Pcm16 => w.push_with(kind, offset, |o| pcm::encode_pcm16(src, o))?,
                BlockType::Bc10 => w.push_with(kind, offset, |o| {
                    bc10::encode(src, CHANNELS, self.fmt.bc10_block_frames(), o)
                })?,
                _ => return Err(Error::BadBlocks),
            }
        }
        Ok(Built {
            len: h + w.finish()?,
            seq: self.seq,
        })
    }

    /// How many of the layout's blocks this period can carry: earlier periods
    /// only count if they really were the immediately preceding ones.
    fn available_blocks(&self, first_sample: i64) -> usize {
        let f = self.fmt.period_frames as i64;
        let mut n = 1;
        for (k, first) in self.history_first.iter().enumerate() {
            if n > self.fmt.layout.depth() {
                break;
            }
            if *first == Some(first_sample - f * (k as i64 + 1)) {
                n += 1;
            } else {
                break;
            }
        }
        n
    }

    /// Moves to the next period: remembers this one for redundancy and steps the
    /// sequence number.
    pub fn advance(&mut self, first_sample: i64, samples: &[i32]) {
        let n = self.fmt.period_samples().min(samples.len());
        self.history.swap(0, 1);
        self.history[0][..n].copy_from_slice(&samples[..n]);
        self.history_first.swap(0, 1);
        self.history_first[0] = Some(first_sample);
        self.seq = self.seq.wrapping_add(1);
    }
}
