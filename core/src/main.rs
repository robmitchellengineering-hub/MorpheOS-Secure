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

    /* ---- the multiboot header. The loader searches for this magic in the first 8 KiB and nothing else ---- */
    .section .multiboot, "a"
    .align 4
    .long 0x1BADB002
    .long 0x00000000
    .long -(0x1BADB002 + 0)

    .section .text
    .code32
    .global _start
_start:
    cli

    /* OUR GDT, not the loader's: a 64-bit code segment and a flat data segment. The loader's GDT is not ours to
     * rely on, and after the far jump below nothing of the loader's is left in the path. */
    lgdt [gdt64_ptr]

    /* Identity-map the first 1 GiB with 2 MiB pages: pml4[0] -> pdpt[0] -> pd[0..512]. Nothing above 1 GiB exists
     * on rung M0b, and a Core that maps memory it has not asked for is a Core that lies about what it owns. */
    mov edi, offset pd
    mov eax, 0x83                 /* present | writable | 2 MiB page */
    mov ecx, 512
1:  mov [edi], eax
    add eax, 0x200000
    add edi, 8
    loop 1b

    mov dword ptr [pdpt], offset pd
    or  dword ptr [pdpt], 3
    mov dword ptr [pdpt + 4], 0

    mov dword ptr [pml4], offset pdpt
    or  dword ptr [pml4], 3
    mov dword ptr [pml4 + 4], 0

    /* PAE, then the long-mode enable bit, then paging — the order the CPU requires. */
    mov eax, cr4
    or  eax, 0x20                 /* CR4.PAE */
    mov cr4, eax

    mov eax, offset pml4
    mov cr3, eax

    mov ecx, 0xC0000080           /* EFER */
    rdmsr
    or  eax, 0x100                /* EFER.LME */
    wrmsr

    mov eax, cr0
    or  eax, 0x80000000           /* CR0.PG */
    mov cr0, eax

    /* A far jump with the 64-bit selector: the only way into 32/64 mode, encoded by hand because the assembler's
     * mnemonic for it differs between syntaxes and this must not be a guess. */
    .byte 0xEA
    .long long_mode
    .word 0x08

    .code64
long_mode:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    /* The stack the linker script gave us. After this point everything is Rust, and the loader's only remaining
     * role is that it read the file. */
    lea rsp, [rip + stack_top]
    call kmain

2:  hlt
    jmp 2b

    /* The page tables the stub builds. Zeroed by the loader because they are part of the image's memory size. */
    .section .bss
    .align 4096
pml4:  .skip 4096
pdpt:  .skip 4096
pd:    .skip 4096

    .section .data
    .align 8
gdt64:
    .quad 0
    .quad 0x00AF9A000000FFFF      /* 64-bit code, present, DPL0, readable */
    .quad 0x00CF92000000FFFF      /* flat data, present, writable */
gdt_end:
gdt64_ptr:
    .word gdt_end - gdt64 - 1
    .long gdt64
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
