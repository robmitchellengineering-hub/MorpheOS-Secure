//! What a flow carries and how its packets are laid out (spec, "Packet format"
//! and "Stream and timing model").

use crate::prelude::*;

use rob_core::payload::BlockType;

/// Largest period this crate handles: 4 ms at 48 kHz.
pub const MAX_PERIOD_FRAMES: usize = 192;
/// Channels per flow in v0.1 (stereo; spec: 2 channels fit a Wi-Fi packet).
pub const CHANNELS: usize = 2;
/// Samples in the largest period, all channels.
pub const MAX_PERIOD_SAMPLES: usize = MAX_PERIOD_FRAMES * CHANNELS;

/// Packet layouts (spec table "Layouts").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// PCM24 now, PCM16 of the previous period, BC10 of the one before.
    /// **No longer the default** (design 24 section 4.4, decided from E15 on
    /// 2026-10-14): it is the `Layered` protection, chosen explicitly.
    WifiDefault,
    /// PCM24 now, PCM16 of the previous period.
    WifiTight,
    /// PCM24 now only (wired flows, or bit-exact with multipath).
    Single,
}

/// What a flow sends to protect against a lost packet.
///
/// **The shipped default depends on how many paths the flow has**, decided by
/// Rob on 2026-10-14 from E15 (design 24 section 4.4): see
/// [`Protection::default_for_paths`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    /// PCM24 now only: the packet is the period, and the second radio path is
    /// the protection. 609 bytes of ROB packet at 48 kHz stereo (653 on the
    /// wire with UDP/IPv4 and the SRTP tag).
    Duplicate,
    /// PCM24 now, PCM16 of the previous period, BC10 of the one before: the
    /// old default, which the E2 tables call "ROB layered (24+16+10)".
    /// 1,245 bytes of ROB packet, 1,289 on the wire.
    Layered,
    /// PCM24 now, PCM16 of the previous period: the tight-latency middle.
    Tight,
}

impl Protection {
    /// The shipped default for a flow carried on `paths` paths.
    ///
    /// **Two or more paths: the plain duplicate.** Every packet already goes
    /// out on every path and the receiver keeps the first copy, so the
    /// in-packet layers buy almost nothing on real traces while doubling the
    /// airtime (1,289 -> 653 bytes of the packet on the wire; E2 replayed the
    /// filed traces).
    ///
    /// **One path: the layers stay on.** A single path has no second copy, so
    /// the layers ARE its protection, and the scoreboard says what dropping
    /// them costs there: with the duplicate as the only protection, **33 of
    /// the filed single-path measurements got worse** (a whole-link grade even
    /// fell from Programme to none on one quiet trace). That is the "keep
    /// layering where data shows isolated loss" half of the decision, applied
    /// where the loss is not covered any other way.
    ///
    /// `--protection` overrides either choice.
    pub fn default_for_paths(paths: usize) -> Protection {
        if paths > 1 {
            Protection::Duplicate
        } else {
            Protection::Layered
        }
    }

    pub fn layout(self) -> Layout {
        match self {
            Protection::Duplicate => Layout::Single,
            Protection::Layered => Layout::WifiDefault,
            Protection::Tight => Layout::WifiTight,
        }
    }

    /// The word a report, a trace and a command line use for this protection.
    pub fn name(self) -> &'static str {
        match self {
            Protection::Duplicate => "duplicate",
            Protection::Layered => "layered",
            Protection::Tight => "tight",
        }
    }

    /// The whole packet on the wire for this protection, at 48 kHz stereo with
    /// 96-frame periods: what the E2 tables and the design notes mean by
    /// "bytes/packet" (653 for the duplicate, 1,289 with the layers).
    ///
    /// PCM24 is 576 bytes, PCM16 384, BC10 244; each block carries a 4-byte
    /// header; and 73 bytes of framing sit around them - the payload count,
    /// the RTP and ROB headers (28) and UDP/IPv4 plus the SRTP tag (44), the
    /// spec's size budget. Pinned against what the packetizer really builds by
    /// `the_reported_packet_size_matches_what_the_packetizer_builds`.
    pub fn packet_bytes(self) -> usize {
        let payload = match self.layout() {
            Layout::Single => 576,
            Layout::WifiTight => 576 + 384,
            Layout::WifiDefault => 576 + 384 + 244,
        };
        // A 4-byte header per block, the 1-byte payload count, the RTP and ROB
        // headers (28) and UDP/IPv4 plus the SRTP tag (44).
        payload + 4 * self.layout().blocks().len() + 73
    }
}

impl core::str::FromStr for Protection {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "duplicate" => Ok(Protection::Duplicate),
            "layered" => Ok(Protection::Layered),
            "tight" => Ok(Protection::Tight),
            other => Err(format!(
                "unknown protection {other:?}: expected duplicate, layered or tight"
            )),
        }
    }
}

