//! Tasks — a second execution context, and what happens when it oversteps.
//!
//! Two claims, and they are the two a show depends on:
//!   1. the Core hands the CPU to code running on **its own stack**, and takes it back (M4a);
//!   2. if that code does something the machine cannot allow, the **task** dies and the machine continues (M4c).
//!
//! HOW THE SWITCH IS WRITTEN, AND WHY IT TOOK THREE TRIES. The first two versions used `asm!` with `in(reg)` and
//! `out(reg)` operands. Both failed: the compiler is entitled to allocate an operand to a register the template also
//! uses, so `mov x20, sp` could overwrite the pointer it was about to store through, and the second version stomped
//! the statics outright (`frame at 0x1, returned 0x1`). The fix is not another operand constraint: it is to stop
//! letting the compiler choose. The switch below is a **hand-written function in `global_asm!`** with a normal C
//! calling convention, so every register is decided here and the Rust side is an ordinary `extern "C"` call.
//!
//! WHAT IT IS NOT: this is a stack switch with a parked stack pointer. The task still shares the Core's page tables
//! when it is created (M4b gives a task its OWN tables, but the switch does not install them — that is the next rung).
//! There is no scheduler and no preemption: a task runs until it returns or oversteps.

/// A task's stack. 16 KiB, 16-byte aligned, static.
#[repr(align(16))]
pub struct Stack(pub [u8; 16 * 1024]);

pub static mut TASK_STACK: Stack = Stack([0; 16 * 1024]);

/// Where the task's frame actually was, recorded by the task; the Core judges it against the region.
pub static mut TASK_STACK_SEEN: usize = 0;

/// What the task returned, as seen by the task itself.
pub static mut TASK_RESULT: u64 = 0;

/// The Core's stack pointer, parked by the switch. `no_mangle` because the assembly names it.
#[unsafe(no_mangle)]
pub static mut CORE_SP: usize = 0;

/// The function an aborted task resumes at. It must not return. `no_mangle` for the same reason.
#[unsafe(no_mangle)]
pub static mut RESUME: usize = 0;

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

unsafe extern "C" {
    /// x0 = entry, x1 = stack_top. Park the Core's sp, switch, run the entry, come back with its value in x0.
    fn morph_task_start(entry: usize, stack_top: usize) -> u64;
    /// Restore the Core's sp and tail-call RESUME. Never returns.
    fn morph_task_abort() -> !;
}

use core::arch::global_asm;

global_asm!(
    r#"
    .section .text.task, "ax"

    /* extern "C" fn morph_task_start(entry: usize, stack_top: usize) -> u64 */
    .global morph_task_start
morph_task_start:
    stp x29, x30, [sp, #-16]!
    mov x29, sp

    /* Park the Core's stack pointer in CORE_SP, then switch to the task's. */
    adrp x9, CORE_SP
    add  x9, x9, :lo12:CORE_SP
    mov  x10, sp
    str  x10, [x9]

    mov  sp, x1          /* the task's stack */
    blr  x0              /* run the task; its return value arrives in x0 */
    mov  x19, x0         /* keep it across the switch back */

    adrp x9, CORE_SP
    add  x9, x9, :lo12:CORE_SP
    ldr  x10, [x9]
    mov  sp, x10         /* the Core's stack back */

    ldp x29, x30, [sp], #16
    mov x0, x19
    ret

    /* extern "C" fn morph_task_abort() -> ! */
    .global morph_task_abort
morph_task_abort:
    adrp x9, CORE_SP
    add  x9, x9, :lo12:CORE_SP
    ldr  x10, [x9]
    mov  sp, x10         /* the Core's stack back */

    adrp x9, RESUME
    add  x9, x9, :lo12:RESUME
    ldr  x11, [x9]
    br   x11             /* tail-call: the resume function never returns */
"#
);

/// Run `entry` on `stack_top` and come back to the Core with its value.
///
/// # Safety
/// `stack_top` must be the top of a writable, 16-byte aligned region large enough for the entry.
pub unsafe fn run(entry: Entry, stack_top: usize) -> u64 {
    // SAFETY: the caller guarantees the stack; the assembly function is the switch.
    unsafe { morph_task_start(entry as usize, stack_top) }
}

/// Return to the Core from a faulted task.
///
/// # Safety
/// Only valid while a task launched by `run` is executing, with a resume target that never returns.
pub unsafe fn abort_to_core() -> ! {
    // SAFETY: as above.
    unsafe { morph_task_abort() }
}
