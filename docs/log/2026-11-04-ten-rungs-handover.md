# 2026-11-04 · DeepSeek · ten rungs of the Core, and the eight bugs the interrupt rung cost

This is the handover. Read it before touching `core/`. Everything here is measured on `studio-linux2`; nothing is a
claim. The goal's real completion — a full AirMatrix show on one engine — is not reached, and this file says what is
between here and there.

## Green and merged, each with a falsifier proven able to fail

| rung | what it proves | PR | script |
|---|---|---|---|
| M0 | boots AArch64 (QEMU `virt`, `-cpu cortex-a76`), no Linux under it, reports itself | #1 | `scripts/m0.sh` |
| M1 | authority is an object a task was handed; both directions named; the mutation flips exactly one verdict | #2 | `scripts/m1.sh` |
| M2 | the ledger: generations, undo without a reboot, a tampered generation refused | #3 | `scripts/m2.sh` |
| M2b | the allocator: first fit, reuse, a double free refused | #3 | `scripts/m2b.sh` |
| M3a | our page tables; the guard page faults and the Core survives it | #5 | `scripts/mmu.sh` |
| M4a | a second execution context on its own stack, judged by the Core | #6 | `scripts/task.sh` |
| M4b | the task's own address space, a block absent from its map | #7 | `scripts/isolation.sh` |
| M4c | a task that oversteps is a dead task, not a dead machine | #8 | `scripts/isolation.sh` |
| M5a | the clock; 1 ms deadline honoured in 1.002 ms | #9 | `scripts/clock.sh` |
| M5b | the timer **interrupts** the Core (GICv2) | #11 | `scripts/tick.sh` |

**The rule that found every bug below**: a gate must be proven able to fail (`--self-test` on every script), and a gate
that passes while the machine prints `FAIL` is a broken gate.

## The interrupt rung's eight bugs — all real, all measured
1. **GICv2, not v3** — the machine's device tree says `arm,gic-v2m-frame`, so `ICC_*` system registers are undefined instructions.
2. `msr daifset/daifclr` take an **immediate**.
3. **`.intel_syntax` is x86** — never in an aarch64 block.
4. `irq_handler` was referenced and **never defined**.
5. A level-triggered interrupt must be **re-armed before the EOI**.
6. The code must run on **SP_EL1** (`spsel #1`), or the IRQ entry stacks on a zero pointer.
7. **Vector entries must be exactly 128 bytes apart** — a 250-byte body per slot put the IRQ entry inside a fault body. This one cost six rounds and was invisible: an interrupt was taken and never returned.
8. A bad replacement **duplicated the assembly block**, then left an orphan `.endm`.

## The lesson that cost the most
Every one of the ten rounds spent on M5b that was not a GIC bug was a **fragment edit of an assembly block**. The rungs
that went smoothly were the ones **rewritten in one pass** (`task.rs`, the vector table). Rule for next time: if a
block is being edited for the second time, rewrite the whole block instead.

## What is next, in order
1. **M5c preemption** — `M5c-PREEMPTION.md` has the design, the registers and the falsifier. Falsifier: two tasks that never yield; after 100 ms **both** counters are non-zero. Mutation `sched-mutant-no-switch` must leave the second at exactly zero.
2. **Per-task `TTBR0` on the switch** — M4b builds the tables; installing them as part of the switch is what makes "own address space" true *while the task runs*.
3. **M5d priorities** — audio beats everything, and a priority is a claim about a budget.
4. **M7a the schedulability certificate** — `CERTIFICATES.md`. A ledger generation carries `Σ(Cᵢ/Tᵢ) ≤ 1` exactly; `apply()` refuses one that does not check and names the term. This is the rung that makes *"one engine never drops"* arithmetic rather than hope, and it comes **before** the network.
5. **M0b the Raspberry Pi 5** — UEFI, our MMU tables, GIC, timers, RP1 PL011 at `0x1F22010000`, per-core. **Needs the Pi from Rob.**
6. **M0c the boot path's A/B**, then the OS services a show needs (UDP/RTP, ledger-backed state, the control API), then the show itself.

## What is not done, in one paragraph
No preemption, no scheduler, no priority class — the tick is counted, not used. No certificates. No network, no
storage, no Pi. **AirMatrix cannot run on this machine**, and the honest distance to a show is the OS-services rung:
`thread-priority` and `if-addrs` are in AirMatrix's dependency graph, and the Core has nothing for either yet.

## The baseline to match
AirMatrix's own workspace on `studio-linux2`: **204 tests passed, 0 failed**. That is the reference; anything this OS
claims about running AirMatrix is measured against it, not against a green light of our own making.
