//! Tasks — a second execution context, and what happens when it oversteps.
//!
//! Two claims, and they are the two a show depends on:
//!   1. the Core hands the CPU to code running on **its own stack**, and takes it back (M4a);
//!   2. if that code does something the machine cannot allow, the **task** dies and the machine continues (M4c).
//!
//! The second is what this file exists for now. A task that oversteps is a dead task, not a dead show — and the way
//! to get there without a scheduler is the smallest mechanism that works: before switching, the Core parks its own
//! stack pointer and names the function to resume at; the fault handler, recognising a task's violation, gives the
//! Core its stack back and tail-calls that function. The resume target never returns, so no return address has to be
//! saved and no label has to be computed inside assembly.
//!
//! An earlier version parked the Core's lr and computed an abort label with `adr` inside inline asm. It broke twice:
//! once because `in(reg)` operands can be allocated to the very registers the asm template used, and once because the
//! label's position in the template was not what the surrounding code needed. This version has neither, and that is
//! the point of rewriting it rather than patching it again.

/// A task's stack. 16 KiB, 16-byte aligned, static.
#[repr(align(16))]
pub struct Stack(pub [u8; 16 * 1024]);

pub static mut TASK_STACK: Stack = Stack([0; 16 * 1024]);

/// Where the task's frame actually was, recorded by the task. The Core compares it against the region it handed over;
/// that comparison is what makes "own stack" checkable rather than asserted.
pub static mut TASK_STACK_SEEN: usize = 0;

/// What the task returned. Passed through a static rather than an asm output — an asm output and a task's real return
/// value disagreed once, and one way to be wrong is enough.
pub static mut TASK_RESULT: u64 = 0;

/// The Core's stack pointer, parked before a switch and restored by an abort.
pub static mut CORE_SP: usize = 0;

/// The function an aborted task resumes at. It must not return.
static mut RESUME: usize = 0;

/// What a task runs.
pub type Entry = fn() -> u64;

/// Tell the switch where to go if the task has to be aborted. Called once, before any task runs.
pub fn set_resume(target: extern "C" fn() -> !) {
    // SAFETY: single writer, before any task exists.
    unsafe { RESUME = target as usize };
}

/// The stack region a task is given, as (bottom, top).
pub fn region() -> (usize, usize) {
    let bottom = core::ptr::addr_of!(TASK_STACK) as usize;
    (bottom, bottom + core::mem::size_of::<Stack>())
}

/// Run `entry` on `stack_top` and come back to the Core.
///
/// The only thing parked is the Core's sp: the abort path needs nothing else, because it resumes by tail-calling a
/// function that never returns. `tmp` is an `out(reg)` operand so the compiler chooses its register, and nothing in
/// this template names a register the compiler might also have chosen for an operand.
///
/// # Safety
/// `stack_top` must be the top of a writable, 16-byte aligned region large enough for the entry, and `set_resume`
/// must have been called with a function that never returns.
pub unsafe fn run(entry: Entry, stack_top: usize) {
    let mut tmp: usize = 0;
    // SAFETY: the caller guarantees the stack and the resume target.
    unsafe {
        core::arch::asm!(
            "mov {tmp}, sp",
            "str {tmp}, [{slot}]",
            "mov sp, {stack}",
            "blr {entry}",
            "ldr {tmp}, [{slot}]",
            "mov sp, {tmp}",
            slot = in(reg) core::ptr::addr_of_mut!(CORE_SP),
            stack = in(reg) stack_top,
            entry = in(reg) entry as usize,
            tmp = out(reg) tmp,
        );
    }
}

/// Return to the Core from a faulted task: hand it its stack pointer, then tail-call the resume function.
///
/// # Safety
/// Only valid while a task launched by `run` is executing, and only with the resume target set to a function that
/// never returns.
pub unsafe fn abort_to_core() -> ! {
    // SAFETY: both values were written before the task started.
    unsafe {
        let sp = CORE_SP;
        let resume = RESUME;
        core::arch::asm!(
            "mov sp, {sp}",
            "br {pc}",
            sp = in(reg) sp,
            pc = in(reg) resume,
            options(noreturn),
        );
    }
}
