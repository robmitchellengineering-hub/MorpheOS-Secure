//! COM1, by port I/O, because on M0 there is no driver model and nothing else that can speak before the Core
//! has an allocator, an interrupt table or a way to find a device.
//!
//! This is the Core's first output device and it exists to be *read by a test*. It is deliberately the least
//! interesting code in the repository: 16550 UART, 115200 8N1, polled.

use core::arch::asm;

const COM1: u16 = 0x3f8;

pub struct Serial {
    port: u16,
}

impl Serial {
    /// Initialise COM1. No allocation, no interrupts, no locking — one writer is all M0 has.
    pub fn new() -> Self {
        let mut serial = Self { port: COM1 };
        serial.init();
        serial
    }

    fn out(&self, offset: u16, value: u8) {
        // SAFETY: port I/O at CPL 0 to COM1, offset by a constant in this module. The port is present on every
        // x86 machine QEMU emulates, and `scripts/m0.sh` reads the result on the serial line.
        unsafe {
            asm!("out dx, al", in("dx") self.port + offset, in("al") value, options(nomem, nostack, preserves_flags));
        }
    }

    fn init(&mut self) {
        self.out(1, 0x00); // disable interrupts: M0 polls
        self.out(3, 0x80); // DLAB on
        self.out(0, 0x01); // divisor low  — 115200
        self.out(1, 0x00); // divisor high
        self.out(3, 0x03); // 8 bits, no parity, one stop
        self.out(2, 0xc7); // FIFO on, cleared, 14-byte threshold
        self.out(4, 0x03); // DTR + RTS
    }

    fn ready(&self) -> bool {
        let mut status: u8 = 0;
        // SAFETY: as above; reads the line status register.
        unsafe {
            asm!("in al, dx", in("dx") self.port + 5, out("al") status, options(nomem, nostack, preserves_flags));
        }
        status & 0x20 != 0
    }

    pub fn write_byte(&mut self, byte: u8) {
        while !self.ready() {
            core::hint::spin_loop();
        }
        self.out(0, byte);
    }

    pub fn write_line(&mut self, text: &str) {
        for byte in text.bytes() {
            self.write_byte(byte);
        }
        self.write_byte(b'\n');
        self.write_byte(b'\r');
    }
}
