//! PL011 UART — the serial port the machine talks to before it has a driver model.
//!
//! On QEMU's `virt` machine the PL011 sits at 0x0900_0000. On a Raspberry Pi 5 the same PL011 is the primary UART at
//! 0x1F_2201_0000 (RP1) — so this file is the *first* place the two machines differ, and the base address is a
//! parameter rather than a constant, because a Core that hard-codes one board's address is a Core for one board.
//!
//! Polled, 8N1, no interrupts: M0 has one writer and nothing else to do.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Held for the duration of one FLUSH, and nowhere else. Measured: taking a lock inside `write_str` broke composite
/// output, because `push_hex` re-enters through `write_str`, fails to acquire, and silently drops characters. A buffer
/// takes the lock once, writes the whole line, and releases - no nesting, so no re-entry to get wrong.
static CONSOLE: AtomicBool = AtomicBool::new(false);

/// QEMU `virt`: PL011.
pub const UART_QEMU_VIRT: usize = 0x0900_0000;
/// Raspberry Pi 5: the PL011 on the RP1 south bridge.
pub const UART_PI5: usize = 0x1F_2201_0000;

/// Where the console actually is. Resolved ONCE at boot from the firmware's device tree when one is passed, and from
/// the constant otherwise — because a hardcoded address is a memory, and the Pi's memory will be wrong.
static BASE: AtomicUsize = AtomicUsize::new(UART_QEMU_VIRT);

pub fn base() -> usize {
    BASE.load(Ordering::Relaxed)
}

pub fn set_base(address: usize) {
    BASE.store(address, Ordering::Relaxed);
}

const DR: usize = 0x00; // data register
const FR: usize = 0x18; // flag register
const TXFF: u32 = 1 << 5; // transmit FIFO full
const LCR_H: usize = 0x2c;
const CR: usize = 0x30;

/// A whole line, formatted first and written once. The type exists because two contexts sharing a console must not be
/// able to splice each other's lines: formatting is local, and only the flush is shared.
pub struct LineBuf {
    buf: [u8; 256],
    len: usize,
}

impl LineBuf {
    pub const fn new() -> Self {
        Self { buf: [0; 256], len: 0 }
    }

    pub fn push_str(&mut self, text: &str) {
        for byte in text.bytes() {
            self.push_byte(byte);
        }
    }

    pub fn push_byte(&mut self, byte: u8) {
        if self.len < self.buf.len() {
            self.buf[self.len] = byte;
            self.len += 1;
        }
    }

    /// Decimal, for counts a person reads.
    pub fn push_u64(&mut self, mut value: u64) {
        let mut digits = [0u8; 20];
        let mut n = 0;
        if value == 0 {
            self.push_byte(b'0');
            return;
        }
        while value > 0 {
            digits[n] = b'0' + (value % 10) as u8;
            value /= 10;
            n += 1;
        }
        while n > 0 {
            n -= 1;
            self.push_byte(digits[n]);
        }
    }

    /// Hex, for addresses a person compares.
    pub fn push_hex(&mut self, value: u64) {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        self.push_str("0x");
        let mut started = false;
        for shift in (0..16).rev() {
            let nibble = ((value >> (shift * 4)) & 0xf) as usize;
            if nibble != 0 {
                started = true;
            }
            if started || shift == 0 {
                self.push_byte(DIGITS[nibble]);
            }
        }
    }

    /// Write the line, whole, under one lock. If the console is busy the line is DROPPED: a diagnostic is not a
    /// guarantee, and spinning here would deadlock against the context that holds it.
    pub fn flush(&mut self, uart: &Uart) {
        if CONSOLE.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            self.len = 0;
            return;
        }
        for i in 0..self.len {
            uart.write_byte_raw(self.buf[i]);
        }
        uart.write_byte_raw(b'\n');
        CONSOLE.store(false, Ordering::Release);
        self.len = 0;
    }
}

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

    /// The byte writer with no framing, used by `LineBuf::flush` while it holds the console lock. Public because
    /// `LineBuf` lives in a different module and must not re-enter the locked path.
    pub fn write_byte_raw(&self, byte: u8) {
        self.write_byte(byte);
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
        self.write_str(text);
        self.write_byte(b'\n');
    }
}
