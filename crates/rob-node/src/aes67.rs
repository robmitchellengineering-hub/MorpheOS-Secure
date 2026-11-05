//! Receiving an AES67 stream (a stage box, a Dante device in AES67 mode):
//! RTP packets of L24/L16 audio, many channels interleaved, timed by PTP.
//!
//! Each packet becomes one period per selected **pair** of channels (a
//! stereo pair, or a mono channel on both sides), in the same [`Chunk`] form
//! as ROB, so the jitter buffer, channel strip, matrix and late-input
//! handling all work unchanged. The period is the stream's packet time (1 ms
//! = 48 frames by default): the jitter buffer for an AES67 pair is built with
//! [`Aes67Depacketizer::format`].
//!
//! Timing: the RTP timestamp minus the stream's media offset is PTP time in
//! samples, which is ROB network time when the node follows PTP. Duplicate
//! packets (ST 2022-7 style redundant paths) are dropped. No allocation
//! after `new`.

use crate::prelude::*;

use crate::flow::{Chunk, FlowFormat, Layer, Layout, CHANNELS, MAX_PERIOD_FRAMES};
use rob_core::aes67::{decode_channel, Stream};
use rob_core::dedupe::{Dedupe, Verdict};
use rob_core::{rtp, timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Aes67Stats {
    pub packets: u64,
    pub duplicates: u64,
    pub too_old: u64,
    /// Wrong payload type or packet size, or unparsable.
    pub malformed: u64,
    /// From another sender than the first heard.
    pub foreign: u64,
}

pub struct Aes67Depacketizer {
    stream: Stream,
    frames: usize,
    /// Channel indices (left, right) per pair.
    pairs: Vec<[usize; 2]>,
    ssrc: Option<u32>,
    dedupe: Dedupe,
    l: [i32; MAX_PERIOD_FRAMES],
    r: [i32; MAX_PERIOD_FRAMES],
    pub stats: Aes67Stats,
}

impl Aes67Depacketizer {
    /// `pairs`: channel indices (0-based) for each output pair; a mono
    /// channel is `[c, c]`. Errors if the stream cannot be taken (rate, packet
    /// time, channel numbers).
    pub fn new(stream: Stream, pairs: &[[usize; 2]]) -> Result<Aes67Depacketizer, String> {
        if stream.sample_rate != 48_000 {
            return Err(format!(
                "{} Hz: only 48 kHz streams for now",
                stream.sample_rate
            ));
        }
        let frames = stream.frames();
        if frames == 0 || frames > MAX_PERIOD_FRAMES {
            return Err(format!(
                "packet time {} ms is not supported",
                stream.ptime_ms
            ));
        }
        if let Some(c) = pairs.iter().flatten().find(|&&c| c >= stream.channels) {
            return Err(format!(
                "channel {} does not exist (stream has {})",
                c + 1,
                stream.channels
            ));
        }
        Ok(Aes67Depacketizer {
            frames,
            pairs: pairs.to_vec(),
            ssrc: None,
            dedupe: Dedupe::new(),
            l: [0; MAX_PERIOD_FRAMES],
            r: [0; MAX_PERIOD_FRAMES],
            stats: Aes67Stats::default(),
            stream,
        })
    }

    /// Every channel in pairs: 1-2, 3-4, ... (an odd last one mono).
    pub fn all_pairs(channels: usize) -> Vec<[usize; 2]> {
        (0..channels.div_ceil(2))
            .map(|k| [2 * k, (2 * k + 1).min(channels - 1)])
            .collect()
    }

    /// The flow format for a pair's jitter buffer.
    pub fn format(&self) -> FlowFormat {
        FlowFormat {
            sample_rate: self.stream.sample_rate,
            period_frames: self.frames,
            layout: Layout::Single,
        }
    }

    pub fn stream(&self) -> &Stream {
        &self.stream
    }

    pub fn pairs(&self) -> usize {
        self.pairs.len()
    }

    /// Handles one datagram (`now_net_ns`: network time, to unwrap the
    /// timestamp); emits `(pair, chunk)` for each pair.
    pub fn accept(
        &mut self,
        datagram: &[u8],
        now_net_ns: u64,
        mut emit: impl FnMut(usize, &Chunk),
    ) {
        let Ok(p) = rtp::parse(datagram, 0) else {
            self.stats.malformed += 1;
            return;
        };
        if p.header.payload_type != self.stream.payload_type {
            self.stats.malformed += 1;
            return;
        }
        match self.ssrc {
            Some(s) if s != p.header.ssrc => {
                self.stats.foreign += 1;
                return;
            }
            None => self.ssrc = Some(p.header.ssrc),
            _ => {}
        }
        // Size before sequence: a malformed packet must not use up its number.
        let (enc, ch) = (self.stream.encoding, self.stream.channels);
        if p.payload.len() != self.frames * ch * enc.bytes() {
            self.stats.malformed += 1;
            return;
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
        let ts = p.header.timestamp.wrapping_sub(self.stream.media_offset);
        let now = timestamp::samples_at(now_net_ns, self.stream.sample_rate);
        let first = timestamp::unwrap(ts, now) as i64;
        let mut chunk = Chunk::empty();
        chunk.layer = Layer::Full24;
        chunk.first_sample = first;
        let n = self.frames;
        for (k, &[a, b]) in self.pairs.iter().enumerate() {
            decode_channel(p.payload, enc, ch, a, &mut self.l[..n]);
            decode_channel(p.payload, enc, ch, b, &mut self.r[..n]);
            for f in 0..n {
                chunk.samples[f * CHANNELS] = self.l[f];
                chunk.samples[f * CHANNELS + 1] = self.r[f];
            }
            emit(k, &chunk);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::net::Ipv4Addr;
    use rob_core::aes67::{encode_l24, Encoding};
    use rob_core::rtp::RtpHeader;

    fn stream(channels: usize, offset: u32) -> Stream {
        Stream {
            name: "Box".into(),
            session_id: 1,
            origin: Ipv4Addr::LOCALHOST,
            dest: Ipv4Addr::new(239, 69, 1, 1),
            port: 5004,
            payload_type: 97,
            encoding: Encoding::L24,
            sample_rate: 48_000,
            channels,
            ptime_ms: 1.0,
            ptp_gm: None,
            ptp_domain: Some(0),
            media_offset: offset,
            source: None,
        }
    }

    /// One AES67 packet: frame f, channel c holds `c * 1000 + f`.
    fn packet(seq: u16, ts: u32, channels: usize, out: &mut [u8]) -> usize {
        let h = RtpHeader {
            marker: false,
            payload_type: 97,
            sequence: seq,
            timestamp: ts,
            ssrc: 0xD4A7_E000,
        };
        let n = rtp::write_header(out, &h, None).unwrap_or(0);
        let samples: Vec<i32> = (0..48)
            .flat_map(|f| (0..channels).map(move |c| (c * 1000 + f) as i32))
            .collect();
        n + encode_l24(&samples, &mut out[n..])
    }

    #[test]
    fn pairs_timing_and_duplicates() {
        // 8 channels, pairs 1-2 and 7-8, and channel 5 mono; offset applied.
        let offset = 1_000_000;
        let mut d = Aes67Depacketizer::new(stream(8, offset), &[[0, 1], [6, 7], [4, 4]])
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(d.format().period_frames, 48);
        let now_ns = 25 * 3600 * 1_000_000_000u64; // past a 32-bit wrap
        let sample = timestamp::samples_at(now_ns, 48_000) - 480;
        let ts = (sample as u32).wrapping_add(offset);
        let mut buf = [0u8; 1500];
        let n = packet(7, ts, 8, &mut buf);
        let mut got = Vec::new();
        d.accept(&buf[..n], now_ns, |k, c| {
            got.push((
                k,
                c.first_sample,
                c.samples[0],
                c.samples[1],
                c.samples[2 * 47 + 1],
            ))
        });
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], (0, sample as i64, 0, 1_000, 1_047));
        assert_eq!(got[1], (1, sample as i64, 6_000, 7_000, 7_047));
        assert_eq!(got[2], (2, sample as i64, 4_000, 4_000, 4_047));
        // The same packet again (a second path): dropped.
        d.accept(&buf[..n], now_ns, |_, _| panic!("duplicate emitted"));
        assert_eq!(d.stats.duplicates, 1);
        // Wrong size: refused.
        d.accept(&buf[..n - 3], now_ns, |_, _| panic!("short packet emitted"));
        assert_eq!(d.stats.malformed, 1);
    }

    #[test]
    fn refuses_streams_and_channels_it_cannot_take() {
        assert!(Aes67Depacketizer::new(stream(2, 0), &[[0, 2]]).is_err());
        let mut s = stream(2, 0);
        s.sample_rate = 96_000;
        assert!(Aes67Depacketizer::new(s, &[[0, 1]]).is_err());
        assert_eq!(
            Aes67Depacketizer::all_pairs(5),
            vec![[0, 1], [2, 3], [4, 4]]
        );
    }
}
