//! PL011 UART — the serial port the machine talks to before it has a driver model.
//!
//! On QEMU's `virt` machine the PL011 sits at 0x0900_0000. On a Raspberry Pi 5 the same PL011 is the primary UART at
//! 0x1F_2201_0000 (RP1) — so this file is the *first* place the two machines differ, and the base address is a
//! parameter rather than a constant, because a Core that hard-codes one board's address is a Core for one board.
//!
//! Polled, 8N1, no interrupts: M0 has one writer and nothing else to do.

use core::ptr::{read_volatile, write_volatile};

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
        // A BOUNDED wait, deliberately. Measured on studio-linux2: the polled write hung the machine mid-line ("...it
        // was handed" with no newline), because the check spun on TXFF with no way out. On a boot path, a console
        // that can stop the Core is a worse fault than a dropped character — so this waits a long time, then writes
        // anyway and lets the hardware drop the byte if it must.
        const MAX_SPINS: u32 = 2_000_000;
        let mut spins = 0u32;
        // SAFETY: as above. FR is read-only in normal operation; DR accepts one byte while TXFF is clear.
        unsafe {
            while read_volatile(self.reg(FR)) & TXFF != 0 {
                if spins >= MAX_SPINS {
                    break;
                }
                spins += 1;
                core::hint::spin_loop();
            }
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

    pub fn write_line(&self, text: &str) {
        self.write_str(text);
        self.write_byte(b'\n');
    }
}