impl Layout {
    /// The blocks a packet carries: (period offset, type).
    pub fn blocks(self) -> &'static [(u8, BlockType)] {
        match self {
            Layout::WifiDefault => &[
                (0, BlockType::Pcm24),
                (1, BlockType::Pcm16),
                (2, BlockType::Bc10),
            ],
            Layout::WifiTight => &[(0, BlockType::Pcm24), (1, BlockType::Pcm16)],
            Layout::Single => &[(0, BlockType::Pcm24)],
        }
    }

    /// How many earlier periods it repeats.
    pub fn depth(self) -> usize {
        self.blocks().len() - 1
    }
}

/// A flow's format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowFormat {
    pub sample_rate: u32,
    /// Frames per packet (96 = 2 ms at 48 kHz).
    pub period_frames: usize,
    pub layout: Layout,
}

impl FlowFormat {
    /// The flow shape every ROB stream uses: 48 kHz, 2 ms periods, and the
    /// protection asked for.
    pub fn new(protection: Protection) -> FlowFormat {
        FlowFormat {
            sample_rate: 48_000,
            period_frames: 96,
            layout: protection.layout(),
        }
    }

    /// The shipped default for a flow carried on `paths` paths (design 24
    /// section 4.4, decided from E15 on 2026-10-14): the plain duplicate with
    /// two or more, the layers with one. See
    /// [`Protection::default_for_paths`] for why, and `--protection` to
    /// override.
    pub fn for_paths(paths: usize) -> FlowFormat {
        FlowFormat::new(Protection::default_for_paths(paths))
    }

    /// The layered format explicitly: PCM24 now, PCM16 of the previous period,
    /// BC10 of the one before. What a single path ships, and what
    /// `--protection layered` asks for anywhere.
    pub fn layered() -> FlowFormat {
        FlowFormat::new(Protection::Layered)
    }

    pub fn period_samples(&self) -> usize {
        self.period_frames * CHANNELS
    }

    pub fn period_ns(&self) -> u64 {
        self.period_frames as u64 * 1_000_000_000 / u64::from(self.sample_rate)
    }

    /// BC10 block length: one 1 ms block.
    pub fn bc10_block_frames(&self) -> usize {
        (self.sample_rate / 1_000) as usize
    }
}

/// Which copy of a period a chunk holds, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Full24 = 0,
    Pcm16 = 1,
    Bc10 = 2,
}

/// One decoded copy of one period, handed from the network thread to the audio
/// thread by value (it is `Copy`, so the queue never allocates or frees).
#[derive(Clone, Copy)]
pub struct Chunk {
    /// Absolute network sample index of the period's first frame.
    pub first_sample: i64,
    pub layer: Layer,
    /// Interleaved 24-bit samples (`period_samples()` of them are valid).
    pub samples: [i32; MAX_PERIOD_SAMPLES],
}

impl Chunk {
    pub fn empty() -> Chunk {
        Chunk {
            first_sample: 0,
            layer: Layer::Full24,
            samples: [0; MAX_PERIOD_SAMPLES],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped default is the plain duplicate, and the layout follows the
    /// protection (design 24 section 4.4, decided from E15 on 2026-10-14).
    ///
    /// Mutation check: point `Protection::default()` at `Layered` and this
    /// fails here and in `the_shipped_default_carries_the_period_and_nothing_else`
    /// (packets.rs), which pins the 617-byte packet.
    #[test]
    fn the_default_follows_the_path_count() {
        // Two paths are already a duplicate; one path has nothing behind it.
        assert_eq!(Protection::default_for_paths(2), Protection::Duplicate);
        assert_eq!(Protection::default_for_paths(3), Protection::Duplicate);
        assert_eq!(Protection::default_for_paths(1), Protection::Layered);
        assert_eq!(FlowFormat::for_paths(2).layout, Layout::Single);
        assert_eq!(FlowFormat::for_paths(2).layout.depth(), 0);
        assert_eq!(FlowFormat::for_paths(1).layout, Layout::WifiDefault);
        assert_eq!(FlowFormat::for_paths(1).layout.depth(), 2);
        assert_eq!(FlowFormat::layered().layout, Layout::WifiDefault);
        assert_eq!(FlowFormat::new(Protection::Tight).layout, Layout::WifiTight);
        // The words a report and a command line use.
        assert_eq!(
            "duplicate".parse::<Protection>().unwrap(),
            Protection::Duplicate
        );
        assert_eq!(
            "layered".parse::<Protection>().unwrap(),
            Protection::Layered
        );
        assert_eq!("tight".parse::<Protection>().unwrap(), Protection::Tight);
        assert!("layers".parse::<Protection>().is_err());
        assert_eq!(Protection::Duplicate.name(), "duplicate");
    }

    /// The size a report quotes is the size the packetizer builds. Mutation
    /// check: change 73 and this fails against what packets.rs measures.
    #[test]
    fn the_reported_packet_size_matches_what_the_packetizer_builds() {
        // packets.rs builds the ROB packet (609 and 1,245 bytes); UDP/IPv4
        // (28) and the SRTP tag (16) make the wire packet.
        assert_eq!(Protection::Duplicate.packet_bytes(), 609 + 44);
        assert_eq!(Protection::Layered.packet_bytes(), 1_245 + 44);
        // The E2 table's "ROB tight (24+16)" row: 1,041 bytes on the wire.
        assert_eq!(Protection::Tight.packet_bytes(), 1_041);
    }
}
