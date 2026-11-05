//! The network device, brought up far enough to send one frame.
//!
//! WHAT IS PROVEN HERE: a descriptor ring in our own memory, a buffer the device can read, a kick, and a used-ring
//! entry that comes back — then one Ethernet/ARP frame that the HOST side captures. The pcap either contains our bytes
//! or it does not; there is no "seems to work" available.
//!
//! WHAT IS NOT HERE: a receive path that queues into anything, a protocol stack, checksums, retries. This is the
//! smallest honest milestone: the wire saw us.
//!
//! THE LAYOUT SUPPORTS BOTH: legacy (version 1, one page frame into `QueuePFN`) and modern (version 2, three separate
//! addresses plus `QueueReady`). Both were measured against QEMU and both behave identically once the queue is right.

const DESC_SIZE: usize = 16;
const AVAIL_OFF: usize = 0x1000;
const USED_OFF: usize = 0x2000;

/// The rings, page-aligned because the legacy interface addresses them by page frame number.
#[repr(C, align(4096))]
pub struct Rings {
    pub bytes: [u8; 0x3000],
}

static mut RINGS: Rings = Rings { bytes: [0u8; 0x3000] };
static mut FRAME: [u8; 64] = [0u8; 64];

pub struct Device {
    pub base: usize,
    pub mac: [u8; 6],
    pub queue_num: u16,
}

unsafe fn rd(base: usize, off: usize) -> u32 {
    core::ptr::read_volatile((base + off) as *const u32)
}
unsafe fn wr(base: usize, off: usize, value: u32) {
    core::ptr::write_volatile((base + off) as *mut u32, value);
}

/// Bring the device's queue up. `Ok` means the queue's page frame was accepted and a descriptor was offered.
pub fn start_queue(dev: &Device) -> Result<(), &'static str> {
    unsafe {
        // THE SELECTOR FIRST. Legacy virtio-mmio addresses every queue through QueueSel, and a queue operation against
        // an unselected queue is undefined — measured: the queue number read back as 0, which is what "no queue
        // selected" looks like from the driver's side.
        // QUEUE 1 IS TRANSMIT. virtio-net has two queues and queue 0 is RECEIVE — measured: a perfectly formed ring with
        // descriptor, length, flags and available index all correct, offered to queue 0, is never completed, because a
        // receive queue is not where a frame goes out. The ring was right all along; the queue was wrong.
        wr(dev.base, 0x030, 1); // QueueSel = 1, the transmit queue
        let max = (rd(dev.base, 0x034) as u16).min(8);
        if max == 0 {
            return Err("the device offers no queue");
        }
        wr(dev.base, 0x038, max as u32); // QueueNum
        wr(dev.base, 0x03c, 4096); // QueueAlign = page size, legacy
        let rings = &raw mut RINGS as *mut u8;
        // Zero the rings: a used ring with a stale index makes the first completion look like the hundredth.
        for i in 0..0x3000 {
            core::ptr::write_volatile(rings.add(i), 0);
        }
        let version = rd(dev.base, 0x004);
        if version >= 2 {
            // MODERN: the three rings are named separately and the queue is not live until QueueReady is set.
            // Measured: the same device reports version 2 with `force-legacy=false`, and writing the legacy page frame
            // to it does nothing at all — which is exactly the silence that cost rounds on the legacy path.
            let desc = rings as u64;
            let avail = (rings as u64) + AVAIL_OFF as u64;
            let used = (rings as u64) + USED_OFF as u64;
            wr(dev.base, 0x080, desc as u32);
            wr(dev.base, 0x084, (desc >> 32) as u32);
            wr(dev.base, 0x090, avail as u32);
            wr(dev.base, 0x094, (avail >> 32) as u32);
            wr(dev.base, 0x0a0, used as u32);
            wr(dev.base, 0x0a4, (used >> 32) as u32);
            wr(dev.base, 0x044, 1); // QueueReady
        } else {
            let pfn = (rings as usize) / 4096;
            wr(dev.base, 0x040, pfn as u32); // QueuePFN: where the rings live
        }
        Ok(())
    }
}

