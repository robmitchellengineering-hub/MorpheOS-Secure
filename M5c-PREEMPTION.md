# M5c — PREEMPTION: make the tick decide what runs

The rung after M5b. The tick currently **counts**; nothing chooses between two runnable contexts. This is the first
rung where the machine *decides* — and therefore the first where a bad decision costs a deadline.

## The claim

> The timer interrupt can take the CPU away from a running task and give it to another, and the interrupted task
> resumes exactly where it was.

## What it takes, in this machine's terms

1. **A context**: 31 general registers (x0–x30) and the stack pointer. The IRQ entry already saves x0–x30 into a
   256-byte frame on the interrupted stack (M5b) — preemption needs that frame to belong to a *task*, not to a literal.
2. **A per-task frame**: a static `[u64; 32]` per task, holding sp and the saved registers. `x30` doubles as the
   resume address, so a "context switch" is: save the outgoing task's sp into its slot, load the incoming task's sp,
   and return from the interrupt — the `eret` then resumes the *other* task.
3. **A ready list**: two tasks is enough. Round-robin, one tick each. No priorities yet: this rung proves the
   *mechanism*, and M5d is where a class (audio beats everything) is added.
4. **Arming discipline**: the timer is re-armed **before** the EOI (proved in M5b — the difference between a tick and
   a storm).

## The falsifier, and the mutation

| | |
|---|---|
| **the test** | two tasks, each incrementing its own counter in a loop. Neither yields. Task A loops forever **without** calling anything. |
| **green means** | after 100 ms, **both** counters are non-zero, and the sum is within 20% of what one task would do alone — the CPU was shared, not stolen |
| **mutation** (`sched-mutant-no-switch`) | the IRQ entry returns to the task it interrupted. Task B's counter stays **exactly zero**, so the check must go red |
| **a second falsifier** | the interrupted task's registers survive: a task keeps a known pattern in x1–x10 across dozens of switches and verifies it |

**Why that mutation is the right one**: a scheduler that always switches back to the same task looks like a
scheduler — it takes interrupts, restores state, and prints nothing — and only a counter that *cannot* move unless
another context ran will catch it.

## What this rung does not do, stated before it is discovered

- **No priorities.** Round-robin only. Audio beats everything is M5d, and it needs a deadline-aware choice, which is
  also where the certificate (M7a) starts to matter: a priority is a claim about a budget, and the certificate is how
  that claim is checked rather than asserted.
- **No timer-driven wakeups.** Tasks run when they are runnable, and there is no sleep/block yet.
- **No per-task page tables on the switch.** M4b builds them and the falsifier exercises them, but installing a
  task's `TTBR0` as part of the switch is the rung after this one, and it is where the task's "own address space"
  becomes true *while it runs* rather than merely during a probe.

## Order
M5c (this) → per-task `TTBR0` on the switch → M5d priorities with a certificate → M7a the schedulability certificate
that decides whether a set of tasks may be admitted at all. The last of those is the one that makes *"one engine never
drops"* arithmetic instead of hope, per CERTIFICATES.md.
