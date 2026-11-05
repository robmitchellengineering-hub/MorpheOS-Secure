//! IPv4 + ICMP echo — the smallest proof that a real stack exists.
//!
//! WHAT IS PROVEN: the Core composes an IP datagram, computes a correct IP and ICMP checksum, and answers an echo
//! request with an echo reply. The reply is captured off the wire, and the gate re-checks the checksum so a wrong one
//! cannot pass.
//!
//! WHAT IS HONESTLY NOT HERE: the trigger. slirp answers our questions but never asks one — it cannot ping the guest —
//! so the echo request is synthesized from the gateway's real MAC (the same pattern as the ARP reply: the logic and
//! the delivery are real, the trigger is synthetic until a real peer exists, which is rung 4).

// The Core's address is CONFIGURATION, not a memory. Default is QEMU slirp's subnet (10.0.2.15 / gateway 10.0.2.2);
// a real LAN selects its own via a feature, so the same image answers ARP and ping for a real address on a real wire.
#[cfg(feature = "net-lan-19216815")]
pub const OUR_IP: [u8; 4] = [192, 168, 15, 42];
#[cfg(not(feature = "net-lan-19216815"))]
pub const OUR_IP: [u8; 4] = [10, 0, 2, 15];

#[cfg(feature = "net-lan-19216815")]
pub const GATEWAY_IP: [u8; 4] = [192, 168, 15, 1];
#[cfg(not(feature = "net-lan-19216815"))]
pub const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];

/// The length of the echo payload. Fixed so the packet size is known and the checksum has a real subject. Eight bytes
/// keeps the whole frame inside the 64-byte transmit buffer: 14 eth + 20 ip + 8 icmp + 8 payload = 50, plus 10 header.
const PAYLOAD_LEN: usize = 8;

/// RFC 1071 ones'-complement checksum, computed over the given bytes. The mutation makes it wrong, and the gate must
/// catch that — a checksum the Core computes but nobody checks is decoration.
pub fn checksum(data: &[u8]) -> u16 {
    if cfg!(feature = "ip-mutant-bad-checksum") {
        return 0xFFFF;
    }
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// An Ethernet + IPv4 + ICMP echo REQUEST from a peer, addressed to us. The peer's MAC must exist on the wire or the
/// emulated network drops the reply — measured with ARP, where a reply to a synthesised MAC never left slirp.
pub fn echo_request_from(peer_mac: [u8; 6], peer_ip: [u8; 4], our_mac: [u8; 6]) -> ([u8; 64], usize) {
    let mut f = [0u8; 64];
    for i in 0..6 {
        f[i] = our_mac[i]; // dst = us
        f[6 + i] = peer_mac[i]; // src = the asker
    }
    f[12] = 0x08;
    f[13] = 0x00; // IPv4
    let ip = 14;
    let total_len = 20 + 8 + PAYLOAD_LEN;
    f[ip] = 0x45; // v4, IHL 5
    f[ip + 2] = (total_len >> 8) as u8;
    f[ip + 3] = total_len as u8;
    f[ip + 8] = 64; // TTL
    f[ip + 9] = 1; // protocol: ICMP
    for i in 0..4 {
        f[ip + 12 + i] = peer_ip[i]; // src = asker
        f[ip + 16 + i] = OUR_IP[i]; // dst = us
    }
    let cksum = checksum(&f[ip..ip + 20]);
    f[ip + 10] = (cksum >> 8) as u8;
    f[ip + 11] = cksum as u8;
    let icmp = ip + 20;
    f[icmp] = 8; // echo request
    f[icmp + 1] = 0;
    f[icmp + 4] = 0x12;
    f[icmp + 5] = 0x34; // id
    f[icmp + 6] = 0x00;
    f[icmp + 7] = 0x01; // seq
    for i in 0..PAYLOAD_LEN {
        f[icmp + 8 + i] = 0x41 + i as u8; // 'A'.. 'H'
    }
    let cksum = checksum(&f[icmp..icmp + 8 + PAYLOAD_LEN]);
    f[icmp + 2] = (cksum >> 8) as u8;
    f[icmp + 3] = cksum as u8;
    (f, 14 + total_len)
}

/// Answer an echo request with an echo reply. `request` must be an Ethernet + IPv4 + ICMP echo request frame.
pub fn echo_reply(request: &[u8], our_mac: [u8; 6]) -> ([u8; 64], usize) {
    let mut f = [0u8; 64];
    for i in 0..6 {
        f[i] = request[6 + i]; // dst = whoever asked
        f[6 + i] = our_mac[i]; // src = us
    }
    f[12] = 0x08;
    f[13] = 0x00; // IPv4
    let ip = 14;
    let total_len = 20 + 8 + PAYLOAD_LEN;
    f[ip] = 0x45;
    f[ip + 2] = (total_len >> 8) as u8;
    f[ip + 3] = total_len as u8;
    f[ip + 8] = 64;
    f[ip + 9] = 1;
    for i in 0..4 {
        f[ip + 12 + i] = OUR_IP[i]; // src = us
        f[ip + 16 + i] = request[ip + 12 + i]; // dst = the asker
    }
    let cksum = checksum(&f[ip..ip + 20]);
    f[ip + 10] = (cksum >> 8) as u8;
    f[ip + 11] = cksum as u8;
    let icmp = ip + 20;
    f[icmp] = 0; // echo reply
    f[icmp + 1] = 0;
    f[icmp + 4] = request[icmp + 4]; // id, echoed
    f[icmp + 5] = request[icmp + 5];
    f[icmp + 6] = request[icmp + 6]; // seq, echoed
    f[icmp + 7] = request[icmp + 7];
    for i in 0..PAYLOAD_LEN {
        f[icmp + 8 + i] = request[icmp + 8 + i]; // payload, echoed
    }
    let cksum = checksum(&f[icmp..icmp + 8 + PAYLOAD_LEN]);
    f[icmp + 2] = (cksum >> 8) as u8;
    f[icmp + 3] = cksum as u8;
    (f, 14 + total_len)
}
