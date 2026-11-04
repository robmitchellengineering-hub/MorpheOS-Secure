# 2026-11-04 · the preemption branch: what works, what does not, and the one thing that unblocks it

Branch `core/preempt`, unmerged. `main` is green at PR #21 and is **not** affected by anything here.

## What works
The switch **moves the CPU between two tasks.** Task B did not exist before this branch: it is started for the first
time by `irq_switch` returning the frame `sched::install_frame` built (`ELR` = its entry, `SPSR` = `0x5`), and the
interrupt entry restores it with `RESTORE_ALL_WITH_STATE`. Its iteration count appeared on the console. That is a
context switch, not a demonstration of one.

## What does not work
**The console cannot report two tasks.** A line from task A and a line from task B interleave, and no gate can read
the result — a working switch looks like a failure:

```
TASK B: 30ran 0x0xd40 iterations1388 — and it was interrupted along the way
```

That line is `TASK A: ran 0x30d40` and `TASK B: ran 0x1388` spliced together.

## The thing that was tried and reverted
A `try_lock` on the console. It made the output **worse**, not better, and the reason is worth keeping: `write_hex`
calls `write_str`, which re-enters the lock, fails, and **silently drops characters** — so composite output lost digits
while the tasks still interleaved. Reverted in `da88e99`.

## The fix, and it is a design change
**`LineBuf`**: format a whole line into a buffer — `push_str`, `push_hex`, `push_u32` — then flush it with **one**
locked write. No nested locking, no `write_hex` calling `write_str`. Every print site moves to it; there are a few
dozen across `main.rs`, `cap.rs`, `ledger.rs`, `sched.rs`, `gic.rs`, `clock.rs`.

It is mechanical, it touches six files, and it is the prerequisite for **every** multi-task measurement from here on —
including the two-task falsifier, priorities, and the certificate's own test output.

## After that, in order
1. Print `sched::moved()` (already recorded by `on_irq`) and re-run `scripts/preempt.sh`. That number separates
   *"the switch never comes back to task A"* from *"it comes back but task A is starved"* — two different faults the
   garbled log cannot distinguish.
2. Merge the preemption rung with both counters and the mutation (`sched-mutant-no-switch` must leave `TASK B: ran`
   absent).
3. Per-task `TTBR0` on the switch — the rung that makes "own address space" true *while* a task runs.
4. **M5d priorities** — audio beats everything; and a priority is a claim about a budget, which is where M7a's
   certificate begins.

## What is not claimed, in the same breath
Preemption is not done. There is no priority class, no certificate, no network, no storage, no Pi. AirMatrix cannot run
on this machine, and its own 204/0 remains the reference.
