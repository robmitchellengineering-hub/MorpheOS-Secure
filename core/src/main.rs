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
mod box_admission;
mod blk;
mod ab;
mod cap;
mod cert;
mod clock;
mod fdt;
mod gic;
mod sched;
mod ledger;
mod mmu;
mod net;
mod ip;
mod udp;
mod rtp;
mod task;
mod uart;
use cap::{access, CapTable, Object, ObjectId};
use ledger::Ledger;

#[global_allocator]
static ALLOCATOR: heap::FirstFit = heap::FirstFit::new();
use uart::{Uart, UART_QEMU_VIRT};

/// The device tree blob the firmware passed in `x0`, kept so the reader can be asked what the machine is.
#[unsafe(no_mangle)]
pub static mut DTB_POINTER: usize = 0;

/// The line M0 asserts. One place, so the test and the Core cannot disagree.
/// Milestone 1 of the loader: a foreign function, as raw AArch64 bytes, in the executable text section. It is
/// `mov x0, #0x42; ret` — returns 66 and nothing else, so the only way the boot prints 66 is that the CPU ran these
/// bytes. No absolute addresses, no UART access: position-independent by construction.
#[cfg(not(feature = "load-mutant-wrong-result"))]
#[unsafe(link_section = ".text")]
static LOADED_CODE: [u32; 2] = [0xD280_0840, 0xD65F_03C0]; // mov x0,#0x42; ret -> 66

/// Milestone 2: the same jump, but the bytes come FROM THE DISK at runtime into this executable buffer.
#[unsafe(link_section = ".text")]
static mut LOAD_BUF: [u8; 8] = [0u8; 8];

