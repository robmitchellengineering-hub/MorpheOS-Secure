//! The GIC — rung M5b's interrupt half.
//!
//! Two interfaces, gated by feature. QEMU's `virt` with a cortex-a76 defaults to **GICv2** (memory-mapped GICC); the
//! Pi 5's GIC-600 is **GICv3** (the CPU interface moved into `ICC_*` system registers). The `gic-v3` feature selects
//! the system-register interface, which is the Pi 5's real one, emulated by QEMU's `-machine gic-version=3`.
//!
//! THE ADDRESSES (GICv2): distributor 0x0800_0000, CPU interface 0x0801_0000. (GICv3): distributor 0x0800_0000,
//! redistributor 0x080A_0000. The EL1 physical timer is **PPI 30** in both.

use core::arch::asm;

const GICD_BASE: usize = 0x0800_0000;
#[cfg(not(feature = "gic-v3"))]
const GICC_BASE: usize = 0x0801_0000;
#[cfg(feature = "gic-v3")]
const GICR_BASE: usize = 0x080A_0000;

const GICD_CTLR: usize = GICD_BASE + 0x000;
#[cfg(not(feature = "gic-v3"))]
const GICD_ISENABLER0: usize = GICD_BASE + 0x100;
#[cfg(not(feature = "gic-v3"))]
const GICD_IPRIORITYR: usize = GICD_BASE + 0x400;
#[cfg(not(feature = "gic-v3"))]
const GICC_CTLR: usize = GICC_BASE + 0x000;
#[cfg(not(feature = "gic-v3"))]
const GICC_PMR: usize = GICC_BASE + 0x004;
#[cfg(not(feature = "gic-v3"))]
const GICC_IAR: usize = GICC_BASE + 0x00c;
#[cfg(not(feature = "gic-v3"))]
const GICC_EOIR: usize = GICC_BASE + 0x010;
#[cfg(feature = "gic-v3")]
const GICR_CTLR: usize = GICR_BASE + 0x000;
#[cfg(feature = "gic-v3")]
const GICR_WAKER: usize = GICR_BASE + 0x014;
#[cfg(feature = "gic-v3")]
const GICR_ISENABLER0: usize = GICR_BASE + 0x10000 + 0x100; // SGI page
#[cfg(feature = "gic-v3")]
const GICR_IPRIORITYR: usize = GICR_BASE + 0x10000 + 0x400; // SGI page, byte-addressed
#[cfg(feature = "gic-v3")]
const GICR_IGROUPR0: usize = GICR_BASE + 0x10000 + 0x080; // SGI page

/// The EL1 physical timer's PPI.
pub const TIMER_INTID: u32 = 30;

static mut TICKS: u64 = 0;

pub fn ticks() -> u64 {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(TICKS)) }
}

#[cfg(not(feature = "gic-v3"))]
/// Bring the GICv2 up and enable the timer's PPI on this CPU.
pub fn init() {
    unsafe {
        let write = |addr: usize, v: u32| core::ptr::write_volatile(addr as *mut u32, v);
        write(GICC_PMR, 0xff);
        write(GICC_CTLR, 1);
        write(GICD_CTLR, 1); // EnableGrp0 for SPIs
        write(GICD_IPRIORITYR + 28, 0x0000_0000);
        write(GICD_ISENABLER0, 1 << TIMER_INTID);
        let _ = core::ptr::read_volatile(GICD_ISENABLER0 as *const u32);
    }
}

#[cfg(feature = "gic-v3")]
/// Bring the GICv3 up and enable the timer's PPI on this CPU.
pub fn init() {
    unsafe {
        let write = |addr: usize, v: u32| core::ptr::write_volatile(addr as *mut u32, v);
        let sre: u64;
        asm!("mrs {0}, icc_sre_el1", out(reg) sre, options(nostack));
        asm!("msr icc_sre_el1, {0}", in(reg) sre | 1, options(nostack));
        asm!("isb", options(nostack));
        asm!("msr icc_pmr_el1, {0}", in(reg) 0xffu64, options(nostack));
        asm!("msr icc_igrpen0_el1, {0}", in(reg) 1u64, options(nostack));
        write(GICR_WAKER, 0);
        write(GICR_CTLR, 1); // EnableGrp0 — the timer PPI is group 0, delivered as FIQ
        write(GICR_IPRIORITYR + 28, 0x0000_0000); // INTIDs 28..31 to priority 0 (highest) — 0xFF default is below the PMR
        write(GICR_ISENABLER0, 1 << TIMER_INTID);
        write(GICD_CTLR, 1); // EnableGrp0 for SPIs
        let _ = core::ptr::read_volatile(GICR_ISENABLER0 as *const u32);
    }
}

/// Unmask IRQs on this CPU.
pub fn unmask() {
    unsafe { asm!("msr daifclr, #6", options(nostack)) }; // clear I (IRQ) and F (FIQ) — the timer PPI is a FIQ in GICv3
}

/// Acknowledge, count, re-arm, end-of-interrupt.
pub unsafe fn handle() -> u32 {
    #[cfg(not(feature = "gic-v3"))]
    let intid = unsafe { core::ptr::read_volatile(GICC_IAR as *const u32) } & 0x3ff;
    #[cfg(feature = "gic-v3")]
    let intid = {
        let v: u64;
        unsafe { asm!("mrs {0}, icc_iar0_el1", out(reg) v, options(nostack)) };
        (v & 0xffff_ffff) as u32 & 0x3ff
    };

    if intid == TIMER_INTID {
        let n = ticks() + 1;
        unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(TICKS), n) };
        crate::clock::arm_in(crate::clock::ticks_for_ms(1));
        if n == 1 {
            let uart = crate::uart::Uart::at(crate::uart::UART_QEMU_VIRT);
            uart.write_line("IRQ: the first timer interrupt arrived — the Core was interrupted, not polled");
        }
        if n == 1000 && !crate::sched::switching() {
            let uart = crate::uart::Uart::at(crate::uart::UART_QEMU_VIRT);
            uart.write_line("IRQ: STORM — 1000 interrupts, disabling the timer so the boot can continue");
            crate::clock::disarm();
        }
    }
    #[cfg(not(feature = "gic-v3"))]
    unsafe { core::ptr::write_volatile(GICC_EOIR as *mut u32, intid) };
    #[cfg(feature = "gic-v3")]
    unsafe { asm!("msr icc_eoir0_el1, {0}", in(reg) intid as u64, options(nostack)) };
    intid
}
