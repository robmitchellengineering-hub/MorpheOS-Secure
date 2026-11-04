# The chunk, closed — handover for the next session

Six items were set; five are done and measured, and the sixth needs a Raspberry Pi 5 on a desk.

## Green on `main`, sixteen gates, one command
`scripts/all.sh` boots the Core sixteen times and prints a table. **16 green, 0 red.** Every gate has a mutation
(`--self-test`) that proves the check can fail, and every gate has been shown able to be *reached* — a gate that
cannot reach its subject reports silence, which is how `m2b` stayed red for rounds without anyone noticing.

| gate | what it proves |
|---|---|
| `m0` | boots on AArch64, no Linux under it |
| `m1` | authority is an object a task was handed; both directions named |
| `m2` | the machine is its history — undo, tamper refused |
| `m2b` | the memory it owns — first fit, reuse, double free refused |
| `mmu` | our own page tables; unmapped memory is unreachable and the Core survives it |
| `task` | a second context on its own stack, the Core getting its stack back |
| `isolation` | a task that oversteps is a dead task, not a dead machine |
| `clock` | its own clock, 1 ms honoured |
| `tick` | the timer interrupts it |
| `sched` | the policy alternates |
| `preempt` | **two tasks, one CPU, neither yielding** |
| `prio` | the audio class takes every pick while it is ready |
| `cert` | **a generation that cannot prove it fits never becomes history** |
| `cap` | **authority only ever decreases** |
| `box` | **a box is validated once at load, not masked per access** |
| `fdt` | the Core reads the firmware's device tree (the Pi's boot protocol) |

## The five hard lessons, all bought and paid for
1. **A gate's environment must be identical to the one it was proven in.** Running the FDT demo on every boot turned
   `preempt` red on the fifteen gates that have no tree.
2. **Verify the instrument before trusting the reading.** Three rounds were spent suspecting a device-tree walker that
   was correct; the branch it sat on did not compile, and the `E0133` lines being read as errors were lint warnings.
3. **A mechanism must not depend on mutable global state.** Resolving the console through a global *inside the task
   entry* broke the task's return path — measured, then reverted.
4. **Print what the machine does.** `x0` was `0`, then `52 instead of 56`, then "no marker at offset 0" — each found by
   an observation, never by a better guess. Four separate facts, four rounds, zero correct hypotheses.
5. **A line-range move swallows what sits inside the cut.** Five demos travelled into a 176-line preemption block and
   vanished from the boot. If a block has to be edited twice, rewrite it whole.

## What is next, in order
1. **The Pi 5 (M0b).** Needs: the board, power, a serial console. The software side is ready — the Core boots as an
   arm64 `Image`, keeps `x0`, reads the DTB, and can report what it finds. See `PI-BRINGUP.md`. The one open design
   item is the console base on the Pi: the reader finds it at boot, but the print sites still use the constant, and the
   attempt to make them ask a global broke `task`. Fix by **handing the base in**, not looking it up mid-flight.
2. **M0c** — the boot path's A/B, one-shot trial and fallback.
3. **M7b** — the Lean half of the bounded check.
4. Then the OS services a show needs: UDP/RTP, ledger-backed state, control API, conductor-mode APs.

## What is NOT done, stated plainly
No network, no storage, no hardware. **AirMatrix cannot run on this Core.** Its own 204 passed / 0 failed on
studio-linux2 remains the yardstick — not a green light of our own making.
