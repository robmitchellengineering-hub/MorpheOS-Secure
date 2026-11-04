//! The Core of MorpheOS Secure — rung M0.
//!
//! WHAT THIS IS, AND WHAT IT IS NOT. This is the first thing in this repository that runs. It has no Linux under
//! it, no syscalls, no processes and no filesystem: it is a bare-metal program the machine loads and runs, and its
//! whole job on this rung is to reach userspace-with-nothing-below-it and **say what the machine is**.
//!
//! The falsifier is the point: `scripts/m0.sh` boots this in QEMU and fails unless the exact line below appears on
//! the serial port — and it also boots a build whose line is deliberately wrong, to prove the check can fail.
//!
//! WHAT IS NOT OURS YET, stated rather than implied: on M0 the loader is QEMU's multiboot loader. Writing our own
//! loader (`morph-loader`) is rung M0b, with its own falsifier, because a loader with no Core to load would be a
//! solution to the wrong problem first.

#![no_std]
#![no_main]
// The Core is not allowed to have a kernel-shaped interface. These are refused by construction as rungs land; on M0
// there is nothing to refuse yet, and saying so here beats pretending the absence of `unsafe` elsewhere.
#![deny(clippy::undocumented_unsafe_blocks)]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

/// The line M0 asserts. Kept in one place so the test and the Core cannot disagree.
pub const BANNER: &str = "MORPHEOS-CORE 0.0.1: the Core is running; no Linux, no syscalls, no processes";

/// QEMU's `isa-debug-exit` device. Writing here ends the emulator with `(value << 1) | 1`, which is how a bare
/// program returns a verdict to a test harness without an operating system to return to.
const QEMU_EXIT_PORT: u16 = 0xf4;
const QEMU_EXIT_OK: u32 = 0x10;
const QEMU_EXIT_FAIL: u32 = 0x11;

mod serial;
use serial::Serial;

// The multiboot header, and the entry point. Both must be assembly: the loader looks for a magic number in the
// first 8 KiB of the image before any Rust exists, and it jumps to a symbol with no stack promised.
global_asm!(
    r#"
    .intel_syntax noprefix

    .section .multiboot, "a"
    .align 4
    .long 0x1BADB002          /* magic the loader searches for */
    .long 0x00000000          /* flags: nothing requested, not even memory info */
    .long -(0x1BADB002 + 0)   /* checksum */

    .section .text
    .global _start
_start:
    /* A loader guarantees nothing about the stack; give ourselves ours from the linker script before any Rust. */
    lea rsp, [rip + stack_top]
    call kmain
1:  hlt
    jmp 1b
"#
);

/// The Core's first instruction in Rust.
///
/// # Safety
/// Called once by `_start` with a stack the linker script provided. It never returns.
#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    let mut serial = Serial::new();
    serial.write_line(BANNER);
    serial.write_line("MORPHEOS-CORE: no capability space yet, no ledger yet — those are rungs M1 and M2");
    halt(QEMU_EXIT_OK)
}

fn halt(code: u32) -> ! {
    // SAFETY: `QEMU_EXIT_PORT` is the isa-debug-exit device QEMU is told to attach (see scripts/m0.sh). On a
    // machine without it this writes to an unused port, which is harmless — and there is no other way for a bare
    // program to report an exit code.
    unsafe {
        asm!("out dx, eax", in("dx") QEMU_EXIT_PORT, in("eax") code, options(nomem, nostack, preserves_flags));
    }
    loop {
        // SAFETY: `hlt` is legal at CPL 0 and this loop has nothing else to do if the exit device is absent.
        unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let mut serial = Serial::new();
    serial.write_line("MORPHEOS-CORE: PANIC — the Core failed before it could report");
    if let Some(location) = info.location() {
        serial.write_line(location.file());
    }
    halt(QEMU_EXIT_FAIL)
}
