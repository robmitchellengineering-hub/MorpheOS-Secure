//! A sending node: audio thread → lock-free queue → network thread.
//!
//! * [`SenderAudio`] (audio thread): resamples the ADC's frames onto the network
//!   time grid and cuts them into periods.
//! * [`SenderNet`] (network thread): builds one packet per period (with
//!   redundancy for earlier periods) and hands it to the socket.
//!
//! The ADC runs on its own crystal. Its frames are *read* at network-time
//! positions by the same phase-locked resampler the receiver uses, so a period's
//! timestamp really is the capture time of its first sample.

use crate::prelude::*;

use crate::asrc::Asrc;
use crate::flow::{FlowFormat, CHANNELS, MAX_PERIOD_SAMPLES};
use crate::interp::Quality;
use crate::packetize::{Packetizer, MAX_PACKET};
use crate::pll::TrackerConfig;
use crate::rolling::RollingBuffer;

/// One period of network-time audio, passed by value (no allocation).
#[derive(Clone, Copy)]
pub struct Period {
    pub first_sample: i64,
    pub samples: [i32; MAX_PERIOD_SAMPLES],
}

#[derive(Debug, Clone, Copy)]
pub struct SenderConfig {
    pub fmt: FlowFormat,
    pub quality: Quality,
    /// Offset of this node's period grid in samples (its soft time slot).
    pub phase: i64,
    pub ssrc: u32,
    pub payload_type: u8,
    pub ext_id: u8,
    /// The latency the receivers use, for each packet's `budget_us`.
    pub latency_ns: u64,
    /// Phase-locked loop settings for the ADC-side resampler.
    pub tracker: TrackerConfig,
}

impl SenderConfig {
    pub fn new(fmt: FlowFormat, ssrc: u32, latency_ns: u64) -> SenderConfig {
        SenderConfig {
            fmt,
            quality: Quality::Standard,
            phase: 0,
            ssrc,
            payload_type: 97,
            ext_id: rob_core::rtp::DEFAULT_ROB_EXT_ID,
            latency_ns,
            tracker: TrackerConfig {
                output_rate: f64::from(fmt.sample_rate),
                ..TrackerConfig::default()
            },
        }
    }
}

/// The send-grid phase (offset) for each of `n` senders, spread evenly across
/// `period_frames` so no two send at the same instant (design 12, "soft time
/// slots"). Sender `i` gets `i * period_frames / n`; the caller assigns each
/// phase to a node's [`SenderConfig::phase`] (same units: period-grid frames —
/// [`FlowFormat::period_frames`] = 96 at 2 ms 48 kHz).
///
/// `n = 1` gives `[0]`; `n = 0` gives no phases.
pub fn spread_phases(n: usize, period_frames: usize) -> Vec<i64> {
    if n == 0 {
        return Vec::new();
    }
    let den = n as i64;
    let step = period_frames as i64;
    (0..n).map(|i| i as i64 * step / den).collect()
}

pub struct SenderAudio {
    cfg: SenderConfig,
    asrc: Asrc,
    queue: rtrb::Producer<Period>,
    /// Card frames pushed so far (the next card frame's index).
    card_frames: i64,
    /// Network sample index where the next period starts.
    next_period: Option<i64>,
    /// Latest anchor: (card index, network sample position as a whole number
    /// and a fraction; one f64 would lose the fraction at network-time
    /// magnitudes).
    anchor: Option<(i64, i64, f64)>,
    scratch: [f32; MAX_PERIOD_SAMPLES],
    /// Periods dropped because the network thread fell behind.
    pub queue_full: u64,
    /// Periods produced.
    pub periods: u64,
}

pub struct SenderNet {
    cfg: SenderConfig,
    packetizer: Packetizer,
    queue: rtrb::Consumer<Period>,
    buf: [u8; MAX_PACKET],
    /// Everything this node sent, for the recorder to resend (design 02, idea 6).
    buffer: RollingBuffer,
    pub packets: u64,
}

