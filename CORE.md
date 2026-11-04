# CORE — the Core: a capability microkernel, and the only thing in the machine that is trusted

**State, measured on studio-linux2 (2026-11-04).** Ten rungs are green and merged, each with a falsifier proven
able to fail:

- **M0** boots on AArch64 (QEMU `virt`, `-cpu cortex-a76`) with no Linux under it and reports itself — PR #1.
- **M1** authority is an object a task was handed; both directions named; the mutation flips exactly one verdict — PR #2.
- **M2** the ledger: generations, undo without a reboot, a tampered generation refused — PR #3.
- **M2b** the allocator: first fit, reuse, a double free refused — PR #3.
- **M3a** the MMU: our own tables, Device and Normal memory, a guard page that faults and a Core that survives it — PR #5.
- **M4a** a second execution context on its own stack, judged by the Core — PR #6.
- **M4b** the task's own address space, with a block that does not exist in it — PR #7.
- **M4c** a task that oversteps is a dead task, not a dead machine — PR #8.
- **M5a** the machine's clock: the counter advances, 1 ms honoured in 1.002 ms — PR #9.
- **M5b** the timer **interrupts** the Core (GICv2, sixteen four-byte vector entries, re-arm before EOI) — PR #11.

**What is NOT done, in the same breath**: the tick is counted, not used — no preemption, no scheduler, no priority
class. No certificates (M7a–M7d), no network, no storage, no Raspberry Pi. AirMatrix cannot run on this machine yet.

```
MORPHEOS-CORE 0.0.1: the Core is running on aarch64; no Linux, no syscalls, no processes
MORPHEOS-CORE: reported; halting
```

and `scripts/m0.sh --self-test` goes red on a deliberately wrong banner, so the gate is not theatre. **What is not
done: no capability space (M1), no ledger (M2), no Pi 5 hardware, no MMU, no GIC, no timers.**

**The machine is a Raspberry Pi 5** — AArch64, Cortex-A76. That decides the architecture of everything here: no
real mode, no multiboot, no long-mode switch; the firmware or EDK2 UEFI hands over an AArch64 image and the Core
takes it from there. The x86 loader in `loader/` is kept because it was measured, not because it is the path.

This is the scope and the first rung, written so the first line of code has something to be judged against.

## What it is

**The Core** — a capability microkernel written in Rust, crate `morph-core`. It is not called a kernel in the
product, because that word imports four assumptions this machine rejects (a syscall boundary, processes, a
filesystem, and a privileged core that *checks* authority). The Core mediates by handing out capabilities over
versioned objects; there is no ambient authority to check. For engineers, "capability microkernel" is the honest
classification, in the seL4 family.

Four properties, structural rather than enforced on top:

1. **No ambient authority exists.** A task holds capabilities; there is no "user" whose whole home and whole
   network are reachable. A denied action is not a policy someone applied — it is an object the task does not
   have.
2. **The machine is its own history.** State is generations, not mutable files; a change is a signed transition,
   and "undo" is descending to a node, not restoring a backup.
3. **The Core reports its own failure.** A crash leaves evidence it wrote, readable after a hard
   lockup, and a bad generation is never the one descended from.
4. **The machine can prove what it booted** — a measured chain from the loader, with the hardware seal where a
   TPM exists, and an honest "cannot prove it here" where it does not.

That is a different computer, not a hardened one: the unit is a **task with intent** holding **capabilities**
over **versioned state**, and the display is a projection of that state.

## What it must never do

- **Never become another Windows, or another Linux.** If the answer to a design question is "because that is how
  Unix does it", the answer is wrong until it is re-derived from the four properties above.
- **Never grow a syscall table, processes or a filesystem.** Those are the words that would make it a Unix again.
  A task holds capabilities; the ledger holds state; there is no third thing.
- **Never claim a security property the acceptance suite has not attacked.** `acceptance/attacks/` decides.
- **Never quietly become a driver project.** See hardware, below.

## The workload this machine exists for

**AirMatrix** (`airmatrix-dsh`: `amt-core`, `amt-dsp`, `amt-engine`, `amt-matrix`, `amt-node`, …) is Rust, and it
needs three things this Core does not have yet: an allocator and tasks (5 crates use `std::thread`), state I/O (5 use
`std::fs`, 7 use `std::net`), and **64-bit audio plugins** — arbitrary native AArch64 shared libraries, loaded and
run untrusted on a real-time deadline. [AUDIO-AND-PLUGINS.md](AUDIO-AND-PLUGINS.md) states what that costs and what
the falsifiers are. It is the reason the capability model and the ledger come before the audio path, not after.

## Hardware — the cost, stated once

This estate's machines are laptops: Wi-Fi (`b43`, `iwlwifi`), a panel (`i915`, `amdgpu`), audio
(`snd-hda-intel`). A new kernel has no drivers for any of them, and there are three honest ways to get them:

1. **Native drivers, written for this kernel.** A driver per chip, forever. That is where most new operating
   systems disappear, and we will not pretend otherwise.
2. **Foreign drivers in isolated boxes** — a vendor or Linux driver running as **untrusted code in a VM the Core
   controls**, holding only the capabilities it was handed. A driver crash takes down one box, not the machine; a
   malicious driver has nothing to escalate from. This is the plan: Linux is a *device provider*, never the
   operating system.
