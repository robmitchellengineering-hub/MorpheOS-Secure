//! The Core of MorpheOS Secure — rung M0.
//!
//! WHAT THIS IS, AND WHAT IT IS NOT. This is the first thing in this repository that runs. It has no Linux under
//! it, no syscalls, no processes and no filesystem: it is a bare-metal program the machine loads and runs, and its
//! whole job on this rung is to reach userspace-with-nothing-below-it and **say what the machine is**.
//!
//! The falsifier is the point: `scripts/m0.sh` boots this in QEMU and fails unless the exact line below appears on
//! the serial port — and it also boots a build whose line is deliberately wrong, to prove the check can fail.
//!
//! WHAT IS OURS NOW: the loader (`loader/loader.S`) places this image at 2 MiB, builds the page tables, selects a
//! 64-bit code segment and jumps here. What still belongs to whatever put the *loader* in memory is just the file
//! read — and rung M0b-2 moves that, with the two generations and the one-shot trial, into our loader too.

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

// The Core's entry point. It is the first byte of the image (`.text.entry` in linker.ld), because `morph-loader`
// copies this blob to 2 MiB and jumps to that address with a 64-bit code segment already selected and the page
// tables already on. Everything before this point is the loader's job, and the loader is ours: `loader/loader.S`.
//
// The stack is ours too — the loader promises a stack pointer but not a stack we own, so the first instruction here
// switches to the one the linker script reserved.
global_asm!(
    r#"
    .intel_syntax noprefix
    .section .text.entry, "ax"
    .global _start
_start:
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