/// Builds the two halves of a sender. Allocates; call before audio starts.
pub fn sender(cfg: SenderConfig) -> (SenderAudio, SenderNet) {
    let (tx, rx) = rtrb::RingBuffer::new(256);
    (
        SenderAudio {
            cfg,
            asrc: Asrc::new(CHANNELS, cfg.quality, cfg.tracker, 1 << 14),
            queue: tx,
            card_frames: 0,
            next_period: None,
            anchor: None,
            scratch: [0.0; MAX_PERIOD_SAMPLES],
            queue_full: 0,
            periods: 0,
        },
        SenderNet {
            cfg,
            packetizer: Packetizer::new(cfg.fmt, cfg.ssrc, cfg.payload_type, cfg.ext_id),
            queue: rx,
            buf: [0; MAX_PACKET],
            // 60 s at 500 packets/s (2 ms periods).
            buffer: RollingBuffer::new(30_000),
            packets: 0,
        },
    )
}

impl SenderAudio {
    /// Takes one block of ADC frames (interleaved stereo). `adc_net_ns` is the
    /// network time at which `input`'s first frame was captured. No allocation.
    pub fn process(&mut self, input: &[f32], adc_net_ns: Option<u64>) {
        let block_first = self.card_frames;
        if self.card_frames == 0 {
            self.asrc.reset_input(0);
        }
        for frame in input.chunks_exact(CHANNELS) {
            self.asrc.push_frame(frame);
            self.card_frames += 1;
        }
        if let Some(t) = adc_net_ns {
            let x = i128::from(t) * i128::from(self.cfg.fmt.sample_rate);
            self.anchor = Some((
                block_first,
                x.div_euclid(1_000_000_000) as i64,
                x.rem_euclid(1_000_000_000) as f64 / 1e9,
            ));
        }
        let Some((c0, s0_whole, s0_frac)) = self.anchor else {
            return;
        };
        let f = self.cfg.fmt.period_frames as i64;
        let history = (self.asrc.kernel_latency() + 1) as f64;
        let next = *self.next_period.get_or_insert_with(|| {
            // The first period whose card audio is all in the ring.
            let earliest = s0_whole - c0 + (s0_frac + history).ceil() as i64 + 64;
            let k = (earliest - self.cfg.phase).div_euclid(f) + 1;
            k * f + self.cfg.phase
        });
        let mut next = next;
        loop {
            // Card position that network sample `next` should read.
            // (Whole parts subtracted as integers first: exact.)
            let target = c0 as f64 + ((next - s0_whole) as f64 - s0_frac);
            let needed =
                (target + f as f64 * 1.003).ceil() as i64 + self.asrc.kernel_latency() as i64 + 1;
            if needed > self.card_frames {
                break;
            }
            let n = self.cfg.fmt.period_samples();
            self.asrc.render(&mut self.scratch[..n], Some(target));
            let mut p = Period {
                first_sample: next,
                samples: [0; MAX_PERIOD_SAMPLES],
            };
            for (d, s) in p.samples[..n].iter_mut().zip(&self.scratch[..n]) {
                *d = (s.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
            }
            if self.queue.push(p).is_err() {
                self.queue_full += 1;
            }
            self.periods += 1;
            next += f;
        }
        self.next_period = Some(next);
    }

    pub fn asrc(&self) -> &Asrc {
        &self.asrc
    }
}

impl SenderNet {
    /// Sends every queued period: `send` gets each packet's bytes. `now_net_ns` is
    /// network time now.
    pub fn poll(&mut self, now_net_ns: u64, mut send: impl FnMut(&[u8])) {
        let fs = u64::from(self.cfg.fmt.sample_rate);
        let n = self.cfg.fmt.period_samples();
        while let Ok(p) = self.queue.pop() {
            let capture_ns = (p.first_sample.max(0) as u64).saturating_mul(1_000_000_000) / fs;
            let deadline = capture_ns + self.cfg.latency_ns;
            if let Ok(b) = self.packetizer.build(
                p.first_sample,
                &p.samples[..n],
                now_net_ns,
                deadline,
                0,
                0,
                &mut self.buf,
            ) {
                send(&self.buf[..b.len]);
                self.buffer.push(b.seq, &self.buf[..b.len]);
                self.packets += 1;
            }
            self.packetizer.advance(p.first_sample, &p.samples[..n]);
        }
    }

