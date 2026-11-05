//! RTP — the packet a show's audio rides on.
//!
//! WHAT IS PROVEN: the Core emits RTP packets whose sequence number strictly increments and whose timestamp advances by
//! a fixed step derived from the Core's own sample clock (48 kHz, so 1 ms = 48 samples). The claim is determinism, and
//! the gate verifies it off the wire: sequences 0..n and timestamp deltas of exactly the stated step.
//!
//! WHAT IS HONESTLY NOT HERE: real audio samples. The payload is a placeholder pattern; the DSP path and the plugins
//! are deferred by Rob. What is here is the transport envelope AirMatrix's engine will fill.

use crate::udp;

pub const PAYLOAD_TYPE: u8 = 96; // dynamic RTP payload type, L24/48k — the format a real engine would use
pub const SAMPLE_RATE: u32 = 48000;
/// One millisecond of mono 48 kHz — the timestamp advance between consecutive packets at the show's cadence.
pub const TIMESTAMP_STEP: u32 = 48;
pub const PAYLOAD_BYTES: usize = 48; // a placeholder: 16 mono 24-bit samples, to be replaced by real samples

/// Build one Ethernet + IPv4 + UDP + RTP packet. The RTP header is 12 bytes; the payload is the placeholder block.
pub fn packet(
    our_mac: [u8; 6],
    our_ip: [u8; 4],
    dst_mac: [u8; 6],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    seq: u16,
    timestamp: u32,
    ssrc: u32,
) -> ([u8; 256], usize) {
    // RFC 3550 RTP header (12 bytes) + the AMT one-byte extension (16 bytes) + payload. X=1 marks the extension.
    let mut rtp = [0u8; 12 + 16 + PAYLOAD_BYTES];
    rtp[0] = 0x90; // V=2, X=1 (extension present)
    rtp[1] = PAYLOAD_TYPE;
    if cfg!(feature = "rtp-mutant-frozen-seq") {
        rtp[2] = 0;
        rtp[3] = 0; // the mutation: the stream never advances
    } else {
        rtp[2] = (seq >> 8) as u8;
        rtp[3] = seq as u8;
    }
    rtp[4] = (timestamp >> 24) as u8;
    rtp[5] = (timestamp >> 16) as u8;
    rtp[6] = (timestamp >> 8) as u8;
    rtp[7] = timestamp as u8;
    if cfg!(feature = "rtp-mutant-wrong-ssrc") {
        rtp[8] = 0;
        rtp[9] = 0;
        rtp[10] = 0;
        rtp[11] = 0; // the mutation: the SSRC is wrong, so neither stream can be attributed
    } else {
        rtp[8] = (ssrc >> 24) as u8;
        rtp[9] = (ssrc >> 16) as u8;
        rtp[10] = (ssrc >> 8) as u8;
        rtp[11] = ssrc as u8;
    }
    // AMT extension, RFC 8285 one-byte form: profile 0xBEDE, length 3 words, element ID 1 / length 9, then the
    // 9-byte amt-core data (version/mode, path/slot, budget_us, send_time_us) and 2 padding bytes. Zero for now —
    // this is the transport envelope AirMatrix's engine fills with real values.
    rtp[12] = 0xBE;
    rtp[13] = 0xDE;
    rtp[14] = 0x00;
    rtp[15] = 0x03;
    rtp[16] = 0x18;
    for i in 0..PAYLOAD_BYTES {
        rtp[28 + i] = (seq as u8).wrapping_add(i as u8); // a ramp keyed to the sequence, so a frozen sequence is visible
    }
    udp::datagram(our_mac, our_ip, dst_mac, dst_ip, src_port, dst_port, &rtp)
}

/// The fields the gate verifies, read back off a captured frame.
pub struct Seen {
    pub seq: u16,
    pub timestamp: u32,
}

/// Parse an RTP packet's sequence and timestamp. `None` if the frame is not RTP (UDP, first byte 0x80).
pub fn parse(frame: &[u8]) -> Option<Seen> {
    // Ethernet 14 + IP 20 + UDP 8 = 42, then the 12-byte RTP header.
    if frame.len() < 42 + 12 {
        return None;
    }
    if frame[12] != 0x08 || frame[13] != 0x00 || frame[14 + 9] != 17 {
        return None;
    }
    let rtp = 42;
    if frame[rtp] != 0x80 {
        return None;
    }
    let seq = ((frame[rtp + 2] as u16) << 8) | frame[rtp + 3] as u16;
    let timestamp = ((frame[rtp + 4] as u32) << 24)
        | ((frame[rtp + 5] as u32) << 16)
        | ((frame[rtp + 6] as u32) << 8)
        | frame[rtp + 7] as u32;
    Some(Seen { seq, timestamp })
}
