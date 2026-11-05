//! The GIC — rung M5b's interrupt half.
//!
//! MEASURED BEFORE WRITTEN: the machine's own device tree says `arm,gic-v2m-frame`, so QEMU's `virt` with a
//! cortex-a76 gives **GICv2**, not GICv3. That is why the first version faulted with *"undefined instruction"* at the
//! first `mrs icc_sre_el1`: the `ICC_*` system registers do not exist on a GICv2 machine. This file speaks GICv2's
//! memory-mapped interface instead, which is the interface the machine actually has.
//!
//! THE ADDRESSES: distributor at 0x0800_0000, CPU interface at 0x0801_0000. The EL1 physical timer is **PPI 30**, a
//! per-CPU interrupt enabled through the distributor's `ISENABLER0` (PPIs live in the low 32 bits).
//!
//! WHAT THIS IS NOT: no priorities beyond "all the same", no preemption of a running task, no second CPU. The tick
//! only counts; the rung that uses it schedules.

use core::arch::asm;

const GICD_BASE: usize = 0x0800_0000;
const GICC_BASE: usize = 0x0801_0000;

const GICD_CTLR: usize = GICD_BASE + 0x000;
const GICD_ISENABLER0: usize = GICD_BASE + 0x100;
const GICD_IPRIORITYR: usize = GICD_BASE + 0x400;
const GICC_CTLR: usize = GICC_BASE + 0x000;
const GICC_PMR: usize = GICC_BASE + 0x004;
const GICC_IAR: usize = GICC_BASE + 0x00c;
const GICC_EOIR: usize = GICC_BASE + 0x010;

/// The EL1 physical timer's PPI.
pub const TIMER_INTID: u32 = 30;

static mut TICKS: u64 = 0;

pub fn ticks() -> u64 {
    // SAFETY: one writer (the IRQ handler); a u64 load is atomic on this machine.
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(TICKS)) }
}

/// Bring the GICv2 up and enable the timer's PPI on this CPU.
pub fn init() {
    // SAFETY: these are the GICv2 registers for this machine; this runs once with interrupts masked.
    unsafe {
        let read = |addr: usize| core::ptr::read_volatile(addr as *const u32);
        let write = |addr: usize, v: u32| core::ptr::write_volatile(addr as *mut u32, v);

        // CPU interface: allow every priority, then enable it.
        write(GICC_PMR, 0xff);
        write(GICC_CTLR, 1);

        // Distributor: enable it, then give the timer PPI the highest priority and enable it.
        write(GICD_CTLR, 1);
        // IPRIORITYR is byte-addressed; PPI 30 is byte 30.
        write(GICD_IPRIORITYR + 28, 0x0000_0000); // covers INTIDs 28..31
        write(GICD_ISENABLER0, 1 << TIMER_INTID);

        // A read back, so the writes are not reordered past the enable in a way that matters.
        let _ = read(GICD_ISENABLER0);
    }
}

/// Unmask IRQs on this CPU.
pub fn unmask() {
    // SAFETY: clearing DAIF.I at EL1 is exactly "take interrupts". DAIFClr takes an immediate.
    unsafe { asm!("msr daifclr, #2", options(nostack)) };
}

/// Acknowledge, count, re-arm, end-of-interrupt.
///
/// # Safety
/// Called only from the IRQ vector entry, with the GIC initialised.
pub unsafe fn handle() -> u32 {
    // SAFETY: GICC_IAR acknowledges the highest-priority interrupt; GICC_EOIR ends it.
    let intid = unsafe { core::ptr::read_volatile(GICC_IAR as *const u32) } & 0x3ff;
    if intid == TIMER_INTID {
        let n = ticks() + 1;
        // SAFETY: single writer.
        unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(TICKS), n) };

        // RE-ARM BEFORE THE EOI, deliberately. The timer's interrupt is level-triggered on its condition, so if the
        // condition is not cleared before the interrupt is ended, the GIC asserts it again immediately and the CPU
        // never returns to what it was doing - an interrupt storm that looks exactly like a hang at unmask, which is
        // what the boot showed. Order matters, and it is stated here because the symptom is invisible.
        crate::clock::arm_in(crate::clock::ticks_for_ms(1));

        if n == 1 {
            let uart = crate::uart::Uart::at(crate::uart::UART_QEMU_VIRT);
            uart.write_line("IRQ: the first timer interrupt arrived — the Core was interrupted, not polled");
        }
        // The storm guard is for a REAL storm: interrupts firing with no switching enabled. Measured: the preemption
        // demo legitimately runs past 1000 ticks (one second) under node load, and the guard disarmed the timer mid-demo,
        // killing the very preemption it was measuring - task A then never finished. A storm only looks like a storm
        // when nothing is being switched; once switching is on, ticks are progress, not a hang.
        if n % 100 == 0 {
            let uart = crate::uart::Uart::at(crate::uart::UART_QEMU_VIRT);
            let mut l = crate::uart::LineBuf::new();
            l.push_str("TICK ");
            l.push_u64(n as u64);
            l.push_str(", switching ");
            l.push_u64(crate::sched::switching() as u64);
            l.flush(&uart);
        }
        if n == 1000 && !crate::sched::switching() {
            let uart = crate::uart::Uart::at(crate::uart::UART_QEMU_VIRT);
            uart.write_line("IRQ: STORM — 1000 interrupts, disabling the timer so the boot can continue");
            crate::clock::disarm();
        }
    }
    // SAFETY: EOI for the interrupt just acknowledged.
    unsafe { core::ptr::write_volatile(GICC_EOIR as *mut u32, intid) };
    intid
}
