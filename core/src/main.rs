//! The Core of MorpheOS Secure — rung M0, on the architecture it is actually for.
//!
//! The target is a **Raspberry Pi 5**: AArch64, Cortex-A76. That is why there is no real-mode stub, no multiboot
//! header and no long-mode switch here — those are x86 rituals and the Pi has none of them. The firmware (or EDK2
//! UEFI) hands the CPU an AArch64 image at EL1/EL2 with the MMU off, and the Core takes it from there.
//!
//! M0's claim is small and testable: **the Core runs on AArch64 and says what it is**, with no Linux or any other
//! operating system underneath. The falsifier is `scripts/m0.sh`: it boots this image on QEMU's `virt` machine — the
//! same architecture as the Pi, so the code is the Pi's code — and requires the exact banner on the serial line,
//! after first booting a deliberately wrong banner to prove the check can fail.
//!
//! NOT DONE HERE, stated rather than implied: no MMU, no GIC, no timers, no per-core bring-up, and no Pi 5 hardware.
//! QEMU's `virt` gives a UART and RAM; the Pi brings a different UART address, an EEPROM bootloader and real
//! firmware. This file is the seed of that, not the thing itself.

#![no_std]
#![no_main]
#![deny(clippy::undocumented_unsafe_blocks)]

use core::arch::global_asm;
use core::panic::PanicInfo;

extern crate alloc;

#[path = "alloc.rs"]
mod heap;
mod cap;
mod ledger;
mod uart;
use cap::{access, CapTable, Object, ObjectId};
use ledger::Ledger;

#[global_allocator]
static ALLOCATOR: heap::FirstFit = heap::FirstFit::new();
use uart::{Uart, UART_QEMU_VIRT};

/// The line M0 asserts. One place, so the test and the Core cannot disagree.
pub const BANNER: &str = "MORPHEOS-CORE 0.0.1: the Core is running on aarch64; no Linux, no syscalls, no processes";
pub const DONE: &str = "MORPHEOS-CORE: reported; halting";

// The entry point, in assembly, because there is no stack until it makes one. QEMU starts us at EL1 in AArch64 with
// the MMU off; `sp` is not guaranteed to point anywhere useful, so that is the whole of the first instruction's job,
// and it is the only assembly the Core needs on this rung.
global_asm!(
    r#"
    .section .text.entry, "ax"
    .global _start
_start:
    /* The stack the linker script reserved. AArch64 requires sp to stay 16-byte aligned whenever it is touched. */
    adrp x0, stack_top
    add  x0, x0, :lo12:stack_top
    mov  sp, x0

    /* Interrupts off. On this rung nothing should raise one, and an unexpected one must not become a silent halt. */
    msr daifset, #0xf

    /* A vector table, so a fault says WHAT faulted instead of leaving a machine that stopped with no words. Measured
     * need: the Core died silently after the M1 verdict, which could be a hang or a fault and there was no way to
     * tell them apart. VBAR_EL1 must be 2 KiB aligned. */
    adrp x0, vectors
    add  x0, x0, :lo12:vectors
    msr vbar_el1, x0

    bl   kmain
1:  wfe
    b    1b

    .balign 2048
vectors:
    .rept 16
    .balign 128
    mrs x0, esr_el1
    mrs x1, far_el1
    mrs x2, elr_el1
    b   exc_report
    .endr
"#
);

/// The Core's own failure path: name the fault, with the registers an engineer needs to act on it.
///
/// `esr` is the exception syndrome (what kind of fault), `far` the faulting address where the architecture provides
/// one, and `elr` the instruction that was executing. Printing all three is the difference between "it stopped" and
/// "it faulted reading 0x… at 0x…", and it costs a handful of instructions on a path that has already gone wrong.
#[unsafe(no_mangle)]
pub extern "C" fn exc_report(esr: u64, far: u64, elr: u64) -> ! {
    let uart = Uart::at(UART_QEMU_VIRT);
    uart.init();
    uart.write_line("MORPHEOS-CORE: EXCEPTION — the Core faulted, and this is it reporting rather than stopping");

    let ec = (esr >> 26) & 0x3f;
    uart.write_str("  class: ");
    uart.write_line(match ec {
        0x00 => "undefined instruction",
        0x01 => "trapped WFI or WFE",
        0x0e => "illegal execution state",
        0x15 => "SVC (a syscall on a machine that has none)",
        0x20 | 0x21 => "instruction abort (lower/current EL)",
        0x22 => "PC alignment fault",
        0x24 | 0x25 => "data abort (lower/current EL)",
        0x26 => "SP alignment fault",
        0x2c => "trapped floating-point",
        _ => "other — see the syndrome below",
    });
    uart.write_str("  ESR_EL1: ");
    uart.write_hex(esr);
    uart.write_str("  FAR_EL1: ");
    uart.write_hex(far);
    uart.write_str("  ELR_EL1: ");
    uart.write_hex(elr);
    uart.write_line("");
    if ec == 0x24 || ec == 0x25 {
        // DFSC in the low six bits: 0b100001..0b100111 are alignment faults, and they are the common one with the MMU
        // off (Device memory forbids unaligned access).
        let dfsc = esr & 0x3f;
        if (0x21..=0x27).contains(&dfsc) {
            uart.write_line("  cause: ALIGNMENT FAULT — the access was not aligned for its size (Device memory forbids unaligned)");
        } else {
            uart.write_line("  cause: data abort — see FAR above for the address that faulted");
        }
    }
    loop {
        // SAFETY: nothing else to do after a fault on this rung.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let uart = Uart::at(UART_QEMU_VIRT);
    uart.init();
    uart.write_line("MORPHEOS-CORE: PANIC — the Core failed before it could report");
    if let Some(location) = info.location() {
        uart.write_str("  at ");
        uart.write_line(location.file());
    }
    loop {
        // SAFETY: as above — a panicked core has nothing to do but wait.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}
