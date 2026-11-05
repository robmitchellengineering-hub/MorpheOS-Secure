#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}

/// The entry the loader calls. It makes the four syscalls a loadable program is allowed to make, then returns 66.
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
    }
    66
}
