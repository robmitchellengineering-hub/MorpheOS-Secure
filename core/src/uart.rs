//! PL011 UART — the serial port the machine talks to before it has a driver model.
//!
//! On QEMU's `virt` machine the PL011 sits at 0x0900_0000. On a Raspberry Pi 5 the same PL011 is the primary UART at
//! 0x1F_2201_0000 (RP1) — so this file is the *first* place the two machines differ, and the base address is a
//! parameter rather than a constant, because a Core that hard-codes one board's address is a Core for one board.
//!
//! Polled, 8N1, no interrupts: M0 has one writer and nothing else to do.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering};

/// One writer at a time. Measured: two tasks sharing the console interleaved mid-line, so the log said
/// "TASK B:PREEMPT: switching enabled … ran" and no gate could read it. A lock fixes that.
///
/// It is a TRY-lock, deliberately: the interrupt handler prints too, and a handler that spins on a lock held by the
/// task it interrupted deadlocks the machine. A line is a diagnostic, not a guarantee — dropping one is survivable,
/// wedging the Core is not.
static CONSOLE: AtomicBool = AtomicBool::new(false);

fn try_lock() -> bool {
    CONSOLE.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
}

fn unlock() {
    CONSOLE.store(false, Ordering::Release);
}

/// QEMU `virt`: PL011.
pub const UART_QEMU_VIRT: usize = 0x0900_0000;
/// Raspberry Pi 5: the PL011 on the RP1 south bridge.
pub const UART_PI5: usize = 0x1F_2201_0000;

const DR: usize = 0x00; // data register
const FR: usize = 0x18; // flag register
const TXFF: u32 = 1 << 5; // transmit FIFO full
const LCR_H: usize = 0x2c;
const CR: usize = 0x30;

pub struct Uart {
    base: usize,
}

impl Uart {
    pub const fn at(base: usize) -> Self {
        Self { base }
    }

    fn reg(&self, offset: usize) -> *mut u32 {
        (self.base + offset) as *mut u32
    }

    /// 8 bits, no parity, one stop bit, FIFO on, UART enabled, TX only. Nothing here is clever; it is the shortest
    /// path from "the CPU is running" to "a person can read it".
    pub fn init(&self) {
        // SAFETY: the base address is the PL011 for the machine this image was built for (see the constants), and
        // each offset is a documented PL011 register. This runs once, before any concurrency exists.
        unsafe {
            write_volatile(self.reg(LCR_H), 0x70); // 8N1, FIFO enabled
            write_volatile(self.reg(CR), 0x301); // UARTEN | TXE
        }
    }

    pub fn write_byte(&self, byte: u8) {
        // NO FLAG POLL, AND A FIXED PACE. Measured twice on studio-linux2: the polled write wedged the machine on the
        // longest line - FR.TXFF stayed set and the Core stopped talking mid-sentence, with the bounded escape only
        // hiding the symptom. A console that depends on a flag clearing cannot be trusted on the boot path, so this
        // paces the write instead: long enough for a 115200-baud FIFO to drain a byte, short enough to cost nothing.
        // If the UART is genuinely slower than assumed, a character is dropped - and that is a visible failure, not a
        // machine that stops.
        for _ in 0..2_000u32 {
            core::hint::spin_loop();
        }
        // SAFETY: DR is the PL011's data register at a base address chosen for the machine this image was built for.
        unsafe {
            write_volatile(self.reg(DR), u32::from(byte));
        }
    }

    pub fn write_str(&self, text: &str) {
        // Guarded, so a line is not spliced into another context's line.
        if !try_lock() {
            return;
        }
        self.write_str_unlocked(text);
        unlock();
    }

    fn write_str_unlocked(&self, text: &str) {
        for byte in text.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
    }

    /// Hex, because a fault report is read by an engineer, not a person on a phone.
    pub fn write_hex(&self, value: u64) {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        self.write_str("0x");
        let mut started = false;
        for shift in (0..16).rev() {
            let nibble = ((value >> (shift * 4)) & 0xf) as usize;
            if nibble != 0 {
                started = true;
            }
            if started || shift == 0 {
                self.write_byte(DIGITS[nibble]);
            }
        }
    }

    pub fn write_line(&self, text: &str) {
        // The whole line under one lock, so it cannot be split by a switch or an interrupt.
        if !try_lock() {
            return;
        }
        self.write_str_unlocked(text);
        self.write_byte(b'\n');
        unlock();
    }

    fn write_line_unused(&self, text: &str) {
        self.write_str(text);
        self.write_byte(b'\n');
    }
}
