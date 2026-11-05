//! UDP — a datagram in and out.
//!
//! WHAT IS PROVEN: the Core composes an IPv4/UDP datagram (with a correct UDP checksum over the RFC 768 pseudo-header)
//! and puts it on the wire; and it parses an inbound datagram, extracting ports, length and payload. The out direction
//! is real (captured off the wire); the in direction is real parsing against a synthesized datagram until a real peer
//! exists (rung 4).

use crate::ip;

/// RFC 768 checksum over the pseudo-header (src IP, dst IP, zero, protocol, UDP length) plus the UDP segment. A UDP
/// checksum of zero would be legal in IPv4 ("not computed") — but for an audio transport, a checksum that is not
/// computed is a checksum that cannot detect a bad packet, so it is computed.
pub fn checksum(src_ip: [u8; 4], dst_ip: [u8; 4], segment: &[u8]) -> u16 {
    if cfg!(feature = "udp-mutant-bad-checksum") {
        return 0xFFFF;
    }
    let len = segment.len();
    // Pseudo-header is 12 bytes; the segment may be odd, so the sum carries a final zero byte.
    let mut sum: u32 = 0;
    let add_word = |sum: &mut u32, hi: u8, lo: u8| {
        *sum += ((hi as u32) << 8) | lo as u32;
    };
    add_word(&mut sum, src_ip[0], src_ip[1]);
    add_word(&mut sum, src_ip[2], src_ip[3]);
    add_word(&mut sum, dst_ip[0], dst_ip[1]);
    add_word(&mut sum, dst_ip[2], dst_ip[3]);
    add_word(&mut sum, 0, 17); // zero + protocol
    add_word(&mut sum, (len >> 8) as u8, len as u8);
    let mut i = 0;
    while i + 1 < segment.len() {
        add_word(&mut sum, segment[i], segment[i + 1]);
        i += 2;
    }
    if i < segment.len() {
        add_word(&mut sum, segment[i], 0);
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Build an Ethernet + IPv4 + UDP datagram. Payload must fit in the 64-byte transmit buffer: 14 eth + 20 ip + 8 udp
/// leaves 22 bytes of payload.
pub fn datagram(
    our_mac: [u8; 6],
    our_ip: [u8; 4],
    dst_mac: [u8; 6],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> ([u8; 256], usize) {
    let mut f = [0u8; 256];
    for i in 0..6 {
        f[i] = dst_mac[i];
        f[6 + i] = our_mac[i];
    }
    f[12] = 0x08;
    f[13] = 0x00; // IPv4
    let ip_off = 14;
    let udp_len = 8 + payload.len();
    let total_len = 20 + udp_len;
    f[ip_off] = 0x45;
    f[ip_off + 2] = (total_len >> 8) as u8;
    f[ip_off + 3] = total_len as u8;
    f[ip_off + 8] = 64;
    f[ip_off + 9] = 17; // protocol UDP
    for i in 0..4 {
        f[ip_off + 12 + i] = our_ip[i];
        f[ip_off + 16 + i] = dst_ip[i];
    }
    let ip_cksum = ip::checksum(&f[ip_off..ip_off + 20]);
    f[ip_off + 10] = (ip_cksum >> 8) as u8;
    f[ip_off + 11] = ip_cksum as u8;
    let udp_off = ip_off + 20;
    f[udp_off] = (src_port >> 8) as u8;
    f[udp_off + 1] = src_port as u8;
    f[udp_off + 2] = (dst_port >> 8) as u8;
    f[udp_off + 3] = dst_port as u8;
    f[udp_off + 4] = (udp_len >> 8) as u8;
    f[udp_off + 5] = udp_len as u8;
    for (i, b) in payload.iter().enumerate() {
        f[udp_off + 8 + i] = *b;
    }
    let udp_cksum = checksum(our_ip, dst_ip, &f[udp_off..udp_off + udp_len]);
    f[udp_off + 6] = (udp_cksum >> 8) as u8;
    f[udp_off + 7] = udp_cksum as u8;
    (f, 14 + total_len)
}

/// What a parsed datagram holds.
pub struct Parsed {
    pub src_port: u16,
    pub dst_port: u16,
    pub payload_len: usize,
}

/// Parse an Ethernet + IPv4 + UDP frame. `None` if it is not UDP, or if the length field disagrees with the IP total
/// length — which is what a length-ignoring mutation must be caught on.
pub fn parse(frame: &[u8]) -> Option<Parsed> {
    if frame.len() < 14 + 20 + 8 {
        return None;
    }
    if frame[12] != 0x08 || frame[13] != 0x00 || frame[14 + 9] != 17 {
        return None;
    }
    let ip_off = 14;
    let total = ((frame[ip_off + 2] as usize) << 8) | frame[ip_off + 3] as usize;
    let udp_off = ip_off + 20;
    let udp_len = ((frame[udp_off + 4] as usize) << 8) | frame[udp_off + 5] as usize;
    if cfg!(feature = "udp-mutant-ignore-length") {
        // The mutation trusts nothing: it reports the WHOLE buffer as the payload, which is a parse that read past the
        // datagram. The gate must catch this.
        return Some(Parsed {
            src_port: ((frame[udp_off] as u16) << 8) | frame[udp_off + 1] as u16,
            dst_port: ((frame[udp_off + 2] as u16) << 8) | frame[udp_off + 3] as u16,
            payload_len: frame.len() - udp_off - 8,
        });
    }
    if udp_len < 8 || 20 + udp_len != total {
        return None;
    }
    Some(Parsed {
        src_port: ((frame[udp_off] as u16) << 8) | frame[udp_off + 1] as u16,
        dst_port: ((frame[udp_off + 2] as u16) << 8) | frame[udp_off + 3] as u16,
        payload_len: udp_len - 8,
    })
}
