# MorpheOS Secure — a new type of computer, with a Core that is its own thing

This repository is the **secure edition of MorpheOS**, built on **the Core** — not Linux, not a distribution,
not a Linux we configure and rename. **The Core** — internally `morph-core` — is a capability microkernel
written in Rust, in the spirit of seL4, Redox and Theseus, and *high-assurance first*.

It is called the Core rather than the kernel because the word "kernel" imports four assumptions this machine
rejects: a syscall boundary, processes, a filesystem, and a privileged core that *checks* authority. The Core
hands out capabilities over versioned objects; there is no ambient authority to check.

Rob, on what this is: *"a new type of computer with the original ultimate secure version and plan"* — and on
what it is not: *"I dont want to build another windows … the whole point is the operating system reimagined …
we are meant to be reinventing the wheel here so we have a hover board not rehashing existing shit."*

## The one true sentence about the Core

**No line of the Core exists yet.** There is no source tree, no boot, no capability model. What exists is the
scope, the first rung, and the acceptance tests that will decide whether it is real — all in
[CORE.md](CORE.md). Any claim beyond that sentence is not yet true.

What *does* exist, and is carried here, is everything a secure Core has to satisfy:

| carried here | what it is |
|---|---|
| [acceptance/arm-charter.md](acceptance/arm-charter.md) | the security arm's charter and its measured scorecard — the attacks that must fail, with today's verdicts |
| [acceptance/attacks/](acceptance/attacks/) | the real attackers: port scan, console credential, hostile update, the disk alone (A1–A7), each with fixtures that must pass **and** be caught, and an explicit UNKNOWN |
| [PLAN.md](PLAN.md) | the security plan, and what each control becomes when the Core is ours instead of borrowed |
| [SEALED-STATE.md](SEALED-STATE.md) · [ATTESTATION.md](ATTESTATION.md) · [MEASURED-BOOT.md](MEASURED-BOOT.md) | the three design documents that state what the machine must be able to prove about itself, and exactly where the claim stops |

## What is deliberately not here

- **The everyday MorpheOS image, builder and design set.** Different project, different repo. This one does not
  import it, wrap it, or add flags to it.
- **Linux as a foundation.** This operating system is not built on it, not configured from it, and not renamed
  out of it. Where a Linux *driver* is the only way to talk to a chip, that driver runs as **foreign code inside
  an isolated VM** — a compatibility device the Core does not trust — and [CORE.md](CORE.md) says so in
  those words rather than implying it.

## What it is for, first

**Run all the AirMatrix software on a machine that cannot be talked into betraying it** — `airmatrix-dsh` (Rust room
acoustics, DSP and measurement) and its **64-bit audio plugins** (foreign AArch64 shared libraries, untrusted, on a
real-time deadline). The first hardware is a **Raspberry Pi 5**, because it is AArch64, cheap, and honest about its
firmware. [AUDIO-AND-PLUGINS.md](AUDIO-AND-PLUGINS.md) is the scope of the plugin work.

## The wedge, in the boot path

**Never breaks, and heals itself** is not a feature of this OS; it is the first thing the loader does. Two generations
of the Core, **one** trial boot for a new one, commit only when the Core reaches a health signal, automatic fallback
to the generation that works, and a record of what was tried that survives the failure. A boot path retrofitted with
two generations later is a boot path designed with one. [CORE.md](CORE.md) states the rule and its falsifiers.

## The rule this repository holds itself to

**A claim with no attack is not a claim.** Every statement in these documents is either something an attacker
has tried and failed against, or it says plainly that it has not been tried. `UNKNOWN` is not a pass.

## The state of the repo today

Documents, the arm's plan, the attacks, and the design set for what the Core must prove. No Core code, no
image, no artefact. The first rung that would change that is M0, in [CORE.md](CORE.md).
