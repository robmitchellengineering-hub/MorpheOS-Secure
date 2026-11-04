//! The MMU — our own page tables, rung M3a.
//!
//! THE CLAIM THIS FILE EXISTS TO MAKE: memory is mapped by us, and an address we did not map is **not reachable**.
//! That is the difference between M1's refusal (the Core declining to hand over an object) and real containment: with
//! the MMU on, a task cannot name memory it was not given, because naming it faults.
//!
//! THE MAP, stated as a decision rather than a default:
//!   - `0x0000_0000 .. 0x4000_0000`  — **Device** memory, because inside it is the PL011 at 0x0900_0000 and other
//!     MMIO. Mapping MMIO as Normal is the classic way to get silently coalesced writes and a UART that never sends.
//!   - `0x4000_0000 .. 0x5000_0000`  — **Normal** memory, write-back: this is QEMU's 256 MiB of RAM, which is where
//!     the Core, its stack, its arena and the ledger live.
//!   - `0x5000_0000` — **unmapped**. The guard. Nothing lives past the end of RAM, so if something touches it, the
//!     machine should say so rather than read a zero and carry on believing it owns memory it does not.
//!
//! Two 4 KiB tables do the whole job: an L1 with two 1 GiB entries, and one L2 per GiB of 2 MiB blocks. That is
//! enough to be a real translation regime (TCR, MAIR, TTBR0, SCTLR) and small enough to read in one sitting.
//!
//! WHAT IT IS NOT YET: no per-task address spaces (every task sees this one map), no permissions per page beyond
//! Device/Normal, no executable-never for data, no ASID. Those arrive with tasks, and the file says so.

use core::arch::asm;

/// 2 MiB blocks, 512 of them per L2 table = 1 GiB covered per table.
const BLOCK: u64 = 2 * 1024 * 1024;

const DESC_VALID: u64 = 1 << 0;
const DESC_TABLE: u64 = 1 << 1; // for L1 entries pointing at an L2 table
const DESC_BLOCK: u64 = 0; // level 2 block, indicated by bit 1 == 0
const DESC_AF: u64 = 1 << 10; // access flag — without it the first access faults
const DESC_SH_NONE: u64 = 0;
const DESC_SH_INNER: u64 = 3 << 8;
const DESC_ATTR0: u64 = 0 << 2; // MAIR index 0: Device
const DESC_ATTR1: u64 = 1 << 2; // MAIR index 1: Normal

#[repr(align(4096))]
pub struct Table(pub [u64; 512]);

static mut L1: Table = Table([0; 512]);
static mut L2_DEVICE: Table = Table([0; 512]); // 0x0000_0000 .. 0x4000_0000
static mut L2_RAM: Table = Table([0; 512]); // 0x4000_0000 .. 0x8000_0000

/// Build the identity map described in the module note. Called once, before anything else touches memory that matters.
pub fn build() {
    // SAFETY: these tables are static, aligned to 4 KiB, and this runs once before the MMU is on.
    unsafe {
        let l1 = &mut *core::ptr::addr_of_mut!(L1);
        let device = &mut *core::ptr::addr_of_mut!(L2_DEVICE);
        let ram = &mut *core::ptr::addr_of_mut!(L2_RAM);

        // L1[0] -> the Device table (0..1 GiB), L1[1] -> the RAM table (1..2 GiB).
        l1.0[0] = core::ptr::addr_of!(L2_DEVICE) as u64 | DESC_VALID | DESC_TABLE;
        l1.0[1] = core::ptr::addr_of!(L2_RAM) as u64 | DESC_VALID | DESC_TABLE;

        // Device: every 2 MiB block of the first GiB, non-shareable, MAIR index 0.
        for i in 0..512 {
            device.0[i] = (i as u64 * BLOCK) | DESC_VALID | DESC_BLOCK | DESC_AF | DESC_ATTR0 | DESC_SH_NONE;
        }

        // RAM: 256 MiB of Normal memory at 0x4000_0000, inner-shareable, MAIR index 1, write-back.
        let ram_blocks = 256 * 1024 * 1024 / BLOCK; // 128 blocks
        for i in 0..ram_blocks {
            ram.0[i as usize] = (0x4000_0000 + i * BLOCK)
                | DESC_VALID
                | DESC_BLOCK
                | DESC_AF
                | DESC_ATTR1
                | DESC_SH_INNER;
        }

        // The guard: 0x5000_0000 (block 128 of the RAM table) is deliberately left zero — invalid. Measured: with
        // `mmu-mutant-map-guard`, this one line is what the mutation turns on, and the fault must then disappear.
        if cfg!(feature = "mmu-mutant-map-guard") {
            ram.0[ram_blocks as usize] =
                0x4000_0000 + ram_blocks * BLOCK | DESC_VALID | DESC_BLOCK | DESC_AF | DESC_ATTR1 | DESC_SH_INNER;
        }
    }
}

/// Turn the MMU on with the tables `build` made. After this, every address the Core reads or writes is translated.
///
/// # Safety
/// Only correct after `build`, on one core, with the identity map covering the code currently executing.
pub unsafe fn enable() {
    // MAIR: index 0 = Device-nGnRnE (0x00), index 1 = Normal write-back read/write-allocate (0xFF).
    const MAIR: u64 = 0xff_00;
    // TCR: 4 KiB granule, 4 GiB of address space in TTBR0 (T0SZ=32), write-back, inner-shareable, 40-bit physical.
    const TCR: u64 = (32u64) | (0 << 6) | (1 << 8) | (3 << 10) | (1 << 12) | (3 << 14) | (2 << 32) | (0 << 38);

    let ttbr0 = core::ptr::addr_of!(L1) as u64;
    // SAFETY: the caller guarantees `build` ran and the identity map covers this code. The sequence is the one the
    // architecture requires: MAIR/TCR/TTBR, then TLBI, then SCTLR with the MMU bit set, then ISB.
    unsafe {
        asm!("msr mair_el1, {0}", in(reg) MAIR, options(nostack));
        asm!("msr tcr_el1, {0}", in(reg) TCR, options(nostack));
        asm!("msr ttbr0_el1, {0}", in(reg) ttbr0, options(nostack));
        asm!("dsb ish", "isb");
        asm!("tlbi vmalle1", "dsb ish", "isb");
        let mut sctlr: u64;
        asm!("mrs {0}, sctlr_el1", out(reg) sctlr, options(nostack));
        sctlr |= 1; // M: the MMU itself
        sctlr |= 1 << 2; // C: data cacheable (with Normal memory mapped)
        asm!("msr sctlr_el1, {0}", in(reg) sctlr, options(nostack));
        asm!("isb");
    }
}
