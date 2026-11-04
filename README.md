# MorpheOS Secure — a new type of computer, with a kernel that is its own thing

This repository is the **secure edition of MorpheOS**, built on a **new kernel** — not Linux, not a
distribution, not a Linux we configure and rename. The kernel here is called **Morph**: a capability-based
microkernel, written in Rust, in the spirit of seL4, Redox and Theseus, and *high-assurance first*.

Rob, on what this is: *"a new type of computer with the original ultimate secure version and plan"* — and on
what it is not: *"I dont want to build another windows … the whole point is the operating system reimagined …
we are meant to be reinventing the wheel here so we have a hover board not rehashing existing shit."*

## The one true sentence about the kernel

**No line of Morph exists yet.** There is no source tree, no boot, no capability model. What exists is the
scope, the first rung, and the acceptance tests that will decide whether it is real — all in
[KERNEL.md](KERNEL.md). Any claim beyond that sentence is not yet true.

What *does* exist, and is carried here, is everything a secure kernel has to satisfy:

| carried here | what it is |
|---|---|
| [acceptance/arm-charter.md](acceptance/arm-charter.md) | the security arm's charter and its measured scorecard — the attacks that must fail, with today's verdicts |
| [acceptance/attacks/](acceptance/attacks/) | the real attackers: port scan, console credential, hostile update, the disk alone (A1–A7), each with fixtures that must pass **and** be caught, and an explicit UNKNOWN |
| [PLAN.md](PLAN.md) | the security plan, and what each control becomes when the kernel is ours instead of borrowed |
| [SEALED-STATE.md](SEALED-STATE.md) · [ATTESTATION.md](ATTESTATION.md) · [MEASURED-BOOT.md](MEASURED-BOOT.md) | the three design documents that state what the machine must be able to prove about itself, and exactly where the claim stops |
| [adr/0022-own-the-kernel-build.md](adr/0022-own-the-kernel-build.md) | the record of the step before this one: we built Linux ourselves. That was the platform the tests ran on, not the destination |

## What is deliberately not here

- **The everyday MorpheOS image, builder and design set.** Different project, different repo. This one does not
  import it, wrap it, or add flags to it.
- **Linux.** Not as the kernel, not as the foundation. Where Linux remains, it is a *driver host inside an
  isolated VM* — a device the new kernel does not trust — and that is stated in [KERNEL.md](KERNEL.md) rather
  than implied.

## The rule this repository holds itself to

**A claim with no attack is not a claim.** Every statement in these documents is either something an attacker
has tried and failed against, or it says plainly that it has not been tried. `UNKNOWN` is not a pass.

## The state of the repo today

Documents, the arm's plan, the attacks, and the design set for what the kernel must prove. No kernel code, no
image, no artefact. The first rung that would change that is the first item in [KERNEL.md](KERNEL.md).