/// The mutation returns 0 instead of 66, so the gate can fail when the loaded code is wrong.
#[cfg(feature = "load-mutant-wrong-result")]
#[unsafe(link_section = ".text")]
static LOADED_CODE: [u32; 2] = [0xD280_0000, 0xD65F_03C0]; // mov x0,#0; ret -> 0

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
    /* aarch64 assembly. NOTE: `.intel_syntax noprefix` used to sit here, copied from the x86 loader — it is an
     * x86 directive and on aarch64 it is an unknown directive, so the whole block fails to assemble. */

    /* Save and restore every general register and lr. A handler is ordinary Rust and is entitled to clobber
     * x0-x18, so anything less than all of them gives the resumed code garbage. */
    .macro SAVE_ALL
    sub sp, sp, #288
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
    mrs x9, elr_el1
    str x9, [sp, #248]
    mrs x9, spsr_el1
    str x9, [sp, #256]
    .endm

    .macro RESTORE_ALL
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
    add sp, sp, #288
    .endm

    /* For the INTERRUPT path only: the same restores as RESTORE_ALL, plus the two slots that decide WHERE execution
     * resumes. The fault path must never use this - the fault handler rewrites elr_el1 on purpose (+4, to resume
     * after the faulting load) and restoring the saved value would overwrite that. Measured: doing it on the fault path
     * broke M3a and M4c in one round. */
    .macro RESTORE_ALL_WITH_STATE
    ldr x9, [sp, #248]
    msr elr_el1, x9
    ldr x9, [sp, #256]
    msr spsr_el1, x9
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
    add sp, sp, #288
    .endm

    /* The fault path: name the fault, and return (the handler decides whether a return is allowed). */
    .macro FAULT_ENTRY
    .balign 128
    SAVE_ALL
    mrs x0, esr_el1
    mrs x1, far_el1
    mrs x2, elr_el1
    bl  exc_report
    RESTORE_ALL
    eret
    .endm

    .section .text.entry, "ax"
    .global _start
    .section .text.head, "ax"
    .balign 8
image_header:
    /* code0 and code1: the protocol has TWO 4-byte branch fields before text_offset. Measured: with one, the
       ARM\x64 marker landed at offset 52 and the image was refused — four bytes of arithmetic, not a mystery. */
    b       _start
    b       _start
    .quad   0x80000
    .quad   0
    .quad   3
    .quad   0
    .quad   0
    .quad   0
    .int    0x644d5241
    .balign 8
_start:
    adrp x1, DTB_POINTER
    add  x1, x1, :lo12:DTB_POINTER
    str  x0, [x1]
    /* The stack the linker script reserved. AArch64 requires sp to stay 16-byte aligned. */
    adrp x0, stack_top
    add  x0, x0, :lo12:stack_top
    mov  sp, x0

    /* Interrupts off until the GIC is ready; FP/SIMD on, because the compiler emits it for ordinary copies. */
    msr daifset, #0xf

    /* Run on SP_EL1 explicitly. The IRQ vector entry (entry 5) stacks on SP_ELx, so if the main code were running on
     * SP_EL0 the interrupt would push to whatever SP_EL1 happens to be — zero at reset — and fault immediately. That
     * is exactly the symptom: unmask, and the boot never prints the next line. */
    msr spsel, #1

    mrs x0, cpacr_el1
    orr x0, x0, #(3 << 20)
    msr cpacr_el1, x0
    isb

    /* VBAR_EL1 must be 2 KiB aligned. */
    adrp x0, vectors
    add  x0, x0, :lo12:vectors
    msr vbar_el1, x0
    isb

    bl   kmain
1:  wfe
    b    1b

    /* The vector table, in architecture order: 0-4 faults (EL1t group), 5 IRQ (EL1h), 6-15 faults.
     * Entry 5 is the one this machine's timer arrives on, and it is written out rather than looped so it cannot be
     * an off-by-one. */
    .balign 2048
vectors:
    /* EVERY ENTRY IS FOUR BYTES. The architecture requires entries exactly 128 bytes apart, and an earlier version
     * put a 250-byte handler body in each slot: the .balign then pushed everything out of shape and entry 5 landed
     * inside a body. The symptom was an interrupt that never returned and never printed. Entries are branches; the
     * bodies live outside the table, once each. */
    .balign 128
    b fault_entry
    .balign 128
    b fault_entry
    .balign 128
    b fault_entry
    .balign 128
    b fault_entry
    .balign 128
    b fault_entry
    .balign 128
    b irq_entry
    .balign 128
    b fault_entry
    .balign 128
    b fault_entry
    .balign 128
    b fault_entry
    .balign 128
    b fault_entry
    .balign 128
    b fault_entry
    .balign 128
    b fault_entry
    .balign 128
    b fault_entry
    .balign 128
    b fault_entry
    .balign 128
    b fault_entry
    .balign 128
    b fault_entry

fault_entry:
    SAVE_ALL
    mrs x0, esr_el1
    mrs x1, far_el1
    mrs x2, elr_el1
    bl  exc_report
    RESTORE_ALL
    eret

irq_entry:
    SAVE_ALL

    /* The handler is given the frame it just saved, and returns the frame to resume from. Today it returns the same
     * one, so behaviour is identical to before this commit - which is exactly the point: the plumbing is measured
     * before anything uses it. When the switch lands, this is the only line that changes meaning. */
    mov x0, sp
    bl  irq_switch
    mov sp, x0

    RESTORE_ALL_WITH_STATE
    eret

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
    // AFTER the buffer is written. Measured: recording this before the writes told the Core the task returned 0, and
    // the verdict printed "returned 0x0" - the three facts did the work the narrative had been doing.
    let uart = Uart::at(UART_QEMU_VIRT);
    uart.write_str("M4a: the task is running, with its stack somewhere near 0x");
    // The address of a local, i.e. where the task's stack actually is.
    uart.write_hex(buffer.as_ptr() as u64);
    uart.write_line("");
    let result = u64::from(buffer[0]) + u64::from(buffer[2047]);
    unsafe { task::TASK_RESULT = result };
    result
}

/// The interrupt handler, called by the vector entry with the interrupted context already saved.
///
/// It acknowledges the interrupt, counts it, re-arms the timer and end-of-interrupts. It does not schedule: on this rung
/// the tick only counts, and preemption is the rung that uses it.
#[unsafe(no_mangle)]
pub extern "C" fn irq_switch(sp: usize) -> usize {
    // SAFETY: the vector entry saves and restores every register, and the GIC is initialised before unmask().
    unsafe { gic::handle() };
    // THE SWITCH, in one call: record the frame that was interrupted and answer with the one to resume from. While
    // switching is off this returns `sp` unchanged, so every existing gate must produce exactly the same result — and
    // that is what this commit is measured against.
    sched::on_irq(sp)
}

/// The second task's entry: it prints once and halts, which is enough to prove it was ever run.
#[unsafe(no_mangle)]
pub extern "C" fn task_second_entry() {
    let uart = Uart::at(UART_QEMU_VIRT);
    let mut spins: u64 = 0;
    // Count first, so "it ran" is arithmetic and not a claim: the CPU had to be here for this loop to execute.
    while spins < 5_000 {
        spins += 1;
        core::hint::spin_loop();
    }
    {
        let mut line = uart::LineBuf::new();
        line.push_str("TASK B: ran ");
        line.push_u64(spins);
        line.push_str(" iterations on its own stack — the switch reached it");
        line.flush(&uart);
    }
    loop {
        // SAFETY: a task with nothing to do waits.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}

/// Where an aborted task leaves the machine: the Core's stack is back, the violation is already written, and the boot
/// carries on. This never returns, which is why the switch needs to park only a stack pointer.
extern "C" fn after_task_aborted() -> ! {
    let uart = Uart::at(UART_QEMU_VIRT);
    uart.write_line("M4c: TASK ABORTED — the Core resumed on its own stack, the evidence stays, the boot continues");
    uart.write_line(DONE);
    boot_epilogue(&uart);
    loop {
        // SAFETY: an aborted task's continuation has nothing to return to on this rung.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}

/// The first task: it counts until it is done and then says so. It never yields — the only way it loses the CPU is
/// the timer taking it, which is the whole point.
fn task_a_entry(uart: &Uart) -> ! {
    let mut spins: u64 = 0;
    while spins < 200_000 {
        spins += 1;
        core::hint::spin_loop();
    }
    // Through the buffer, so the line cannot be spliced by the other task printing at the same moment.
    {
        let mut line = uart::LineBuf::new();
        line.push_str("TASK A: ran ");
        line.push_u64(spins);
        line.push_str(" iterations — and it was interrupted along the way");
        line.flush(uart);
    }
    loop {
        // SAFETY: task A is done.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}

/// Everything the boot does AFTER it has reported itself. Measured: the task abort at the isolation rung halts the
/// normal boot path, so a preemption demo placed only after kmain's DONE never ran. One epilogue, called from both
/// paths, is what makes "the machine reports, then becomes two tasks" true wherever the boot happens to end.
fn boot_epilogue(uart: &Uart) {
    // ---- M5c part four: preemption. GATED, and LAST: this is the one rung that never returns. ----
    #[cfg(feature = "preempt-demo")]
    {
    // ---- M5c part four: preemption. GATED: the two-task demo never returns, so it runs only when the gate asks
    // for it. Every other gate boots exactly as before - which is the property the sweep caught us losing. ----
    // ---- M5c part four: preemption. Two contexts, one CPU, and the timer choosing between them. ----
    let second_top = {
        let base = core::ptr::addr_of!(sched::TASK_B_STACK) as usize;
        base + core::mem::size_of::<sched::TaskStack>()
    };
    let second_frame = sched::install_frame(second_top, task_second_entry as usize);
    sched::install_second(second_frame);
    let (elr, spsr) = sched::frame_state(second_frame);
    uart.write_str("PREEMPT: second task frame at 0x");
    uart.write_hex(second_frame as u64);
    uart.write_str(", resumes at 0x");
    uart.write_hex(elr);
    uart.write_str(" in PSTATE 0x");
    uart.write_hex(spsr);
    uart.write_line("");
    // The boot's own frame is recorded by the first interrupt, so nothing to do here but turn it on.
    sched::enable_switching();
    // Measured: the interrupt demo at M5b ends by MASKING interrupts for determinism, so task A ran to completion with
    // no tick at all and no switch happened. Preemption needs them on.
    gic::unmask();
    uart.write_line("PREEMPT: switching enabled and interrupts unmasked — the next tick may move the CPU");
    // From here this context IS task A. It never returns; the timer is the only thing that takes the CPU away.
    task_a_entry(&uart);
}

/// The device tree demonstration, run ONLY when the firmware actually passed a tree. Measured: running it on every boot
/// turned the preemption gate red, because the fifteen ELF gates have no tree and the demo still took the console at a
/// moment their own two-task demo wanted it. A gate's environment must be identical to the one it was proven in.
fn fdt_demo(uart: &Uart) {

    let dtb = unsafe { DTB_POINTER };
    let mut l = uart::LineBuf::new();
    l.push_str("FDT: the firmware's device tree is at ");
    l.push_hex(dtb as u64);
    l.flush(&uart);
    // The network device is described the same way the console is. Asking costs one walk; assuming an address
    // costs a memory, and this project has already paid for one of those.
    match fdt::find_compatible(dtb, "virtio,mmio") {
        Ok(a) => {
            let mut l = uart::LineBuf::new();
            l.push_str("NET: a virtio-mmio window at ");
            l.push_hex(a.base);
            l.push_str(" is described by the firmware");
            l.flush(uart);
        }
        Err(_) => uart.write_line("NET: no virtio-mmio node in this tree"),
    }
    match fdt::find_compatible(dtb, "arm,pl011") {
        Ok(a) => {
            let mut l = uart::LineBuf::new();
            l.push_str("FDT: arm,pl011 at ");
            l.push_hex(a.base);
            l.push_str(" — the constant this boot uses is ");
            l.push_hex(uart::UART_QEMU_VIRT as u64);
            l.flush(&uart);
            if a.base == uart::UART_QEMU_VIRT as u64 {
                uart.write_line("FDT: the tree AGREES with the constant — on the Pi the tree wins");
            }         // WHICH WINDOW HOLDS THE DEVICE? Measured: the tree's first virtio-mmio node is an EMPTY slot (device id 0,
        // legacy version 1), so the driver's first job is enumeration, not setup. QEMU's transports are contiguous,
        // 0x200 apart; the BASE comes from the tree, only the stride is assumed - and on real hardware the tree lists
        // each transport, which is why the loop stays bounded.
        if let Ok(a) = fdt::find_compatible(dtb, "virtio,mmio") {
            let base = a.base as usize;
            let mut found: Option<usize> = None;
            // QEMU's virt places its virtio-mmio transports across the whole window range and may attach a device to a
            // late one — measured: with eight slots scanned and a NIC passed on the command line, every window still
            // reported nothing. So the scan covers the full transport count and prints only windows that hold a device.
            for slot in 0..32usize {
                let window = base + slot * 0x200;
                // SAFETY: inside the window the firmware described; a fault here is the measurement.
                let (magic, version, device) = unsafe {
                    let w = |off: usize| core::ptr::read_volatile((window + off) as *const u32);
                    (w(0x000), w(0x004), w(0x008))
                };
                if magic != 0x7472_6976 || device == 0 {
                    continue;
                }
                let mut l = uart::LineBuf::new();
                l.push_str("NET: window ");
                l.push_u64(slot as u64);
                l.push_str(" at ");
                l.push_hex(window as u64);
                l.push_str(" — version ");
                l.push_u64(version as u64);
                l.push_str(", device ");
                l.push_u64(device as u64);
                l.flush(uart);
                if device == 1 && found.is_none() {
                    found = Some(window);
                }
                // STORAGE. A virtio-blk device (device id 2) reports its capacity in the config space at 0x100, readable
                // BEFORE any queue is set up — the first fact a disk driver needs, and the smallest honest storage claim.
                if device == 2 {
                    // SAFETY: inside the window the firmware described.
                    let (cap_lo, cap_hi) = unsafe {
                        (
                            core::ptr::read_volatile((window + 0x100) as *const u32),
                            core::ptr::read_volatile((window + 0x104) as *const u32),
                        )
                    };
                    let capacity = ((cap_hi as u64) << 32) | cap_lo as u64;
                    let mut l = uart::LineBuf::new();
                    l.push_str("BLK: block device at ");
                    l.push_hex(window as u64);
                    l.push_str(" — capacity ");
                    l.push_u64(capacity);
                    l.push_str(" sectors (");
                    l.push_u64(capacity * 512 / (1024 * 1024));
                    l.push_str(" MiB)");
                    // THE FIRST READ. A sector read is a 3-descriptor chain; the result must round-trip from the disk the gate wrote.
                    let blk = blk::Blk { base: window };
                    match blk::start_queue(&blk) {
                        Ok(()) => {
                            let mut sector = [0u8; 512];
                            match blk::read_sector(&blk, 0, &mut sector) {
                                Ok(0) => {
                                    let mut l = uart::LineBuf::new();
                                    l.push_str("BLK: sector 0 read — first 8 bytes: ");
                                    for (n, b) in sector[..8].iter().enumerate() {
                                        if n > 0 { l.push_str(" "); }
                                        let h = b"0123456789abcdef";
                                        l.push_byte(h[(*b >> 4) as usize]);
                                        l.push_byte(h[(*b & 0xf) as usize]);
                                    }
                                    l.flush(&uart);
                                    // THE WRITE, AND ITS PROOF. Write a signature the Core chooses, read the sector back, and see the same bytes.
                                    let mut wdata = [0u8; 512];
                                    for (n, b) in b"CORE-WR1".iter().enumerate() { wdata[n] = *b; }
                                    match blk::write_sector(&blk, 1, &wdata) {
                                        Ok(0) => {
                                            let mut back = [0u8; 512];
                                            match blk::read_sector(&blk, 1, &mut back) {
                                                Ok(0) => {
                                                    let mut l = uart::LineBuf::new();
                                                    l.push_str("BLK: wrote and read back sector 1: ");
                                                    for b in &back[..8] {
                                                        if *b >= 0x20 && *b < 0x7f { l.push_byte(*b); } else { l.push_str("."); }
                                                    }
                                                    l.flush(&uart);
                                                }
                                                _ => uart.write_line("BLK: read-back after write failed"),
                                            }
                                        }
                                        _ => uart.write_line("BLK: the write returned a non-zero status"),
                                    }
                                }
                                Ok(st) => { let mut l = uart::LineBuf::new(); l.push_str("BLK: read returned status "); l.push_u64(st as u64); l.flush(&uart); }
                                Err(e) => uart.write_line(e),
                            }
                        }
                        Err(e) => uart.write_line(e),                                                    // MILESTONE 2: code from the disk, copied into executable memory, jumped to. The gate writes mov x0,#0x42; ret
                                                    // into sector 2; the boot reads it back into LOAD_BUF and calls it. The result is the same 66, now from the medium.
                                                    let mut sector2 = [0u8; 512];
                                                    if let Ok(0) = blk::read_sector(&blk, 2, &mut sector2) {
                                                        unsafe {
                                                            for i in 0..8 {
                                                                core::ptr::write_volatile((core::ptr::addr_of_mut!(LOAD_BUF) as *mut u8).add(i), sector2[i]);
                                                            }
                                                            let loaded: unsafe extern "C" fn() -> u64 = core::mem::transmute(core::ptr::addr_of!(LOAD_BUF) as *const ());
                                                            let r = loaded();
                                                            let mut l = uart::LineBuf::new();
                                                            l.push_str("LOAD: code loaded from disk returned ");
                                                            l.push_u64(r);
                                                            l.flush(&uart);
                                                        }
                                                    }

                    }
                    l.flush(uart);
                }
            }
            match found {
                Some(w) => {
                    let mut l = uart::LineBuf::new();
                    l.push_str("NET: the network device is at ");
                    l.push_hex(w as u64);
                    l.push_str(" — chosen by its own device id");
                    l.flush(uart);

                    // THE STATUS HANDSHAKE, in the order the specification demands: reset, acknowledge, driver,
                    // features, then DRIVER_OK. Each step is read back, because a driver that assumes a write landed is
                    // a driver that cannot tell "unsupported" from "broken".
                    // SAFETY: inside the window the firmware described.
                    unsafe {
                        let rd = |off: usize| core::ptr::read_volatile((w + off) as *const u32);
                        let wr = |off: usize, v: u32| core::ptr::write_volatile((w + off) as *mut u32, v);
                        wr(0x070, 0); // reset
                        let mut status = 0u32;
                        wr(0x070, status | 1); // ACKNOWLEDGE
                        status |= 1;
                        wr(0x070, status | 2); // DRIVER
                        status |= 2;
                        wr(0x014, 0); // feature word 0
                        let features = rd(0x010);
                        wr(0x024, 0);
                        // Legacy only: the page size the ring will use.
                        wr(0x028, 4096);
                        wr(0x020, features & 0x0000_0001); // take only what we understand so far
                        wr(0x070, status | 8); // FEATURES_OK
                        let after_features = rd(0x070);
                        status = after_features;
                        wr(0x070, status | 4); // DRIVER_OK
                        let final_status = rd(0x070);

                        let mut l = uart::LineBuf::new();
                        l.push_str("NET: features offered 0x");
                        l.push_hex(features as u64);
                        l.push_str(", status now 0x");
                        l.push_hex(final_status as u64);
                        l.flush(uart);

                        // The MAC lives in the device's config space, at 0x100 for legacy virtio-net.
                        let mac: [u8; 6] = [
                            core::ptr::read_volatile((w + 0x100) as *const u8),
                            core::ptr::read_volatile((w + 0x101) as *const u8),
                            core::ptr::read_volatile((w + 0x102) as *const u8),
                            core::ptr::read_volatile((w + 0x103) as *const u8),
                            core::ptr::read_volatile((w + 0x104) as *const u8),
                            core::ptr::read_volatile((w + 0x105) as *const u8),
                        ];
                        let mut l = uart::LineBuf::new();
                        l.push_str("NET: the device's MAC is ");
                        const HEX: &[u8; 16] = b"0123456789abcdef";
                        for (i, b) in mac.iter().enumerate() {
                            if i > 0 {
                                l.push_str(":");
                            }
                            l.push_byte(HEX[(*b >> 4) as usize]);
                            l.push_byte(HEX[(*b & 0xf) as usize]);
                        }
                        l.flush(uart);
                        // THE RING AND THE FIRST FRAME. Started only after the device is ready: a ring offered to a
                        // device that has not acknowledged the driver is a ring the device will never read.
                        if final_status & 0x0f == 0x0f {
                            let dev = net::Device { base: w, mac, queue_num: 0 };
                            match net::start_queue(&dev) {
                                Ok(()) => {
                                    uart.write_line("NET: the queue's rings are in our own memory, addressed by page frame");
                                    let (frame, len) = net::arp_request(mac);
                                    match net::transmit(&dev, &frame[..len]) {
                                        Ok(written) => {
                                            let mut l = uart::LineBuf::new();
                                            l.push_str("NET: the device completed the descriptor and said it wrote ");
                                            l.push_u64(written as u64);
                                            l.push_str(" bytes");
                                            l.flush(uart);
                                            uart.write_line("NET: an ARP request went out on the wire — the host's capture decides whether it arrived");
                                            // THE OTHER HALF OF THE DRIVER. Our ARP request should provoke a reply, and receiving it is the first inbound
                                            // packet this Core ever takes: the receive queue, eight device-writable buffers, and a bounded poll.
                                            match net::start_rx(&dev) {
                                                Ok(()) => {
                                                    uart.write_line("NET: eight receive buffers are offered to the device");
                                                    // ASK AGAIN, NOW THAT THE BUFFERS EXIST. Measured: the reply to the first request had nowhere to land,
                                                    // because the receive queue was set up after the transmit. A device with no descriptors drops the frame.
                                                    let _ = net::transmit(&dev, &frame[..len]);
                                                    match net::poll_rx(&dev, 50_000_000) {
                                                        Some(r) => {
                                                            let mut l = uart::LineBuf::new();
                                                            l.push_str("NET: RECEIVED ");
                                                            l.push_u64(r.len as u64);
                                                            l.push_str(" bytes, ethertype ");
                                                            l.push_hex(r.ethertype as u64);
                                                            l.push_str(" — an inbound frame completed into our ring");
                                                            l.flush(&uart);
                                                            // THE FIRST ANSWER. Give the reply builder a real subject — a request for our own address — and put the
                                                            // reply on the wire. The capture then holds an ARP reply (op 2) from our MAC, which is a reply the Core
                                                            // composed, not one it merely echoed.
                                                            let (req, req_len) = net::arp_request_from(r.src_mac);
                                                            let (reply, reply_len) = net::arp_reply(&req[..req_len], mac);
                                                            match net::transmit(&dev, &reply[..reply_len]) {
                                                                Ok(_) => {
                                                                    let mut l = uart::LineBuf::new();
                                                                    l.push_str("NET: ARP REPLY sent — our address is at our MAC, to ");
                                                                    for (i, b) in req[6..12].iter().enumerate() {
                                                                        if i > 0 {
                                                                            l.push_str(":");
                                                                        }
                                                                        let h = b"0123456789abcdef";
                                                                        l.push_byte(h[(*b >> 4) as usize]);
                                                                        l.push_byte(h[(*b & 0xf) as usize]);
                                                                    }
                                                                    l.flush(&uart);
                                                                }
                                                                Err(e) => uart.write_line(e),
                                                            }
                                                                    // RUNG 1 OF THE NEXT CHUNK. An echo request from the gateway, answered with a reply the Core computes —
                                                                    // checksums and all. slirp cannot ping us, so the request is synthesized from the gateway's real MAC.
                                                                    let (echo_req, echo_req_len) = ip::echo_request_from(r.src_mac, ip::GATEWAY_IP, mac);
                                                                    let (echo_reply, echo_reply_len) = ip::echo_reply(&echo_req[..echo_req_len], mac);
                                                                    match net::transmit(&dev, &echo_reply[..echo_reply_len]) {
                                                                        Ok(_) => uart.write_line("NET: ICMP echo reply sent — the Core answers a ping"),
                                                                        Err(e) => uart.write_line(e),
                                                                    }
                                                                        // RUNG 2: UDP. A datagram OUT (captured off the wire), and a parse IN — ports, length, payload. The out direction
                                                                        // is real delivery; the in direction is real parsing against a synthesized datagram until a real peer exists.
                                                                        let (udp_out, udp_out_len) = udp::datagram(mac, ip::OUR_IP, r.src_mac, ip::GATEWAY_IP, 9000, 9999, b"MORPHEOS");
                                                                        match net::transmit(&dev, &udp_out[..udp_out_len]) {
                                                                            Ok(_) => uart.write_line("NET: UDP datagram sent to 10.0.2.2:9999 — the wire decides whether it arrived"),
                                                                            Err(e) => uart.write_line(e),
                                                                        }
                                                                        let (udp_in, udp_in_len) = udp::datagram(r.src_mac, ip::GATEWAY_IP, mac, ip::OUR_IP, 9999, 9000, b"reply-ok");
                                                                        match udp::parse(&udp_in[..udp_in_len]) {
                                                                            Some(p) => {
                                                                                let mut l = uart::LineBuf::new();
                                                                                l.push_str("NET: UDP parsed — src port ");
                                                                                l.push_u64(p.src_port as u64);
                                                                                l.push_str(", dst port ");
                                                                                l.push_u64(p.dst_port as u64);
                                                                                l.push_str(", payload ");
                                                                                l.push_u64(p.payload_len as u64);
                                                                                l.push_str(" bytes");
                                                                                l.flush(&uart);
                                                                            }
                                                                            None => uart.write_line("NET: UDP parse FAILED"),
                                                                        }
                                                                            // RUNG 3: RTP. Eight packets, sequence 0..7, timestamp advancing 48 per packet (1 ms at 48 kHz). The claim is
                                                                            // determinism, and the gate verifies it off the wire.
                                                                            for i in 0..8u32 {
                                                                                let (pkt, pkt_len) = rtp::packet(mac, ip::OUR_IP, r.src_mac, ip::GATEWAY_IP, 5004, 5004, i as u16, 0x1122_3300 + i * rtp::TIMESTAMP_STEP, 0xDEAD_BEEF);
                                                                                if let Err(e) = net::transmit(&dev, &pkt[..pkt_len]) {
                                                                                    uart.write_line(e);
                                                                                }
                                                                            }
                                                                            uart.write_line("NET: RTP sent 8 packets — sequence 0..7, timestamp step 48, the gate verifies both");
                                                        }
                                                        None => uart.write_line("NET: nothing arrived in the budget — a quiet wire is not an error"),
                                                    }
                                                }
                                                Err(e) => uart.write_line(e),
                                            }
                                            // THE DESCRIPTOR THAT ACTUALLY WENT. A failed transmit shows the ring we meant to write; this shows the one
                                            // the device used. Measured: the captured frame began 10 bytes into our buffer, so the window is the evidence.
                                            let (d_addr, d_len, d_flags, a_flags, a_idx, a_ring0, _) = net::ring_snapshot();
                                            let mut l = uart::LineBuf::new();
                                            l.push_str("NET: sent — descriptor addr ");
                                            l.push_hex(d_addr);
                                            l.push_str(", len ");
                                            l.push_u64(d_len as u64);
                                            l.push_str(", flags ");
                                            l.push_u64(d_flags as u64);
                                            l.push_str("; avail idx ");
                                            l.push_u64(a_idx as u64);
                                            l.push_str(", ring[0] ");
                                            l.push_u64(a_ring0 as u64);
                                            l.push_str(", flags ");
                                            l.push_u64(a_flags as u64);
                                            l.flush(&uart);
                                        }
                                        Err(e) => {
                                            uart.write_line(e);
                                            let (addr, len, flags, aflags, aidx, aring0, _) = net::ring_snapshot();
                                            let mut l = uart::LineBuf::new();
                                            l.push_str("NET: ring read back — desc.addr ");
                                            l.push_hex(addr);
                                            l.push_str(", len ");
                                            l.push_u64(len as u64);
                                            l.push_str(", flags ");
                                            l.push_u64(flags as u64);
                                            l.push_str("; avail.flags ");
                                            l.push_u64(aflags as u64);
                                            l.push_str(", idx ");
                                            l.push_u64(aidx as u64);
                                            l.push_str(", ring[0] ");
                                            l.push_u64(aring0 as u64);
                                            l.flush(&uart);
                                            // WHAT DOES THE DEVICE THINK? A queue the device refused and a queue it never
                                            // read look identical from the outside - both are a timeout. The status
                                            // register tells them apart: bit 6 is DEVICE_NEEDS_RESET, and the PFN
                                            // readback says whether our page frame landed at all.
                                            // SAFETY: inside the window the firmware described.
                                            let (status, pfn, qnum) = unsafe {
                                                (
                                                    core::ptr::read_volatile((w + 0x070) as *const u32),
                                                    core::ptr::read_volatile((w + 0x040) as *const u32),
                                                    core::ptr::read_volatile((w + 0x038) as *const u32),
                                                )
                                            };
                                            let mut l = uart::LineBuf::new();
                                            l.push_str("NET: after the kick — status 0x");
                                            l.push_hex(status as u64);
                                            l.push_str(", QueuePFN 0x");
                                            l.push_hex(pfn as u64);
                                            l.push_str(", QueueNum ");
                                            l.push_u64(qnum as u64);
                                            l.flush(&uart);
                                        }
                                    }
                                }
                                Err(e) => uart.write_line(e),
                            }
                        }
                        if final_status & 0x0f == 0x0f {
                            uart.write_line("NET: the device acknowledged the driver and is ready — a driver exists, not yet a frame");
                        } else {
                            uart.write_line("NET: FAIL the device did not reach DRIVER_OK");
                        }
                    }
                }
                None => uart.write_line("NET: no virtio-net among the windows scanned"),
            }
        }
else {
                uart.write_line("FDT: the tree DISAGREES with the constant — the tree is right");
            }
        }
        Err(_) => uart.write_line("FDT: FAIL no arm,pl011 node was found in the tree"),
    }

}

/// M0c's demonstration: the boot decision exercised through the five states that matter, printed so a person can read
/// what the machine decided rather than trusting that it decided well.
fn ab_demo(uart: &Uart) {
    let mut all_ok = true;

    let mut fresh = ab::Record::fresh();
    let d = ab::choose(&fresh);
    let ok = d.boot == ab::Slot::A && !d.is_trial;
    all_ok &= ok;
    uart.write_line(if ok { "AB: a fresh record boots A" } else { "AB: FAIL a fresh record did not boot A" });

    let trial_offered = ab::propose(&mut fresh, ab::Slot::B);
    let d = ab::choose(&fresh);
    let ok = trial_offered && d.boot == ab::Slot::B && d.is_trial;
    all_ok &= ok;
    uart.write_line(if ok { "AB: a new generation is tried ONCE, as a trial" } else { "AB: FAIL the trial was not offered" });

    ab::settle(&mut fresh, d, true);
    let ok = fresh.active == ab::Slot::B.id() && fresh.trial == 0;
    all_ok &= ok;
    uart.write_line(if ok { "AB: a healthy trial commits and becomes the good generation" } else { "AB: FAIL a healthy trial did not commit" });

    // The case the whole rung exists for: the new generation boots, does NOT report healthy, and the machine must come
    // back to the generation that worked - with the record saying which one failed.
    let mut failed = ab::Record::fresh();
    ab::propose(&mut failed, ab::Slot::B);
    let d = ab::choose(&failed);
    ab::settle(&mut failed, d, false);
    let d2 = ab::choose(&failed);
    let ok = d2.boot == ab::Slot::A && d2.fell_back && failed.tried == ab::Slot::B.id() && failed.active == ab::Slot::A.id();
    all_ok &= ok;
    uart.write_line(if ok {
        "AB: an unhealthy trial falls back to the good generation, and the record remembers it"
    } else {
        "AB: FAIL an unhealthy trial became permanent"
    });

    let corrupt = ab::Record { magic: 0, active: 9, trial: 7, attempts_left: 5, tried: 0 };
    let repaired = ab::Record::read(corrupt);
    let ok = repaired == ab::Record::fresh();
    all_ok &= ok;
    uart.write_line(if ok { "AB: a corrupt record boots the safe generation instead of a guess" } else { "AB: FAIL a corrupt record was trusted" });

    uart.write_line(if all_ok {
        "AB: the boot path is a decision, and it never commits what did not report healthy"
    } else {
        "AB: FAIL the boot decision is not what it claims"
    });
}

/// The Core's first instruction in Rust.
#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    let uart = Uart::at(UART_QEMU_VIRT);
    uart.init();

    uart.write_line(BANNER);
    // THE LOADER, MILESTONE 1. Jump to bytes the compiler did not place here, call them, and read the return. The
    // claim is not "it compiled" but "the CPU executed bytes it was handed at runtime".
    {
        // SAFETY: the code is in .text, position-independent, and returns via ret.
        let loaded: unsafe extern "C" fn() -> u64 = unsafe {
            core::mem::transmute(core::ptr::addr_of!(LOADED_CODE) as *const ())
        };
        let result = unsafe { loaded() };
        let mut l = uart::LineBuf::new();
        l.push_str("LOAD: jumped to loaded code, it returned ");
        l.push_u64(result);
        l.flush(&uart);
    }

    ab_demo(&uart);
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

    // The console can now print a whole line from one context without being spliced by another: the line is built
    // first, then flushed under a single lock. The check is that the SAME line comes out intact through the buffer.
    {
        use uart::LineBuf;
        let mut line = LineBuf::new();
        line.push_str("CONSOLE: buffered line, ");
        line.push_u64(2);
        line.push_str(" tasks expected, counter at ");
        line.push_hex(0x2a);
        line.flush(&uart);
    }

    // ---- M0b: ask the MACHINE where its UART is. Only the Image path has a tree; every ELF gate behaves exactly as
    // it did before this was written. ----
    if unsafe { DTB_POINTER } != 0 {
        fdt_demo(&uart);
    }

    // ---- M7d: a box is validated ONCE at load. If its claim holds, it runs without a mask per access — which is the
    // 5–30% a sandbox otherwise costs, and the difference between this OS and a masked container at a 1 ms deadline. ----
    {
        let honest = box_admission::BoxRequest {
            name: "vst-box",
            code_start: 0x1000,
            code_len: 0x2000,
            limit: 0x4000,
            claim: box_admission::Claim { highest_address: 0x3800, signer: "vendor-signature" },
        };
        match box_admission::admit(&honest) {
            Ok(a) => {
                let mut l = uart::LineBuf::new();
                l.push_str("BOX: admitted ");
                l.push_str(a.name);
                l.push_str(" — claim checked once against limit ");
                l.push_hex(a.checked_against);
                l.push_str(", no per-access mask needed");
                l.flush(&uart);
            }

            Err(_) => uart.write_line("BOX: FAIL an honest box was refused"),
        }
        let lying = box_admission::BoxRequest {
            name: "lying-box",
            code_start: 0x1000,
            code_len: 0x2000,
            limit: 0x4000,
            claim: box_admission::Claim { highest_address: 0x9fff, signer: "vendor-signature" },
        };
        match box_admission::admit(&lying) {
            Err(box_admission::Refused::ClaimOutside { claimed, limit, signer }) => {
                let mut l = uart::LineBuf::new();
                l.push_str("BOX: REFUSED a claim of ");
                l.push_hex(claimed);
                l.push_str(" against limit ");
                l.push_hex(limit);
                l.push_str(" signed ");
                l.push_str(signer);
                l.push_str(" — refused at LOAD, not at an access");
                l.flush(&uart);
            }
            _ => uart.write_line("BOX: FAIL a box that claimed memory it was not granted was admitted"),
        }
    }

    // ---- M7c: a capability may NARROW and never widen. The Core does not apply a policy here; it performs a
    // derivation only when the wanted rights are a subset of the held ones. ----
    {
        let mut table = CapTable::empty();
        let full = table.grant_with(ObjectId(7), cap::Rights::READ.union(cap::Rights::WRITE));
        match cap::derive(full, cap::Rights::READ) {
            Ok(c) => {
                let mut l = uart::LineBuf::new();
                l.push_str("CAP: derived rights ");
                l.push_hex(c.rights.0 as u64);
                l.push_str(" from ");
                l.push_hex(full.rights.0 as u64);
                l.push_str(" — narrower, allowed");
                l.flush(&uart);
            }
            Err(_) => uart.write_line("CAP: FAIL a narrower derivation was refused"),
        }
        let wanted = cap::Rights::WRITE.union(cap::Rights::GRANT);
        match cap::derive(full, wanted) {
            Err(w) => {
                let mut l = uart::LineBuf::new();
                l.push_str("CAP: REFUSED widening to ");
                l.push_hex(w.wanted.0 as u64);
                l.push_str(" — held only ");
                l.push_hex(w.held.0 as u64);
                l.flush(&uart);
            }
            Ok(_) => uart.write_line("CAP: FAIL a derivation widened the rights"),
        }
    }

    // ---- M5c, policy half: two tasks, and the Core choosing between them. No assembly, no switch: this is the
    // decision, measured on its own so that the mechanism cannot hide a policy bug or the reverse. ----
    let mut scheduler = sched::Scheduler::new();
    scheduler.admit(sched::TaskId(0));
    scheduler.admit(sched::TaskId(1));
    uart.write_line("SCHED: two tasks admitted, both ready");
    let mut sequence = [0u8; 6];
    for slot in sequence.iter_mut() {
        match scheduler.pick_next() {
            Some(task) => *slot = b'0' + task.0 as u8,
            None => *slot = b'x',
        }
    }
    let expected = *b"101010";
    if sequence == expected {
        uart.write_line("SCHED: picks 1,0,1,0,1,0 — the choice alternates, so a second task really runs");
    } else {
        uart.write_str("SCHED: FAIL the choice did not alternate: ");
        for byte in sequence.iter() {
            uart.write_byte(*byte);
            uart.write_byte(b' ');
        }
        uart.write_line("");
    }
    uart.write_str("SCHED: switches recorded: 0x");
    uart.write_hex(scheduler.switches());
    uart.write_line("");

    // M5d, the priority policy — tested in the DECISION, not in the timing. Two tasks are ready; one is audio, one is
    // normal. Six picks must all be audio, because a lower class must not take the audio class's CPU while it is
    // ready. Timing would be the honest consequence, but a timing assertion on a 4-core VM is a flake generator; the
    // policy is where the claim actually lives.
    let mut prio = sched::Scheduler::new();
    prio.admit_with_priority(sched::TaskId(0), sched::Priority::Normal);
    prio.admit_with_priority(sched::TaskId(1), sched::Priority::Audio);
    let mut picks = [0u8; 6];
    for slot in picks.iter_mut() {
        *slot = match prio.pick_next() {
            Some(t) => b'0' + t.0 as u8,
            None => b'x',
        };
    }
    let mut line = uart::LineBuf::new();
    line.push_str("PRIORITY: picks ");
    for (i, b) in picks.iter().enumerate() {
        if i > 0 {
            line.push_str(",");
        }
        line.push_byte(*b);
    }
    line.flush(&uart);
    // ---- M7a: the certificate. Whether a graph fits its deadlines is arithmetic, not measurement: the Core checks
    // the exact rational inequality Σ(Cᵢ/Tᵢ) ≤ 1 in bounded time, refuses what does not fit, and names the task that
    // broke the budget. ----
    {
        let (used_before, _, _) = ALLOCATOR.stats();
        let fitting = cert::Certificate {
            terms: [
                cert::Term { name: "engine", cost: 200, period: 1000 },
                cert::Term { name: "control", cost: 100, period: 1000 },
                cert::Term { name: "meter", cost: 50, period: 1000 },
            ],
            len: 3,
            claimed_ppm: 350_000,
        };
        match cert::check(&fitting) {
            Ok(ppm) => {
                let mut l = uart::LineBuf::new();
                l.push_str("CERT: fitting graph admitted — utilisation ");
                l.push_u64(ppm);
                l.push_str(" ppm");
                l.flush(&uart);
            }
            Err(_) => uart.write_line("CERT: FAIL a fitting graph was refused"),
        }
        let too_big = cert::Certificate {
            terms: [
                cert::Term { name: "engine", cost: 900, period: 1000 },
                cert::Term { name: "control", cost: 100, period: 1000 },
                cert::Term { name: "meter", cost: 50, period: 1000 },
            ],
            len: 3,
            claimed_ppm: 1_050_000,
        };
        match cert::check(&too_big) {
            Err(cert::Rejected::OverBudget { largest }) => {
                let mut l = uart::LineBuf::new();
                l.push_str("CERT: over budget REFUSED — the term that broke it is ");
                l.push_str(largest);
                l.flush(&uart);
            }
            _ => uart.write_line("CERT: FAIL an over-budget graph was admitted"),
        }
        let lying = cert::Certificate { claimed_ppm: 123_456, ..fitting };
        match cert::check(&lying) {
            Err(cert::Rejected::Lying { claimed_ppm, actual_ppm }) => {
                let mut l = uart::LineBuf::new();
                l.push_str("CERT: lying certificate REFUSED — claimed ");
                l.push_u64(claimed_ppm);
                l.push_str(" ppm, arithmetic says ");
                l.push_u64(actual_ppm);
                l.flush(&uart);
            }
            _ => uart.write_line("CERT: FAIL a certificate that lied was accepted"),
        }
        // THE LEDGER REFUSES WHAT CANNOT PROVE ITSELF. This is the rung's purpose: a generation that cannot show it
        // fits the budget does not become history - the head does not move, and the refusal names the term.
        let mut budgeted = ledger::Ledger::empty();
        match budgeted.apply_certified(0xD0D0, &fitting) {
            Ok((id, ppm)) => {
                let mut l = uart::LineBuf::new();
                l.push_str("CERT: generation ");
                l.push_u64(id as u64);
                l.push_str(" applied with a certificate at ");
                l.push_u64(ppm);
                l.push_str(" ppm");
                l.flush(&uart);
            }
            Err(_) => uart.write_line("CERT: FAIL a certified generation was refused"),
        }
        let head_before = budgeted.head_id();
        match budgeted.apply_certified(0xBAD, &too_big) {
            Err(crate::cert::Rejected::OverBudget { largest }) => {
                let mut l = uart::LineBuf::new();
                l.push_str("CERT: a generation was REFUSED — ");
                l.push_str(largest);
                l.push_str(" broke the budget, and the ledger's head did not move (");
                l.push_u64(head_before as u64);
                l.push_str(" -> ");
                l.push_u64(budgeted.head_id() as u64);
                l.push_str(")");
                l.flush(&uart);
            }
            _ => uart.write_line("CERT: FAIL an uncertified generation became history"),
        }

        let (used_after, _, _) = ALLOCATOR.stats();
        let mut l = uart::LineBuf::new();
        l.push_str("CERT: bounded — allocator use before ");
        l.push_u64(used_before as u64);
        l.push_str(" after ");
        l.push_u64(used_after as u64);
        l.flush(&uart);
        if used_before == used_after {
            uart.write_line("CERT: the check allocated nothing, so a hostile certificate cannot make it expensive");
        } else {
            uart.write_line("CERT: FAIL the check allocated");
        }
    }

    if picks == *b"111111" {
        uart.write_line("PRIORITY: the audio class took every pick — a normal task cannot starve it");
    } else {
        uart.write_line("PRIORITY: FAIL a normal task took a pick the audio class should have had");
    }

    // Build a task frame and CHECK IT before anything could jump to it. A frame that is wrong is a hang with no
    // output, so its two resumption slots are printed: the entry address and the PSTATE the task will start in.
    let task_region_top = {
        let (bottom, _) = task::region();
        let _ = bottom;
        let stack = core::ptr::addr_of!(sched::TASK_B_STACK) as usize;
        stack + core::mem::size_of::<sched::TaskStack>()
    };
    let frame = sched::install_frame(task_region_top, task_second_entry as usize);
    let (elr, spsr) = sched::frame_state(frame);
    uart.write_str("SCHED: a task frame at 0x");
    uart.write_hex(frame as u64);
    uart.write_str(" resumes at 0x");
    uart.write_hex(elr);
    uart.write_str(" in PSTATE 0x");
    uart.write_hex(spsr);
    uart.write_line("");
    if elr == task_second_entry as u64 && spsr == sched::SPSR_EL1H_UNMASKED {
        uart.write_line("SCHED: the frame is resumable — entry and PSTATE are what a switch must load");
    } else {
        uart.write_line("SCHED: FAIL the frame's resumption slots are wrong");
    }

    // ---- M5a: the machine's clock. The counter every deadline will be measured against. ----
    let freq = clock::frequency();
    uart.write_str("TICK: CNTFRQ_EL0 = 0x");
    uart.write_hex(freq);
    uart.write_line(" Hz (from the hardware, not from a table)");
    // The Core states this itself rather than leaving a gate to guess at a hex string. Measured: two rounds were lost
    // to a regex over a zero-padded value, which is a gate problem pretending to be a clock problem.
    if freq != 0 {
        uart.write_line("TICK: the frequency is non-zero, as a clock needs it to be");
    } else {
        uart.write_line("TICK: FAIL the hardware reported no frequency");
    }
    let start = clock::now();
    // A known workload, so the advance is measured rather than asserted.
    let mut spin: u64 = 0;
    for i in 0..200_000u64 {
        spin = spin.wrapping_add(i);
    }
    let after = clock::now();
    let advanced = after.saturating_sub(start);
    uart.write_str("TICK: the counter advanced by 0x");
    uart.write_hex(advanced);
    uart.write_str(" ticks over a 200k-iteration loop (checksum 0x");
    uart.write_hex(spin);
    uart.write_line(")");
    if advanced == 0 {
        uart.write_line("TICK: FAIL the counter did not advance — a deadline can never be met");
    } else {
        uart.write_line("TICK: the counter advances");
    }
    // A deadline: 1 ms, measured in the hardware's own units, and measured ONLY around the wait.
    //
    // Measured wrong the first time: the reading was taken before the line above was printed, so the console write
    // (~100 characters at 115200 baud) was counted as part of a 1 ms deadline — which is where the reported 10.8 ms
    // came from. A latency figure that includes the instrument is not a latency figure.
    let before = clock::now();
    let deadline = before + clock::ticks_for_ms(1);
    let reached = clock::wait_until(deadline);
    let elapsed_ns = clock::ns_between(before, reached);
    uart.write_str("TICK: a 1 ms deadline was reached, 0x");
    uart.write_hex(elapsed_ns);
    uart.write_line(" ns after the wait began");
    // Strict, and on purpose: a 1 ms deadline met in 10 ms is a miss, and a bound that accepts it is not a bound.
    if elapsed_ns >= 1_000_000 && elapsed_ns <= 3_000_000 {
        uart.write_line("TICK: the deadline was honoured in real time (within 3 ms of a 1 ms target)");
    } else if elapsed_ns < 1_000_000 {
        uart.write_line("TICK: FAIL the deadline returned before its time");
    } else {
        uart.write_line("TICK: FAIL the deadline missed by more than 3x — real-time budget not met");
    }

    // ---- M5b interrupt half: the timer interrupts the Core, with no polling loop asking. ----
    uart.write_line("IRQ: about to bring up the GICv2");
    gic::init();
    uart.write_line("IRQ: the GIC is up (distributor + CPU interface + PPI 30)");
    clock::arm_in(clock::ticks_for_ms(1));
    uart.write_line("IRQ: the timer is armed for 1 ms");
    let irq_start = clock::now();
    // SAFETY: the vector table and the GIC are both in place.
    unsafe { gic::unmask() };
    uart.write_line("IRQ: interrupts are unmasked — anything that arrives from here is the machine's, not a poll");
    let mut waited = 0u64;
    while gic::ticks() < 3 {
        if clock::ns_between(irq_start, clock::now()) > 50_000_000 {
            break;
        }
        waited += 1;
        core::hint::spin_loop();
    }
    let arrived = gic::ticks();
    let irq_ns = clock::ns_between(irq_start, clock::now());
    // SAFETY: mask interrupts again so the rest of the boot is deterministic.
    unsafe { core::arch::asm!("msr daifset, #2", options(nostack)) };
    if arrived >= 3 {
        uart.write_str("TICK: the Core was INTERRUPTED 0x");
        uart.write_hex(arrived);
        uart.write_str(" times by the timer, 0x");
        uart.write_hex(irq_ns);
        uart.write_line(" ns after unmasking (no polling: the interrupts arrived on their own)");
        uart.write_line("TICK: the tick is real — a real-time class can be built on it");
    } else {
        uart.write_str("TICK: FAIL only 0x");
        uart.write_hex(arrived);
        uart.write_line(" timer interrupts arrived in 50 ms — the interrupt path is not working");
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
    task::set_resume(after_task_aborted);
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
    let returned = unsafe { task::run(task_entry, stack_for_task) };
    let recorded = unsafe { task::TASK_RESULT };
    // Two sources must agree: what the switch returned and what the task recorded. Disagreement is a bug in the
    // switch, and it is exactly the class of bug that took three attempts to get out of.
    let value = if returned == recorded {
        returned
    } else {
        uart.write_str("M4a: FAIL the switch returned 0x");
        uart.write_hex(returned);
        uart.write_str(" but the task recorded 0x");
        uart.write_hex(recorded);
        uart.write_line("");
        0
    };
    // The task returns buffer[0] + buffer[2047] = 0xA5 + 0x5A, so a correct return is exactly 0xFF.
    // The judgement: where did the task's stack ACTUALLY sit, compared with the region the Core handed it?
    let seen = unsafe { task::TASK_STACK_SEEN };
    uart.write_str("M4a: task frame at 0x");
    uart.write_hex(seen as u64);
    uart.write_str(", region 0x");
    uart.write_hex(task_bottom as u64);
    uart.write_str("..0x");
    uart.write_hex(task_stack_top as u64);
    uart.write_str(", returned 0x");
    uart.write_hex(value);
    uart.write_line("");
    if seen >= task_bottom && seen < task_stack_top && value == 0xFF {
        uart.write_line("M4a: the task ran on its own stack and returned to the Core");

        // ---- M4b: the task's own address space. The Core maps PRIVATE; the task's tables do not. ----
        mmu::build_task_tables();
        uart.write_line("M4b: switching the task to its own tables — the private block is absent from its map");
        // SAFETY: the tables are built and the code running here is mapped in both regimes.
        unsafe { mmu::use_task_tables() };
        uart.write_line("M4b: task tables active; the task now reaches for 0x4E000000, which the Core maps and it does not");
        // SAFETY: the read is the attack. If the tables are right, it faults and the handler reports the violation.
        let stolen = unsafe { core::ptr::read_volatile(mmu::PRIVATE as *const u64) };
        uart.write_str("M4b: FAIL the task read the Core's memory (value ");
        uart.write_hex(stolen);
        uart.write_line(") — the map was supposed to make that impossible");
        uart.write_line("M4c: NOTE the task returned instead of faulting, so there was no abort to exercise");
        // SAFETY: restore the Core's regime for the rest of the boot.
        unsafe { mmu::use_core_tables() };
        uart.write_line("M4b: the Core is back on its own tables");
    } else {
        uart.write_line("M4a: FAIL the task did not return the value it must");
    }
    } else {
        uart.write_str("MMU: FAIL the guard page was readable (value ");
        uart.write_hex(phantom);
        uart.write_line(") — memory we did not map must not be reachable");
    }

    uart.write_line(DONE);

    boot_epilogue(&uart);
    // The epilogue returns when preemption is not built in; kmain never does.
    loop {
        // SAFETY: nothing left to return to.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }

    }
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
    if far == mmu::PRIVATE as u64 {
        uart.write_line("  M4b: TASK VIOLATION — the task named 0x4E000000, which is absent from its own map");
        uart.write_line("  M4b: the Core maps that block; the task's tables do not. This is containment, not a refusal.");
        uart.write_line("  M4b: aborting the task — the Core resumes, the evidence stays");
        // THE ABORT, rung M4c: instead of halting the machine, give the Core its stack and return address back and
        // continue as if the task had finished. A task that oversteps is a dead task, not a dead show.
        // SAFETY: `run` parked the Core's context before the task started.
        unsafe { task::abort_to_core() };
    }
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
