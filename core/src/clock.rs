//! The machine's clock — the counter every deadline is measured against, rung M5a.
//!
//! THE CLAIM: the Core can read a monotonic counter that the hardware maintains, know its frequency from the hardware
//! itself, and meet a deadline measured in that counter. That is the foundation of a real-time class: AirMatrix's
//! transport needs deadlines, its `thread-priority` dependency says audio threads must be scheduled by one, and the
//! audio path is a chain of them.
//!
//! WHERE THE NUMBERS COME FROM: `CNTFRQ_EL0` (the counter's frequency, programmed by the firmware) and `CNTPCT_EL0`
//! (the physical counter). Both are architectural, so the Pi 5 and QEMU both provide them — unlike a board-specific
//! timer block, which is why this is the right first clock.
//!
//! WHAT THIS IS NOT: an interrupt. This is a counter you read and a loop that spins until a deadline; a *tick* — a
//! timer that interrupts the Core and drives preemption — needs the GIC and an IRQ handler, and that is the next rung.

use core::arch::asm;

/// The counter's frequency in Hz, as the hardware reports it.
pub fn frequency() -> u64 {
    let freq: u64;
    // SAFETY: CNTFRQ_EL0 is readable at EL1 on every AArch64 machine.
    unsafe { asm!("mrs {0}, cntfrq_el0", out(reg) freq, options(nostack)) };
    freq
}

/// The monotonic counter. On this rung "time" is exactly this number.
pub fn now() -> u64 {
    // The mutation: a clock that does not advance. Every deadline check must then fail, so the falsifier can prove it
    // is reading the hardware rather than a constant.
    if cfg!(feature = "tick-mutant-frozen") {
        return 0;
    }
    let ticks: u64;
    // SAFETY: CNTPCT_EL0 is readable at EL1.
    unsafe { asm!("mrs {0}, cntpct_el0", out(reg) ticks, options(nostack)) };
    ticks
}

/// Ticks for `ms` milliseconds, at the frequency the hardware reports.
pub fn ticks_for_ms(ms: u64) -> u64 {
    frequency() / 1000 * ms
}

/// Nanoseconds elapsed between two counter readings — the unit a latency budget is stated in.
pub fn ns_between(start: u64, end: u64) -> u64 {
    let freq = frequency();
    if freq == 0 {
        return 0;
    }
    (end.saturating_sub(start)) * 1_000_000_000 / freq
}

/// Arm the EL1 physical timer to fire once, `ticks` from now. Polled only: this proves the comparator fires, which is
/// the half of a tick that has nothing to do with the GIC.
pub fn arm_in(ticks: u64) {
    let tval = if cfg!(feature = "tick-mutant-armed-far") {
        // The mutation: arm far enough in the future that the deadline cannot arrive within the test. A "tick" that
        // reports firing under this is not reading the comparator.
        1 << 60
    } else {
        ticks
    };
    // SAFETY: CNTP_TVAL_EL0 and CNTP_CTL_EL0 are the EL1 physical timer's own registers.
    unsafe {
        asm!("msr cntp_tval_el0, {0}", in(reg) tval, options(nostack));
        asm!("msr cntp_ctl_el0, {0}", in(reg) 1u64, options(nostack)); // ENABLE=1, IMASK=0
        asm!("isb", options(nostack));
    }
}

/// Has the armed timer fired? Reads the comparator's own status, not a flag this code maintains.
pub fn fired() -> bool {
    let ctl: u64;
    // SAFETY: CNTP_CTL_EL0 bit 2 is ISTATUS, read-only, set when the timer has fired.
    unsafe { asm!("mrs {0}, cntp_ctl_el0", out(reg) ctl, options(nostack)) };
    ctl & 0b100 != 0
}

/// Disarm the timer, so it cannot keep pending.
pub fn disarm() {
    // SAFETY: writing 0 to CNTP_CTL_EL0 clears ENABLE.
    unsafe { asm!("msr cntp_ctl_el0, {0}", in(reg) 0u64, options(nostack)) };
}

/// Spin until `deadline`. Returns the counter reading when it was reached.
pub fn wait_until(deadline: u64) -> u64 {
    loop {
        let t = now();
        if t >= deadline {
            return t;
        }
        core::hint::spin_loop();
    }
}