/// Send one frame. The buffer is reused, so the caller's bytes must fit in 64.
///
/// THE AVAILABLE INDEX MUST GROW. Measured: every transmit wrote `avail.idx = 1` (a constant), so after the first
/// frame the device's notion of "new work" never advanced and every later kick was ignored — the re-request and the
/// ARP reply were composed and completed and never appeared in the capture. The index is monotonic, and the completion
/// is read from the used-ring entry that matches it.
pub fn transmit(dev: &Device, frame: &[u8]) -> Result<u32, &'static str> {
    if frame.len() + 10 > 64 {
        return Err("frame too large for this milestone's buffer");
    }
    unsafe {
        const NET_HDR: usize = 10;
        static mut TX_AVAIL_IDX: u16 = 0;

        let rings = &raw mut RINGS as *mut u8;
        let buf = &raw mut FRAME as *mut u8;
        // Every virtio-net TX buffer starts with a 10-byte header, zeroed, then the frame.
        for i in 0..NET_HDR {
            core::ptr::write_volatile(buf.add(i), 0);
        }
        for (i, b) in frame.iter().enumerate() {
            core::ptr::write_volatile(buf.add(NET_HDR + i), *b);
        }

        // Descriptor 0: the buffer, its length, no chaining.
        let desc = rings;
        core::ptr::write_volatile(desc as *mut u64, buf as u64);
        core::ptr::write_volatile((desc as *mut u32).add(2), (NET_HDR + frame.len()) as u32);
        core::ptr::write_volatile((desc as *mut u16).add(6), 0); // flags
        core::ptr::write_volatile((desc as *mut u16).add(7), 0); // next

        // The available ring: monotonic index, single descriptor 0.
        TX_AVAIL_IDX = TX_AVAIL_IDX.wrapping_add(1);
        if TX_AVAIL_IDX == 0 {
            TX_AVAIL_IDX = 1;
        }
        let my_idx = TX_AVAIL_IDX;
        let avail = rings.add(AVAIL_OFF);
        core::ptr::write_volatile(avail as *mut u16, 0); // flags
        core::ptr::write_volatile((avail as *mut u16).add(1), my_idx); // idx: must grow
        core::ptr::write_volatile((avail as *mut u16).add(2), 0); // ring[0] = descriptor 0

        // Kick the transmit queue, then wait for the completion that matches THIS index.
        wr(dev.base, 0x050, 1);
        let used = rings.add(USED_OFF);
        let mut spins = 0u32;
        loop {
            let used_idx = core::ptr::read_volatile((used as *const u16).add(1));
            if used_idx >= my_idx {
                let slot = (my_idx.wrapping_sub(1) as usize) % 8;
                let entry = used.add(4 + slot * 8);
                let written = core::ptr::read_volatile((entry as *const u32).add(1));
                return Ok(written);
            }
            spins += 1;
            if spins > 10_000_000 {
                return Err("the device never completed the descriptor");
            }
        }
    }
}

/// What the driver believes it wrote, read back through the same memory the device is supposed to read. A ring that the
/// device ignores and a ring the driver mis-wrote look identical from outside: both are a timeout. These numbers
/// separate them, and they cost one line of console rather than another hypothesis.
pub fn ring_snapshot() -> (u64, u32, u16, u16, u16, u16, u16) {
    unsafe {
        let rings = &raw const RINGS as *const u8;
        let desc = rings;
        let addr = core::ptr::read_volatile(desc as *const u64);
        let len = core::ptr::read_volatile((desc as *const u32).add(2));
        let flags = core::ptr::read_volatile((desc as *const u16).add(6));
        let avail = rings.add(AVAIL_OFF);
        let aflags = core::ptr::read_volatile(avail as *const u16);
        let aidx = core::ptr::read_volatile((avail as *const u16).add(1));
        let aring0 = core::ptr::read_volatile((avail as *const u16).add(2));
        (addr, len, flags, aflags, aidx, aring0, len as u16)
    }
}

/// Build an Ethernet + ARP request: "who has 10.0.2.2? tell 10.0.2.15". A frame a host-side capture cannot mistake for
/// noise, and one that needs no protocol stack above it.
pub fn arp_request(mac: [u8; 6]) -> ([u8; 42], usize) {
    let mut f = [0u8; 42];
    for i in 0..6 {
        f[i] = 0xff; // broadcast
        f[6 + i] = mac[i];
    }
    f[12] = 0x08;
    f[13] = 0x06; // ARP
    f[14] = 0x00;
    f[15] = 0x01; // Ethernet
    f[16] = 0x08;
    f[17] = 0x00; // IPv4
    f[18] = 6; // hlen
    f[19] = 4; // plen
    f[20] = 0x00;
    f[21] = 0x01; // request
    for i in 0..6 {
        f[22 + i] = mac[i]; // sender hardware
    }
    f[28] = 10;
    f[29] = 0;
    f[30] = 2;
    f[31] = 15; // sender protocol: 10.0.2.15
    for i in 0..6 {
        f[32 + i] = 0; // target hardware unknown
    }
    f[38] = 10;
    f[39] = 0;
    f[40] = 2;
    f[41] = 2; // target protocol: 10.0.2.2, QEMU's gateway
    (f, 42)
}

