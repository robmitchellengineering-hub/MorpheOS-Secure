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

/// Where the task's stack actually was, recorded by the task itself. The Core compares it against the region it
/// handed over — that comparison is what makes the "own stack" claim checkable rather than asserted.
pub static mut TASK_STACK_SEEN: usize = 0;

/// What the task returned. Passed through a static rather than an asm output: measured, the `lateout("x0")` capture
/// disagreed with the value the task actually produced, and a switch that also has to marshal its result through a
/// register constraint is a switch with two ways to be wrong. The task writes it; the Core reads it.
pub static mut TASK_RESULT: u64 = 0;

/// The task stack region as (bottom, top), so a caller can check an address is inside it.
pub fn region() -> (usize, usize) {
    let bottom = core::ptr::addr_of!(TASK_STACK) as usize;
    (bottom, bottom + core::mem::size_of::<Stack>())
}

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
pub unsafe fn run(entry: Entry, stack_top: usize) {
    // SAFETY note is on the function; the result arrives through TASK_RESULT.
    // SAFETY: the caller guarantees the stack. x20 holds the Core's sp (x19 is reserved by LLVM and cannot be an asm operand) across the call.
    unsafe {
        core::arch::asm!(
            "mov x20, sp",
            "mov sp, {stack}",
            "blr {entry}",
            "mov sp, x20",
            stack = in(reg) stack_top,
            entry = in(reg) entry as usize,
            out("x20") _,
        );
    }
}
