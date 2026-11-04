//! Tasks — a second execution context, rung M4a.
//!
//! THE CLAIM: the Core can hand the CPU to code that runs on **its own stack**, and take it back afterwards. That is
//! the smallest thing that must be true before a show can run: the audio engine, its control surface and every node
//! session are separate execution contexts, and none of them may run on the Core's own stack.
//!
//! WHAT THIS IS NOT YET, said here rather than implied: this is a **stack switch**, not an address-space switch. The
//! task shares the Core's page tables (M3a's identity map), so it can still name memory the Core owns — the capability
//! table refuses what it asks the *Core* for, but nothing stops it writing where it likes. Per-task TTBR0 (a real
//! address space, and the end of that gap) is the next rung, and it is the one that makes M1's refusals containment
//! for code that is not the Core itself.
//!
//! A task on this rung is a function and a stack. That is enough to prove the switch and to be honest about what is
//! missing.

/// A task's stack. 16 KiB, 16-byte aligned, static: the Core does not yet allocate stacks from the arena because an
/// allocator that can be asked for a stack during a switch is a new failure mode for a rung that is about switching.
#[repr(align(16))]
pub struct Stack(pub [u8; 16 * 1024]);

pub static mut TASK_STACK: Stack = Stack([0; 16 * 1024]);

/// What a task runs. Returns a value so the Core can check the task actually ran rather than only that it did not
/// crash.
pub type Entry = fn() -> u64;

/// Run `entry` on `stack_top` and return its value, with the Core's own stack pointer restored afterwards.
///
/// The switch is four instructions and it is all in assembly because there is nothing else that can do it: `sp` is
/// not accessible from stable Rust, and doing this through a "closure on a fake stack" would be a different, weaker
/// claim.
///
/// # Safety
/// `stack_top` must be the top of a writable, 16-byte aligned region at least as large as the entry's stack use, and
/// the entry must not return into the Core's stack.
pub unsafe fn run(entry: Entry, stack_top: usize) -> u64 {
    let mut result: u64;
    // SAFETY: the caller guarantees the stack. x19 (a callee-saved register) holds the Core's sp across the call.
    unsafe {
        core::arch::asm!(
            "mov x19, sp",
            "mov sp, {stack}",
            "bl {entry}",
            "mov sp, x19",
            stack = in(reg) stack_top,
            entry = in(reg) entry as usize,
            out("x19") _,
            lateout("x0") result,
        );
    }
    result
}
