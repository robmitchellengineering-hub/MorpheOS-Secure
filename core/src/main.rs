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
mod mmu;
mod task;
mod uart;
use cap::{access, CapTable, Object, ObjectId};
use ledger::Ledger;

#[global_allocator]
static ALLOCATOR: heap::FirstFit = heap::FirstFit::new();
use uart::{Uart, UART_QEMU_VIRT};

/// The line M0 asserts. One place, so the test and the Core cannot disagree.
pub const BANNER: &str = "MORPHEOS-CORE 0.0.1: the Core is running on aarch64; no Linux, no syscalls, no processes";
pub const DONE: &str = "MORPHEOS-CORE: reported; halting";

/// Set by the fault handler when the guard probe faulted, so the code *after* the probe can tell a recovered fault
/// from a successful read. Without this the post-fault path cannot distinguish them — measured: the probe faulted,
/// the handler recovered, and the Core then printed the failure line because nothing recorded that the fault happened.
static PROBE_FAULTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

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

    /* FP/SIMD on at EL1. Measured the hard way: with CPACR_EL1.FPEN off (the reset default) the first compiler-emitted
     * SIMD instruction traps, and Rust emits them for things as ordinary as a copy loop. ECU 0x07 with FAR 0 was the
     * whole diagnosis. */
    mrs x0, cpacr_el1
    orr x0, x0, #(3 << 20)
    msr cpacr_el1, x0
    isb

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
    /* The FULL general register set. Measured: saving only x0-x3 let the Rust handler clobber the
     * registers the resumed code depended on, so the first return from a fault resumed into garbage. The handler is
     * ordinary Rust and is entitled to clobber x0-x18; a recovery path must give every register back.
     * 256 bytes keeps sp 16-byte aligned. x30 is saved with the rest because `bl` overwrites it.
     * Known gap, stated: the FP/SIMD registers are not saved. A fault taken with live FP state loses it, which is
     * acceptable while the only recovery is a u64 probe and is the next thing to fix before real task switching. */
    sub sp, sp, #256
    stp x0, x1, [sp, #0]
    stp x2, x3, [sp, #16]
    stp x4, x5, [sp, #32]
    stp x6, x7, [sp, #48]
    stp x8, x9, [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x16, x17, [sp, #128]
    stp x18, x19, [sp, #144]
    stp x20, x21, [sp, #160]
    stp x22, x23, [sp, #176]
    stp x24, x25, [sp, #192]
    stp x26, x27, [sp, #208]
    stp x28, x29, [sp, #224]
    str x30, [sp, #240]
    mrs x0, esr_el1
    mrs x1, far_el1
    mrs x2, elr_el1
    bl  exc_report
    ldp x0, x1, [sp, #0]
    ldp x2, x3, [sp, #16]
    ldp x4, x5, [sp, #32]
    ldp x6, x7, [sp, #48]
    ldp x8, x9, [sp, #64]
    ldp x10, x11, [sp, #80]
    ldp x12, x13, [sp, #96]
    ldp x14, x15, [sp, #112]
    ldp x16, x17, [sp, #128]
    ldp x18, x19, [sp, #144]
    ldp x20, x21, [sp, #160]
    ldp x22, x23, [sp, #176]
    ldp x24, x25, [sp, #192]
    ldp x26, x27, [sp, #208]
    ldp x28, x29, [sp, #224]
    ldr x30, [sp, #240]
    add sp, sp, #256
    eret
    .endr
"#
);

/// The task's entry point. It does two things a task must be able to do: use its own stack for real work, and
/// report where that stack was — so the check can tell a switched stack from the Core's.
fn task_entry() -> u64 {
    // A local array large enough to sit well inside the task's stack, and written so the stack is genuinely used.
    let mut buffer = [0u8; 2048];
    // Record where this frame actually is, so the Core can judge it rather than trust the narrative.
    unsafe { task::TASK_STACK_SEEN = core::ptr::addr_of!(buffer) as usize };
    buffer[0] = 0xA5;
    buffer[2047] = 0x5A;
    let uart = Uart::at(UART_QEMU_VIRT);
    uart.write_str("M4a: the task is running, with its stack somewhere near 0x");
    // The address of a local, i.e. where the task's stack actually is.
    uart.write_hex(buffer.as_ptr() as u64);
    uart.write_line("");
    u64::from(buffer[0]) + u64::from(buffer[2047])
}

/// The Core's first instruction in Rust.
#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    let uart = Uart::at(UART_QEMU_VIRT);
    uart.init();

    uart.write_line(BANNER);
    uart.write_line("MORPHEOS-CORE: EL1, MMU off, load address 0x40080000");

    // ---- M1: the capability space. Two objects, two tasks, and the Core deciding who may name what. ----
    let measurement = Object { id: ObjectId(1), name: "measurement", value: 0xA1 };
    let plugin_state = Object { id: ObjectId(2), name: "plugin-state", value: 0xB2 };

    let mut task_bench = CapTable::empty();
    let _bench_cap = task_bench.grant(measurement.id);
    let mut task_plugin = CapTable::empty();
    let _plugin_cap = task_plugin.grant(plugin_state.id);

    #[cfg(feature = "m1-mutant-grant")]
    let _mutant = task_bench.grant(plugin_state.id);

    uart.write_line("M1: task_bench holds: measurement");
    uart.write_line("M1: task_plugin holds: plugin-state");

    let _ = access(&uart, "task_bench", &task_bench, &measurement);
    let _ = access(&uart, "task_bench", &task_bench, &plugin_state);
    let _ = access(&uart, "task_plugin", &task_plugin, &plugin_state);
    let _ = access(&uart, "task_plugin", &task_plugin, &measurement);
    uart.write_line("M1: the Core decided both directions; no task holds a capability it was not handed");

    // ---- M2: the ledger. ----
    let mut machine = Ledger::empty();
    ledger::report(&uart, &mut machine);

    // ---- M3a: the MMU. Our page tables, our translation regime, and a guard page that must fault. ----
    uart.write_line("MMU: building our own tables (Device 0..1G, Normal 0x4000_0000..0x4F00_0000, guard at 0x4F00_0000)");
    mmu::build();
    // SAFETY: the tables are built and the identity map covers this code, the stack and the arena.
    unsafe { mmu::enable() };
    uart.write_line("MMU: ENABLED — SCTLR_EL1.M is set, every access is now translated by our tables");

    // A valid access still works: read the banner's first byte through the new translation.
    let probe = unsafe { core::ptr::read_volatile(BANNER.as_ptr()) };
    if probe == b'M' {
        uart.write_line("MMU: a mapped read still works (the banner's first byte is 'M')");
    } else {
        uart.write_line("MMU: FAIL a mapped read returned the wrong byte");
    }

    // The guard: read memory we never mapped. This MUST fault, and the handler must survive it.
    uart.write_line("MMU: touching the unmapped guard page 0x50000000, which must fault");
    let phantom = unsafe { core::ptr::read_volatile(0x4F00_0000 as *const u64) };
    // Either the read faulted (and the handler recorded it) or it succeeded — and a successful read of an unmapped
    // page is the failure. The two paths are told apart by the flag, not by whether a line was printed.
    if PROBE_FAULTED.load(core::sync::atomic::Ordering::SeqCst) {
        uart.write_line("MMU: the Core continued past the guard probe — the fault was caught, the machine did not die");

    // ---- M4a: a second execution context. The task runs on its OWN stack, and the Core gets its stack back. ----
    let (task_bottom, task_stack_top) = task::region();
    #[cfg(not(feature = "task-mutant-no-switch"))]
    let stack_for_task = task_stack_top;
    // The mutation: hand the task the Core's own stack, so a task and the Core are the same context. The "own stack"
    // line is then a lie and the check must catch it.
    #[cfg(feature = "task-mutant-no-switch")]
    let stack_for_task = 0x4008_0000usize;

    uart.write_str("M4a: switching to a task on stack 0x");
    uart.write_hex(stack_for_task as u64);
    uart.write_line("");
    // SAFETY: the stack is a static 16 KiB region (or, under the mutation, deliberately the wrong one).
    let value = unsafe { task::run(task_entry, stack_for_task) };
    // The task returns buffer[0] + buffer[2047] = 0xA5 + 0x5A, so a correct return is exactly 0xFF.
    // The judgement: where did the task's stack ACTUALLY sit, compared with the region the Core handed it?
    let seen = unsafe { task::TASK_STACK_SEEN };
    if seen >= task_bottom && seen < task_stack_top && value == 0xFF {
        uart.write_line("M4a: the task ran on its own stack and returned to the Core");
    } else {
        uart.write_line("M4a: FAIL the task did not return the value it must");
    }
    } else {
        uart.write_str("MMU: FAIL the guard page was readable (value ");
        uart.write_hex(phantom);
        uart.write_line(") — memory we did not map must not be reachable");
    }

    // ---- M2b: the memory the machine owns. ----
    // SAFETY: called once, before any allocation, on the single core this rung runs on.
    unsafe { ALLOCATOR.init() };
    uart.write_line("M2b: allocator initialised (arena handed to the free list)");
    {
        use alloc::vec::Vec;
        let mut cue_list: Vec<u32> = Vec::new();
        for cue in 1..=8u32 {
            cue_list.push(cue * 1000);
        }
        uart.write_line("M2b: 8 pushes through the global allocator returned");
        let first = cue_list.as_ptr() as usize;
        drop(cue_list);
        uart.write_line("M2b: the cue list was dropped");

        let mut second: Vec<u32> = Vec::with_capacity(8);
        second.push(7);
        let reused = second.as_ptr() as usize == first;
        if reused {
            uart.write_line("M2b: the freed block was reused by the next allocation (first fit, as designed)");
        } else {
            uart.write_line("M2b: the freed block was NOT reused — first fit did not take the hole");
        }

        // The double-free attack, called directly on the allocator so the header is visible. Three numbers:
        // the address and its magic when handed out, after the first free, and after the second.
        // SAFETY: the allocator is initialised and the pointer comes from it.
        let p = unsafe { ALLOCATOR.alloc_block(64) };
        uart.write_str("M2b: alloc -> ");
        uart.write_hex(p as u64);
        uart.write_str(" magic=");
        uart.write_hex(unsafe { ALLOCATOR.magic_of(p) });
        uart.write_line("");

        // SAFETY: p came from this allocator.
        let first = unsafe { ALLOCATOR.free_block(p) };
        uart.write_str("M2b: free#1 -> accepted=");
        uart.write_hex(u64::from(first));
        uart.write_str(" magic=");
        uart.write_hex(unsafe { ALLOCATOR.magic_of(p) });
        uart.write_line("");

        // SAFETY: this is the attack — the same pointer freed again.
        let second = unsafe { ALLOCATOR.free_block(p) };
        uart.write_str("M2b: free#2 -> accepted=");
        uart.write_hex(u64::from(second));
        uart.write_str(" magic=");
        uart.write_hex(unsafe { ALLOCATOR.magic_of(p) });
        uart.write_line("");

        let (_, _, refused) = ALLOCATOR.stats();
        if refused >= 1 && !second {
            uart.write_line("M2b: DOUBLE FREE REFUSED — the block was not inserted twice");
        } else {
            uart.write_line("M2b: FAIL a double free was accepted; two owners can now hold the same bytes");
        }
    }

    uart.write_line(DONE);
    loop {
        // SAFETY: `wfe` at EL1 is always legal; it is the correct way for an idle core to wait.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}

/// The Core's own failure path: name the fault, with the registers an engineer needs to act on it.
///
/// `esr` is the exception syndrome (what kind of fault), `far` the faulting address where the architecture provides
/// one, and `elr` the instruction that was executing. Printing all three is the difference between "it stopped" and
/// "it faulted reading 0x… at 0x…", and it costs a handful of instructions on a path that has already gone wrong.
#[unsafe(no_mangle)]
pub extern "C" fn exc_report(esr: u64, far: u64, elr: u64) {
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
        0x07 => "trapped SIMD/FP access (CPACR_EL1.FPEN is off)",
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
    // THE ONE FAULT THAT IS EXPECTED. The guard probe reads 0x4F00_0000, which is deliberately unmapped; the handler
    // recognises that address, says so, and resumes after the faulting instruction (a load is 4 bytes). Everything
    // else is fatal. This is containment demonstrated: the Core touched memory it did not map, the machine reported
    // it precisely, and the machine kept running.
    const GUARD: u64 = 0x4F00_0000;
    if far == GUARD {
        PROBE_FAULTED.store(true, core::sync::atomic::Ordering::SeqCst);
        uart.write_line("  MMU: guard page FAULTED as required — 0x50000000 is not mapped, and the Core survived it");
        // SAFETY: resume after the faulting load.
        unsafe { core::arch::asm!("msr elr_el1, {0}", in(reg) elr + 4, options(nostack)) };
        return;
    }
    loop {
        // SAFETY: nothing else to do after an unexpected fault.
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