/// A request addressed TO us, "who has 10.0.2.15? tell 10.0.2.2", from the given peer. The peer's MAC must exist on
/// the wire or the emulated network drops the reply: measured, a reply to a synthesised 52:54:00:00:00:01 was composed,
/// transmitted and completed, and never appeared in the capture, because slirp has no route to that address.
pub fn arp_request_from(peer_mac: [u8; 6]) -> ([u8; 42], usize) {
    let mut f = [0u8; 42];
    for i in 0..6 {
        f[i] = 0xff; // broadcast
        f[6 + i] = peer_mac[i]; // the asker
    }
    f[12] = 0x08;
    f[13] = 0x06; // ARP
    f[14] = 0x00;
    f[15] = 0x01; // Ethernet
    f[16] = 0x08;
    f[17] = 0x00; // IPv4
    f[18] = 6;
    f[19] = 4;
    f[20] = 0x00;
    f[21] = 0x01; // request
    for i in 0..6 {
        f[22 + i] = peer_mac[i]; // sender hardware
    }
    f[28] = 10;
    f[29] = 0;
    f[30] = 2;
    f[31] = 2; // sender protocol: 10.0.2.2
    for i in 0..6 {
        f[32 + i] = 0; // target hardware unknown
    }
    f[38] = 10;
    f[39] = 0;
    f[40] = 2;
    f[41] = 15; // target protocol: 10.0.2.15 — us
    (f, 42)
}

/// Build the reply to an ARP request: "10.0.2.15 is at <our MAC>", sent to whoever asked. Ethernet and ARP only — the
/// answer is 42 bytes and there is nothing above it to get wrong.
pub fn arp_reply(request: &[u8], mac: [u8; 6]) -> ([u8; 42], usize) {
    let mut f = [0u8; 42];
    for i in 0..6 {
        f[i] = request[6 + i]; // dst = whoever asked
        f[6 + i] = mac[i]; // src = us
    }
    f[12] = 0x08;
    f[13] = 0x06;
    f[14] = 0x00;
    f[15] = 0x01;
    f[16] = 0x08;
    f[17] = 0x00;
    f[18] = 6;
    f[19] = 4;
    f[20] = 0x00;
    f[21] = 0x02; // reply
    for i in 0..6 {
        f[22 + i] = mac[i]; // sender hardware = us
    }
    f[28] = 10;
    f[29] = 0;
    f[30] = 2;
    f[31] = 15; // sender protocol = us, 10.0.2.15
    for i in 0..6 {
        f[32 + i] = request[22 + i]; // target hardware = the asker
    }
    for i in 0..4 {
        f[38 + i] = request[28 + i]; // target protocol = the asker's address
    }
    (f, 42)
}



// ----------------------------------------------------------------------------------------------------------------
// Receive. Queue 0, its own rings, and a bounded poll: the reply to our ARP request is the first inbound packet.
// ----------------------------------------------------------------------------------------------------------------

const RX_BUFS: usize = 8;
const RX_BUF_LEN: usize = 2048;

#[repr(C, align(4096))]
struct RxRings {
    bytes: [u8; 0x3000],
}

static mut RINGS_RX: RxRings = RxRings { bytes: [0u8; 0x3000] };
static mut RX_BUFFERS: [[u8; RX_BUF_LEN]; RX_BUFS] = [[0u8; RX_BUF_LEN]; RX_BUFS];
static mut RX_LAST_SEEN: u16 = 0;

