//! The GIC, and the IRQ that makes a tick a tick — rung M5b's second half.
//!
//! THE CLAIM: the Core is **interrupted** by the timer, not merely able to read it. Everything a real-time class needs
//! — preemption, a priority class, "audio beats everything else" — needs an interrupt to exist first.
//!
//! THE MACHINE, stated as addresses: QEMU `virt` with a cortex-a76 gives **GICv3**. The CPU interface is system
//! registers (`ICC_*`), the redistributor is memory-mapped at 0x080A_0000 (one per CPU, this machine has one), and a
//! timer interrupt is a **PPI**: the EL1 physical timer is INTID **30**, private to that CPU, enabled in the
//! redistributor rather than the distributor.
//!
//! WHAT THIS IS NOT: no priorities beyond "all the same", no preemption of a running task (the tick only counts), no
//! multiple CPUs. The next rung uses the tick; this one proves it exists.

use core::arch::asm;

const GICR_BASE: usize = 0x080A_0000;
const GICR_WAKER: usize = GICR_BASE + 0x0014;
const GICR_IGROUPR0: usize = GICR_BASE + 0x0080;
const GICR_ISENABLER0: usize = GICR_BASE + 0x0100;

/// The EL1 physical timer's PPI.
pub const TIMER_INTID: u32 = 30;

/// Interrupts taken so far. Written only by the IRQ handler, read by anyone.
static mut TICKS: u64 = 0;

pub fn ticks() -> u64 {
    // SAFETY: a single writer (the IRQ handler) and reads of a u64 are atomic on this machine.
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(TICKS)) }
}

/// Wake this CPU's redistributor, route the timer PPI to this CPU's interrupt group, and enable it.
pub fn init() {
    // SAFETY: these are the GICv3 registers for this machine, and this runs once with interrupts off.
    unsafe {
        // Enable system-register access to the CPU interface.
        let mut sre: u64;
        asm!("mrs {0}, icc_sre_el1", out(reg) sre, options(nostack));
        sre |= 1;
        asm!("msr icc_sre_el1, {0}", in(reg) sre, options(nostack));
        asm!("isb", options(nostack));

        // Allow every priority, and enable group 1 (the group PPIs arrive in).
        asm!("msr icc_pmr_el1, {0}", in(reg) 0xffu64, options(nostack));
        asm!("msr icc_igrpen1_el1, {0}", in(reg) 1u64, options(nostack));
        asm!("isb", options(nostack));

        // Redistributor: clear ProcessorSleep, wait for ChildrenAsleep to clear.
        let mut waker = core::ptr::read_volatile(GICR_WAKER as *const u32);
        waker &= !(1 << 1);
        core::ptr::write_volatile(GICR_WAKER as *mut u32, waker);
        while core::ptr::read_volatile(GICR_WAKER as *const u32) & (1 << 2) != 0 {
            core::hint::spin_loop();
        }

        // The timer PPI is group 1 (bit set in IGROUPR0) and enabled (bit set in ISENABLER0).
        let group = core::ptr::read_volatile(GICR_IGROUPR0 as *const u32);
        core::ptr::write_volatile(GICR_IGROUPR0 as *mut u32, group | (1 << TIMER_INTID));
        core::ptr::write_volatile(GICR_ISENABLER0 as *mut u32, 1 << TIMER_INTID);
    }
}

/// Unmask IRQs on this CPU. Without this the GIC is configured and nothing ever arrives.
pub fn unmask() {
    // SAFETY: clearing DAIF.I at EL1 is exactly "take interrupts".
    unsafe { asm!("msr daifclr, #2", options(nostack)) };
}

/// Acknowledge the interrupt, count it, and re-arm the timer. Returns the INTID for the log.
///
/// # Safety
/// Called only from the IRQ vector entry, with the GIC initialised.
pub unsafe fn handle() -> u32 {
    // SAFETY: reading IAR1 acknowledges the highest-priority group-1 interrupt.
    let intid: u64;
    unsafe { asm!("mrs {0}, icc_iar1_el1", out(reg) intid, options(nostack)) };
    let intid = (intid & 0xffff_ffff) as u32;
    if intid == TIMER_INTID {
        // SAFETY: single writer.
        unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(TICKS), ticks() + 1) };
        // Re-arm for another 1 ms so the stream of ticks continues while the test watches.
        crate::clock::arm_in(crate::clock::ticks_for_ms(1));
    }
    // SAFETY: EOI for the interrupt just acknowledged.
    unsafe { asm!("msr icc_eoir1_el1, {0}", in(reg) intid as u64, options(nostack)) };
    intid
}
