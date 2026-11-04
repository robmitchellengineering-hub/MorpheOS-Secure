# M5c SWITCH — the four instructions, written out

The last piece before preemption is real. Everything around it exists and is green; this file is the recipe so the
next session does not re-derive it — or, worse, do it in fragments.

## What already exists (all merged, all measured)
- **The decision**: `core/src/sched.rs` — two ready tasks, round-robin, `None` when nothing is ready, a switch counter.
- **The frame**: `sched::install_frame(stack_top, entry)` builds 288 bytes — 31 registers at 0..240, `x30` at 240,
  `ELR` at 248, `SPSR` at 256 — with `SPSR = 0x5` (EL1h, interrupts on). `sched::frame_state(frame)` reads the two
  resumption slots back, and the boot already checks them.
- **The save path**: `SAVE_ALL` writes the 288-byte frame, including `ELR` and `SPSR`.
- **The restore path**: `RESTORE_ALL` restores the 31 registers and leaves `ELR`/`SPSR` alone — **on purpose**: the
  fault handler rewrites `ELR` (+4 to resume after the faulting load) and two green rungs depend on it.

## The change, and it is small
1. **A second restore macro**, `RESTORE_ALL_WITH_STATE`: the same as `RESTORE_ALL`, plus
   `ldr x9, [sp, #248]; msr elr_el1, x9; ldr x9, [sp, #256]; msr spsr_el1, x9` **before** the register restores.
2. **`irq_entry` passes and takes a stack pointer**:
   ```
   irq_entry:
       SAVE_ALL
       mov x0, sp
       bl  irq_switch        // returns the frame to resume from, in x0
       mov sp, x0
       RESTORE_ALL_WITH_STATE
       eret
   ```
3. **`irq_switch(sp) -> sp`** in Rust: count the tick, re-arm **before** the EOI (proved necessary in M5b), then —
   if switching is enabled — record `CTX[current] = sp`, `next = sched.pick_next()`, return `CTX[next]`. With switching
   disabled it returns `sp` unchanged, which is exactly today's behaviour.
4. **The first switch**: `CTX[B]` is the frame from `install_frame`, so the first time the scheduler picks B, the
   handler returns that frame and `RESTORE_ALL_WITH_STATE` + `eret` starts B **for the first time** — B's `ELR` is its
   entry and its `SPSR` is EL1h. That is the whole trick, and it is why the frame is checked before use.

## The falsifier (write it before the code)
Two tasks that never yield. Each loops incrementing its own counter and prints exactly once when it is done:

| | |
|---|---|
| **expected green** | both `TASK A: ran` and `TASK B: ran` appear, each with a counter above the threshold, within 100 ms |
| **mutation** (`sched-mutant-no-switch`) | `irq_switch` returns `sp` unchanged: **`TASK B: ran` is absent**, and the check goes red |
| **why a sequence and a mutation** | a scheduler that always resumes the same task looks exactly like a working scheduler; only a line that cannot print unless another context ran will catch it |

## The three ways this goes wrong, in the order they are likely
1. **`SPSR` wrong** — `0x5` is EL1h with interrupts on. Zero puts the CPU in EL0 with interrupts masked and nothing
   after the first `eret` makes sense. The frame is already checked for this, so it should not happen.
2. **The frame's `sp` alignment** — 288 bytes and 16-byte aligned; `install_frame` already forces `& !0xf`.
3. **Restoring `ELR` on the fault path** — this was tried and broke M3a and M4c. The fault path must keep using
   `RESTORE_ALL`. Only `irq_entry` uses the state-restoring variant. This is written here because it is the mistake
   already made once.

## What the switch does not do, and the rungs after it
- No priorities: round-robin only. **M5d** is "audio beats everything", and a priority is a claim about a budget.
- No per-task `TTBR0` on the switch: the task's own tables are installed only by the M4b probe. Putting them in the
  switch is what makes "own address space" true *while a task runs*, and it is the rung after this one.
- No timer-driven wakeups, no blocking, no sleep.

And then **M7a**: a ledger generation carrying `Σ(Cᵢ/Tᵢ) ≤ 1` exactly, so *"one engine never drops"* is arithmetic
rather than hope. That is the rung this whole chain has been walking toward, and it comes before the network.