/// Give the device eight buffers to fill. Each descriptor is DEVICE-WRITABLE (flag 2), which is the difference between
/// a transmit ring and a receive ring: the device writes into these, the driver reads.
pub fn start_rx(dev: &Device) -> Result<(), &'static str> {
    unsafe {
        wr(dev.base, 0x030, 0); // QueueSel = 0: RECEIVE, the queue that was mistaken for a transmit queue earlier
        let max = (rd(dev.base, 0x034) as u16).min(RX_BUFS as u16);
        if max == 0 {
            return Err("the device offers no receive queue");
        }
        wr(dev.base, 0x038, max as u32);
        wr(dev.base, 0x03c, 4096);
        let rings = &raw mut RINGS_RX as *mut u8;
        for i in 0..0x3000 {
            core::ptr::write_volatile(rings.add(i), 0);
        }
        let desc = rings;
        let avail = rings.add(AVAIL_OFF);
        for i in 0..max as usize {
            let buf = (&raw mut RX_BUFFERS as *mut u8).add(i * RX_BUF_LEN);
            // A descriptor is 16 bytes: addr u64 at 0, len u32 at 8, flags u16 at 12, next u16 at 14. Measured: writing
            // the address with a u64 stride of 1 put descriptor 1's address on top of descriptor 0's length, and the
            // device answered "bogus descriptor or out of resources" - the device naming the driver's arithmetic error.
            core::ptr::write_volatile((desc as *mut u64).add(i * 2), buf as u64);
            core::ptr::write_volatile((desc as *mut u32).add(i * 4 + 2), RX_BUF_LEN as u32);
            core::ptr::write_volatile((desc as *mut u16).add(i * 8 + 6), 2); // VIRTQ_DESC_F_WRITE
            core::ptr::write_volatile((desc as *mut u16).add(i * 8 + 7), 0);
            core::ptr::write_volatile((avail as *mut u16).add(2 + i), i as u16);
        }
        core::ptr::write_volatile((avail as *mut u16).add(1), max);
        let version = rd(dev.base, 0x004);
        if version >= 2 {
            let d = desc as u64;
            let a = avail as u64;
            let u = (rings as u64) + USED_OFF as u64;
            wr(dev.base, 0x080, d as u32);
            wr(dev.base, 0x084, (d >> 32) as u32);
            wr(dev.base, 0x090, a as u32);
            wr(dev.base, 0x094, (a >> 32) as u32);
            wr(dev.base, 0x0a0, u as u32);
            wr(dev.base, 0x0a4, (u >> 32) as u32);
            wr(dev.base, 0x044, 1); // QueueReady
        } else {
            wr(dev.base, 0x040, ((rings as usize) / 4096) as u32);
        }
        wr(dev.base, 0x050, 0); // kick the receive queue so the device knows the buffers are there
        Ok(())
    }
}

pub struct Received {
    pub len: usize,
    /// The frame's ethertype, after the 10-byte virtio header.
    pub ethertype: u16,
    /// The sender's MAC, so a reply can go back to the machine that actually asked — not to a synthesised address that
    /// the emulated network will silently drop.
    pub src_mac: [u8; 6],
    /// The first bytes of the payload, so a caller can identify what arrived.
    pub head: [u8; 4],
}

/// Poll for one inbound frame, bounded. Returns `None` if nothing arrived in the budget: a quiet network is not an
/// error, and a boot must not hang waiting for a packet that may never come.
pub fn poll_rx(dev: &Device, budget: u32) -> Option<Received> {
    let _ = dev;
    unsafe {
        let rings = &raw const RINGS_RX as *const u8;
        let used = rings.add(USED_OFF);
        let mut spins = 0u32;
        loop {
            let idx = core::ptr::read_volatile((used as *const u16).add(1));
            if idx != RX_LAST_SEEN {
                RX_LAST_SEEN = idx;
                let id = core::ptr::read_volatile((used as *const u32).add(RS_USED_ENTRY)) as usize;
                let written = core::ptr::read_volatile((used as *const u32).add(RS_USED_ENTRY + 1)) as usize;
                if id < RX_BUFS && written > 10 {
                    let buf = (&raw const RX_BUFFERS as *const u8).add(id * RX_BUF_LEN);
                    // The device's 10-byte virtio header precedes the frame, exactly as on transmit.
                    let frame = buf.add(10);
                    let ethertype = ((core::ptr::read_volatile(frame.add(12)) as u16) << 8)
                        | core::ptr::read_volatile(frame.add(13)) as u16;
                    let mut head = [0u8; 4];
                    for i in 0..4 {
                        head[i] = core::ptr::read_volatile(frame.add(14 + i));
                    }
                    let mut src_mac = [0u8; 6];
                    for i in 0..6 {
                        src_mac[i] = core::ptr::read_volatile(frame.add(6 + i));
                    }
                    return Some(Received { len: written - 10, ethertype, src_mac, head });
                }
            }
            spins += 1;
            if spins > budget {
                return None;
            }
        }
    }
}

// A used-ring entry is { u32 id; u32 len; } and begins 4 bytes into the used ring (after flags and idx). As u32
// indices that is 1 and 2 — measured: with the base at 2 the driver read `len` as the descriptor id and byte 12 as the
// length, so a completed frame looked like an impossible descriptor and was silently discarded.
const RS_USED_ENTRY: usize = 1;
