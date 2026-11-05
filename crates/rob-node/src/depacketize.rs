//! Turning received ROB packets into decoded period copies (receiver, network
//! thread). Drops duplicates from other paths, unwraps timestamps against network
//! time, decodes every block, and counts what it saw.

use crate::flow::{Chunk, FlowFormat, Layer, CHANNELS};
use rob_core::dedupe::{Dedupe, Verdict};
use rob_core::payload::{self, BlockType};
use rob_core::{bc10, pcm, rtp, timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DepacketStats {
    pub packets: u64,
    pub duplicates: u64,
    pub too_old: u64,
    pub malformed: u64,
    /// Blocks decoded, by layer (24-bit, 16-bit, 10-bit).
    pub blocks: [u64; 3],
    /// Wrong SSRC (another flow on this socket).
    pub foreign: u64,
}

pub struct Depacketizer {
    fmt: FlowFormat,
    ssrc: Option<u32>,
    ext_id: u8,
    dedupe: Dedupe,
    pub stats: DepacketStats,
}

impl Depacketizer {
    /// `ssrc`: accept only this flow (`None` = the first one seen).
    pub fn new(fmt: FlowFormat, ssrc: Option<u32>, ext_id: u8) -> Depacketizer {
        Depacketizer {
            fmt,
            ssrc,
            ext_id,
            dedupe: Dedupe::new(),
            stats: DepacketStats::default(),
        }
    }

    /// The flow accepted (fixed by config, or the first one seen).
    pub fn ssrc(&self) -> Option<u32> {
        self.ssrc
    }

    /// Handles one datagram. `now_net_ns` is network time now, used to unwrap the
    /// 32-bit timestamp. Each decoded period copy is passed to `emit`.
    pub fn accept(&mut self, datagram: &[u8], now_net_ns: u64, mut emit: impl FnMut(&Chunk)) {
        let Ok(p) = rtp::parse(datagram, self.ext_id) else {
            self.stats.malformed += 1;
            return;
        };
        match self.ssrc {
            Some(s) if s != p.header.ssrc => {
                self.stats.foreign += 1;
                return;
            }
            None => self.ssrc = Some(p.header.ssrc),
            _ => {}
        }
        match self.dedupe.check(p.header.sequence) {
            Verdict::New => {}
            Verdict::Duplicate => {
                self.stats.duplicates += 1;
                return;
            }
            Verdict::TooOld => {
                self.stats.too_old += 1;
                return;
            }
        }
        self.stats.packets += 1;
        let Ok(blocks) = payload::parse(p.payload) else {
            self.stats.malformed += 1;
            return;
        };
        let now_samples = timestamp::samples_at(now_net_ns, self.fmt.sample_rate);
        let first_now = timestamp::unwrap(p.header.timestamp, now_samples) as i64;
        let f = self.fmt.period_frames;
        let n = self.fmt.period_samples();
        let mut chunk = Chunk::empty();
        for b in blocks.iter() {
            let layer = match BlockType::from_u8(b.kind) {
                Some(BlockType::Pcm24) => Layer::Full24,
                Some(BlockType::Pcm16) => Layer::Pcm16,
                Some(BlockType::Bc10) => Layer::Bc10,
                _ => continue, // unknown or future block types are skipped
            };
            let out = &mut chunk.samples[..n];
            let ok = match layer {
                Layer::Full24 => pcm::decode_pcm24(b.data, out).map(|k| k == n),
                Layer::Pcm16 => pcm::decode_pcm16(b.data, out).map(|k| k == n),
                Layer::Bc10 => bc10::decode(b.data, f, CHANNELS, self.fmt.bc10_block_frames(), out)
                    .map(|k| k == n),
            };
            if ok != Ok(true) {
                self.stats.malformed += 1;
                continue;
            }
            chunk.layer = layer;
            chunk.first_sample = first_now - i64::from(b.period_offset) * f as i64;
            self.stats.blocks[layer as usize] += 1;
            emit(&chunk);
        }
    }
}