3. **No driver yet.** For the first rungs, QEMU's virtio devices are the hardware, and that is enough to prove a
   kernel.

Consequence, written before it is discovered: the first bootable Core has **no Wi-Fi, no panel and no audio**
until a driver box exists for them. That is the price of a Core that is its own thing.

## The rungs, each with what it proves

| rung | what we build | it proves | falsifier |
|---|---|---|---|
| **M0** | **done, measured** — builds for AArch64 and boots on QEMU `virt`, with the check proven able to fail | the Core runs on the Pi's architecture with no OS under it | the build fails, or `--self-test` passes on a deliberately wrong line |
| **M0b** | **the same image on the Pi 5 through its UEFI** (QEMU half done) | the Core runs on the actual machine | no banner on the Pi's serial line; or a banner without the Core reporting it finished |
| **M0c** | **our boot path with two generations, a one-shot trial and a fallback** | the machine cannot be left with a Core that does not come up | a corrupted newer generation is not rolled back; a generation that failed health is tried twice |
| **M1** | capabilities: a task granted one object and refused another, with the refusal coming from the Core | authority is objects, not users | a task reaches an object the kernel did not grant |
| **M2** | the history: state as signed generations; undo to an earlier generation without a reboot | the machine is its history | an undo loses data, or a tampered generation is accepted |
| **M3** | the failure path: a hard lockup leaves Core-written evidence, and a bad generation is not descended from | the machine explains itself without userspace | no evidence after a lockup, or a bad generation boots again |
| **M4** | the seal: measured boot, and a statement of what was booted that a verifier accepts | the machine can prove itself | a tampered build produces a statement that verifies |
| **M6** | the box: a foreign AArch64 ELF64 plugin loads, runs untrusted, processes a buffer, and cannot leave its box | the machine runs the workload it exists for, safely | the plugin reaches a file, a socket or another task's memory; or a crashing box takes the audio graph with it |
| **M5** | the first driver box: one real device (a panel), hosted by foreign driver code inside a VM the Core does not trust | hardware without trusting a driver | the driver box reaches memory or a device it was not given |

**M0 is next, and it is small.** One rung at a time; nothing is claimed before its falsifier has been run.

## What the existing suite already decides for it

The security arm's attacks are not Linux-specific, they are questions about a machine:

- `attack-console.sh` — does the console demand a credential before it gives a shell? Under the Core the answer
  must be structural: there is no console shell to reach without a capability.
- `attack-disk.sh` — is a credential readable off the disk at rest? Under the Core, state is sealed or it is not
  state.
- `attack-update.sh` — is an unsigned or tampered release refused?
- `attack-probe.sh` — what listens, and under what authority?

They are kept here as `acceptance/` because they are the standard the new kernel will be held to, and because a
kernel for a new type of computer that cannot pass them is a different computer with the same holes.

## Why a Core, and not an operating system assembled above one

Because the four properties above cannot be enforced from above. A policy applied by a platform is a policy that
platform can decline; a sandbox built by the thing being sandboxed is a promise. A capability is **an object the
Core either handed you or did not** — there is no path around it, because there is no path.

That is the whole argument, and it is why this project writes a Core instead of configuring one: the properties
that make this a different kind of computer are properties of the Core, not settings.

## Never breaks, heals itself — from the first boot, not retrofitted

This is the wedge of the whole OS, and it is a property of the **boot path**, not a feature added later. The loader
keeps **two generations of the Core**, and the rule is the one the estate already proved on the everyday build:

1. **A new generation is tried once.** It gets one boot to reach a health signal. There is no boot counter to get
   stuck on, and no "try again" that turns a bad image into a slow failure.
2. **It commits only on health.** Reaching the health signal is what makes the new generation the default; anything
   else — a panic, a hang, silence — is a failed trial.
3. **Failure falls back, and says so.** The previous generation boots, the machine records that it happened, and the
   record survives the failure (that is rung M3's job: evidence the Core wrote, readable after a reboot).
4. **The machine rebuilds the spare.** A slot that failed is not left dead — it is rewritten from the generation that
   works.

**Falsifiers, and they are the rung's whole point:** corrupt the newest generation on disk and the machine must still
reach the older one; give it a generation that boots and never signals health and it must not be tried a second
time; and after either, the record must name what was tried and what was refused.

**Why the loader first.** QEMU's own multiboot loader refuses a 64-bit image outright — measured on this rung:
*"Cannot load x86-64 image, give a 32bit one."* So writing our own loader is not a refinement of M0, it is the thing
that makes any 64-bit Core boot at all, and it is where A/B belongs: a boot path retrofitted with two generations
later is a boot path that was designed with one.

## The words this project uses, and refuses

| word | what it means here | refused because |
|---|---|---|
| **the Core** | the only trusted component: capability space, memory, scheduling, the ledger's enforcement | "kernel" imports syscalls, processes and a filesystem |
| **capability space** | the objects a task holds; authority is what it was handed | "permissions", "user", "root" |
| **the ledger** | the machine's state as signed generations | "filesystem", "install" |
| **the seal** | what the machine can prove about what it booted | "trust", "secure boot" as a slogan |
| **the projection** | the display as a view of state and time | "desktop", "window manager" |
| **driver box** | foreign driver code in a VM holding only its capabilities | "driver" as trusted kernel code |
