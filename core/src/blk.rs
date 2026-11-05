//! Storage: read a sector.
//!
//! WHAT IS PROVEN: the Core issues a virtio-blk read request — a 3-descriptor chain (16-byte request header the device
//! reads, a 512-byte data buffer the device writes, a 1-byte status the device writes) — and reads a real sector back.
//!
//! THE REQUEST LAYOUT (modern virtio-blk, little-endian): { type u32; reserved u32; sector u64 }, where type 0 is IN
//! (read) and 1 is OUT (write). The status byte after the data is 0 = OK.

const AVAIL_OFF: usize = 0x1000;
const USED_OFF: usize = 0x2000;

#[repr(C, align(4096))]
struct Rings {
    bytes: [u8; 0x3000],
}

static mut RINGS: Rings = Rings { bytes: [0u8; 0x3000] };
static mut REQUEST: [u8; 16] = [0u8; 16];
static mut DATA: [u8; 512] = [0u8; 512];
static mut STATUS: u8 = 0;

pub struct Blk {
    pub base: usize,
}

unsafe fn rd(base: usize, off: usize) -> u32 {
    core::ptr::read_volatile((base + off) as *const u32)
}
unsafe fn wr(base: usize, off: usize, v: u32) {
    core::ptr::write_volatile((base + off) as *mut u32, v);
}

/// Bring queue 0 up with three descriptors, the modern way (version 2, measured with force-legacy=false).
pub fn start_queue(dev: &Blk) -> Result<(), &'static str> {
    unsafe {
        wr(dev.base, 0x030, 0); // QueueSel = 0
        wr(dev.base, 0x038, 3); // QueueNum: three descriptors per request
        wr(dev.base, 0x03c, 4096);
        let rings = &raw mut RINGS as *mut u8;
        for i in 0..0x3000 {
            core::ptr::write_volatile(rings.add(i), 0);
        }
        let version = rd(dev.base, 0x004);
        if version >= 2 {
            let d = rings as u64;
            let a = (rings as u64) + AVAIL_OFF as u64;
            let u = (rings as u64) + USED_OFF as u64;
            wr(dev.base, 0x080, d as u32);
            wr(dev.base, 0x084, (d >> 32) as u32);
            wr(dev.base, 0x090, a as u32);
            wr(dev.base, 0x094, (a >> 32) as u32);
            wr(dev.base, 0x0a0, u as u32);
            wr(dev.base, 0x0a4, (u >> 32) as u32);
            wr(dev.base, 0x044, 1); // QueueReady
        } else {
            wr(dev.base, 0x040, ((rings as usize) / 4096) as u32); // legacy QueuePFN
        }
        Ok(())
    }
}

/// Read one 512-byte sector. Returns the device's status byte (0 = OK).
pub fn read_sector(dev: &Blk, sector: u64, out: &mut [u8; 512]) -> Result<u8, &'static str> {
    unsafe {
        static mut AVAIL_IDX: u16 = 0;

        // Request header: type 0 (read), reserved 0, sector (little-endian u64).
        for i in 0..4 {
            core::ptr::write_volatile((core::ptr::addr_of_mut!(REQUEST) as *mut u8).add(i), 0); // type = 0 (IN/read), reserved
        }
        for i in 0..8 {
            core::ptr::write_volatile((core::ptr::addr_of_mut!(REQUEST) as *mut u8).add(8 + i), ((sector >> (8 * i)) & 0xff) as u8);
        }

        // Three chained descriptors: header (device reads), data (device writes), status (device writes).
        let rings = &raw mut RINGS as *mut u8;
        let desc = rings;
        // desc 0 -> header
        core::ptr::write_volatile(desc as *mut u64, (&raw mut REQUEST as *mut u8) as u64);
        core::ptr::write_volatile((desc as *mut u32).add(2), 16);
        core::ptr::write_volatile((desc as *mut u16).add(6), 1); // NEXT
        core::ptr::write_volatile((desc as *mut u16).add(7), 1); // next = 1
        // desc 1 -> data
        let d1 = desc.add(16);
        core::ptr::write_volatile(d1 as *mut u64, (out.as_mut_ptr()) as u64);
        core::ptr::write_volatile((d1 as *mut u32).add(2), 512);
        core::ptr::write_volatile((d1 as *mut u16).add(6), 3); // NEXT | WRITE
        core::ptr::write_volatile((d1 as *mut u16).add(7), 2); // next = 2
        // desc 2 -> status
        let d2 = desc.add(32);
        core::ptr::write_volatile(d2 as *mut u64, (&raw mut STATUS as *mut u8) as u64);
        core::ptr::write_volatile((d2 as *mut u32).add(2), 1);
        core::ptr::write_volatile((d2 as *mut u16).add(6), 2); // WRITE
        core::ptr::write_volatile((d2 as *mut u16).add(7), 0);

        // Available ring: monotonic index, head descriptor 0.
        AVAIL_IDX = AVAIL_IDX.wrapping_add(1);
        if AVAIL_IDX == 0 {
            AVAIL_IDX = 1;
        }
        let avail = rings.add(AVAIL_OFF);
        core::ptr::write_volatile(avail as *mut u16, 0);
        core::ptr::write_volatile((avail as *mut u16).add(1), AVAIL_IDX);
        core::ptr::write_volatile((avail as *mut u16).add(2), 0); // head = descriptor 0

        wr(dev.base, 0x050, 0); // kick queue 0

        let used = rings.add(USED_OFF);
        let mut spins = 0u32;
        loop {
            let used_idx = core::ptr::read_volatile((used as *const u16).add(1));
            if used_idx >= AVAIL_IDX {
                return Ok(core::ptr::read_volatile(&raw const STATUS as *const u8));
            }
            spins += 1;
            if spins > 10_000_000 {
                return Err("the device never completed the read");
            }
        }
    }
}