    /// The packet with this sequence number, if still buffered (design 02,
    /// idea 6: the recorder asks a node to resend what the live path lost).
    pub fn resend(&self, seq: u16) -> Option<&[u8]> {
        self.buffer.get(seq)
    }

    /// Answers one archive resend request datagram from the rolling buffer:
    /// returns the response length, or `None` if `req` is not a request. The
    /// response is `"RRSP" seq len packet`, with an empty packet for "not
    /// found" (design 02, idea 6 — the archive path, never the live path).
    pub fn answer_resend(&self, req: &[u8], out: &mut [u8]) -> Option<usize> {
        let seq = rob_core::resend::parse_req(req)?;
        let packet = self.resend(seq).unwrap_or(&[]);
        rob_core::resend::write_resp(seq, packet, out).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives enough tone blocks through a sender to fill its rolling buffer
    /// with at least one packet, and returns that packet.
    fn one_sent_packet() -> (SenderNet, Vec<u8>) {
        let fmt = FlowFormat::layered();
        let (mut audio, mut net) = sender(SenderConfig::new(fmt, 0x7, 20_000_000));
        let block = fmt.period_frames;
        let mut input = vec![0.0f32; block * CHANNELS];
        for (j, v) in input.iter_mut().enumerate() {
            *v = (j % 100) as f32 / 100.0;
        }
        let block_ns = fmt.period_ns();
        for k in 0..32u64 {
            audio.process(&input, Some(k * block_ns));
        }
        let mut captured: Vec<Vec<u8>> = Vec::new();
        net.poll(0, |p| captured.push(p.to_vec()));
        let packet = captured
            .pop()
            .expect("32 blocks must send at least one period");
        (net, packet)
    }

    #[test]
    fn answer_resend_round_trips_a_buffered_packet() {
        let (net, packet) = one_sent_packet();
        let seq = rob_core::rtp::parse(&packet, rob_core::rtp::DEFAULT_ROB_EXT_ID)
            .unwrap()
            .header
            .sequence;
        let mut req = [0u8; rob_core::resend::REQ_LEN];
        rob_core::resend::write_req(seq, &mut req).unwrap();
        let mut resp = [0u8; 8 + MAX_PACKET];
        let n = net.answer_resend(&req, &mut resp).unwrap();
        assert_eq!(
            rob_core::resend::parse_resp(&resp[..n]),
            Some((seq, &packet[..]))
        );
    }

    #[test]
    fn spread_phases_staggers_senders_across_the_period() {
        assert_eq!(spread_phases(4, 96), vec![0, 24, 48, 72]);
        assert_eq!(spread_phases(2, 96), vec![0, 48]);
        assert_eq!(spread_phases(1, 96), vec![0]);
        assert_eq!(spread_phases(0, 96), Vec::<i64>::new());
        // Every phase is inside the period and distinct.
        let p = spread_phases(7, 96);
        assert!(p.iter().all(|&x| (0..96).contains(&x)));
        let mut sorted = p.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 7);
    }

    #[test]
    fn answer_resend_reports_an_unknown_sequence_as_not_found() {
        let (net, _) = one_sent_packet();
        let mut req = [0u8; rob_core::resend::REQ_LEN];
        rob_core::resend::write_req(99, &mut req).unwrap();
        let mut resp = [0u8; 8 + MAX_PACKET];
        let n = net.answer_resend(&req, &mut resp).unwrap();
        assert_eq!(
            rob_core::resend::parse_resp(&resp[..n]),
            Some((99, &[][..]))
        );
    }
}
