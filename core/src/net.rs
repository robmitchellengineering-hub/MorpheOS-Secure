//! The network device, brought up far enough to send one frame.
//!
//! WHAT IS PROVEN HERE: a descriptor ring in our own memory, a buffer the device can read, a kick, and a used-ring
//! entry that comes back — then one Ethernet/ARP frame that the HOST side captures. The pcap either contains our bytes
//! or it does not; there is no "seems to work" available.
//!
//! WHAT IS NOT HERE: a receive path that queues into anything, a protocol stack, checksums, retries. This is the
//! smallest honest milestone: the wire saw us.
//!
//! THE LAYOUT IS THE LEGACY ONE (version 1, measured): descriptor table, available ring and used ring in one contiguous
//! region whose page frame number is written to `QueuePFN`, with `QueueAlign` set to the page size. Modern virtio puts
//! the three rings in three addresses and needs a shared-memory handshake; that is not what this device speaks.

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
        wr(dev.base, 0x030, 0); // QueueSel = 0, the receive queue
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
        let pfn = (rings as usize) / 4096;
        wr(dev.base, 0x040, pfn as u32); // QueuePFN: where the rings live
        Ok(())
    }
}

/// Send one frame. The buffer is reused, so the caller's bytes must fit in 64.
pub fn transmit(dev: &Device, frame: &[u8]) -> Result<u32, &'static str> {
    if frame.len() > 64 {
        return Err("frame too large for this milestone's buffer");
    }
    unsafe {
        let rings = &raw mut RINGS as *mut u8;
        let buf = &raw mut FRAME as *mut u8;
        for (i, b) in frame.iter().enumerate() {
            core::ptr::write_volatile(buf.add(i), *b);
        }
        // Descriptor 0: the buffer, its length, no chaining.
        let desc = rings;
        core::ptr::write_volatile(desc as *mut u64, buf as u64);
        core::ptr::write_volatile((desc as *mut u32).add(2), frame.len() as u32);
        core::ptr::write_volatile((desc as *mut u16).add(6), 0); // flags
        core::ptr::write_volatile((desc as *mut u16).add(7), 0); // next
        // Available ring: flags, idx, ring[0] = descriptor 0.
        let avail = rings.add(AVAIL_OFF);
        core::ptr::write_volatile(avail as *mut u16, 0);
        core::ptr::write_volatile((avail as *mut u16).add(1), 1); // one descriptor offered
        core::ptr::write_volatile((avail as *mut u16).add(2), 0);
        // Kick the queue.
        wr(dev.base, 0x050, 0);

        // Poll the used ring for the completion. Bounded: a device that never answers must not hang the boot.
        let used = rings.add(USED_OFF);
        let mut spins = 0u32;
        loop {
            let used_idx = core::ptr::read_volatile((used as *const u16).add(1));
            if used_idx >= 1 {
                let written = core::ptr::read_volatile((used as *const u32).add(2)); // first entry's length
                return Ok(written);
            }
            spins += 1;
            if spins > 10_000_000 {
                return Err("the device never completed the descriptor");
            }
        }
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
