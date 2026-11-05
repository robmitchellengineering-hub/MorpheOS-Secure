#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

use rob_core::rtp::{self, RobCore, RtpHeader, MODE_BIT_EXACT, ROB_VERSION};

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}

/// The payload a show's audio rides in (placeholder: a ramp; the DSP path and
/// the plugins are deferred by Rob). 48 bytes = 16 mono 24-bit samples at 1 ms.
const PAYLOAD_BYTES: usize = 48;

/// The entry the loader calls. It makes the four syscalls a loadable program is
/// allowed to make, then sends its OWN RTP packet — the AMT/ROB-shaped header
/// (V=2, X=1, the 0xBEDE `rob-core` extension) built with rob-core's
/// `rtp::write_header` — and returns 66.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> u64 {
    unsafe {
        // report (0)
        asm!("svc #0", inout("x0") 0u64 => _, options(nostack));
        // apply_certified (1, arg 42)
        asm!("svc #0", inout("x0") 1u64 => _, inout("x1") 42u64 => _, options(nostack));
        // yield (2)
        asm!("svc #0", inout("x0") 2u64 => _, options(nostack));
        // send_udp (3)
        asm!("svc #0", inout("x0") 3u64 => _, options(nostack));

        // send_rtp (4): the engine's own RTP packet, built here and handed to
        // the Core to transmit. x1 = pointer, x2 = length.
        let mut packet = [0u8; 12 + 16 + PAYLOAD_BYTES];
        let header = RtpHeader {
            marker: false,
            payload_type: 96,
            sequence: 100,
            timestamp: 4800,
            ssrc: 0xC0FF_EE00,
        };
        let core = RobCore {
            version: ROB_VERSION,
            mode: MODE_BIT_EXACT,
            path_id: 0,
            slot: 0,
            budget_us: 0,
            send_time_us: 0,
        };
        // The element id is negotiated in SDP (`a=extmap`); 1 is the default.
        let hlen = match rtp::write_header(&mut packet, &header, Some((1, &core))) {
            Ok(n) => n,
            Err(_) => return u64::MAX,
        };
        for (i, b) in packet[hlen..].iter_mut().enumerate() {
            *b = i as u8; // a ramp keyed to the payload position, so the wire shows the packet is ours
        }
        let len = hlen + PAYLOAD_BYTES;
        let ptr = packet.as_ptr() as u64;
        asm!(
            "svc #0",
            inout("x0") 4u64 => _,
            in("x1") ptr,
            in("x2") len as u64,
            options(nostack)
        );
    }
    66
}
