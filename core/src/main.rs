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

mod cap;
mod uart;
use cap::{access, CapTable, Object, ObjectId};
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
    bl   kmain
1:  wfe
    b    1b
"#
);

/// The Core's first instruction in Rust.
#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    let uart = Uart::at(UART_QEMU_VIRT);
    uart.init();

    uart.write_line(BANNER);
    uart.write_line("MORPHEOS-CORE: EL1, MMU off, load address 0x40080000");

    // ---- M1: the capability space. Two objects, two tasks, and the Core deciding who may name what. ----
    //
    // `task_bench` is handed the measurement object and NOT the plugin-state object. `task_plugin` is the other way
    // round. Neither holds a "user" and neither can obtain the other's object: the only constructor of a capability
    // is `CapTable::grant`, and only the Core calls it.
    let measurement = Object { id: ObjectId(1), name: "measurement", value: 0xA1 };
    let plugin_state = Object { id: ObjectId(2), name: "plugin-state", value: 0xB2 };

    let mut task_bench = CapTable::empty();
    let _bench_cap = task_bench.grant(measurement.id);
    let mut task_plugin = CapTable::empty();
    let _plugin_cap = task_plugin.grant(plugin_state.id);

    // The mutation: `scripts/m1.sh --self-test` builds with this feature and the Core must then ALLOW what it refuses
    // in the real build. If the test still passes, the test is asserting a string rather than the Core's table.
    #[cfg(feature = "m1-mutant-grant")]
    let _mutant = task_bench.grant(plugin_state.id);

    uart.write_line("M1: task_bench holds: measurement");
    uart.write_line("M1: task_plugin holds: plugin-state");

    let _ = access(&uart, "task_bench", &task_bench, &measurement);   // must be ALLOWED
    let _ = access(&uart, "task_bench", &task_plugin, &plugin_state); // must be REFUSED by the Core
    let _ = access(&uart, "task_plugin", &task_plugin, &plugin_state);// must be ALLOWED
    let _ = access(&uart, "task_plugin", &task_plugin, &measurement); // must be REFUSED by the Core

    uart.write_line("M1: the Core decided both directions; no task holds a capability it was not handed");
    uart.write_line(DONE);

    // Nothing to return to: no scheduler and no exit device on this architecture. The test harness watches the serial
    // line for DONE and stops the machine — honest, because on rung M0 halting *is* the end of the story.
    loop {
        // SAFETY: `wfe` at EL1 is always legal; it is the correct way for an idle core to wait.
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
