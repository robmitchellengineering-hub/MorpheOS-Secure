# KERNEL — Morph: the kernel that is its own thing

**State: specified, not built. No source, no boot, no capability model. Zero lines of kernel code exist.**

This is the scope and the first rung, written so the first line of code has something to be judged against.

## What it is

A capability-based microkernel in Rust, for the secure edition, with these properties as **structural** rather
than enforced-on-top:

1. **No ambient authority exists.** A task holds capabilities; there is no "user" whose whole home and whole
   network are reachable. A denied action is not a policy someone applied — it is an object the task does not
   have.
2. **The machine is its own history.** State is generations, not mutable files; a change is a signed transition,
   and "undo" is descending to a node, not restoring a backup.
3. **The kernel reports its own failure.** A crash leaves evidence the kernel wrote, readable after a hard
   lockup, and a bad generation is never the one descended from.
4. **The machine can prove what it booted** — a measured chain from the loader, with the hardware seal where a
   TPM exists, and an honest "cannot prove it here" where it does not.

That is a different computer, not a hardened one: the unit is a **task with intent** holding **capabilities**
over **versioned state**, and the display is a projection of that state.

## What it must never do

- **Never become another Windows, or another Linux.** If the answer to a design question is "because that is how
  Unix does it", the answer is wrong until it is re-derived from the four properties above.
- **Never claim a security property the acceptance suite has not attacked.** `acceptance/attacks/` decides.
- **Never quietly become a driver project.** See hardware, below.

## Hardware — the cost, stated once

This estate's machines are laptops: Wi-Fi (`b43`, `iwlwifi`), a panel (`i915`, `amdgpu`), audio
(`snd-hda-intel`). A new kernel has no drivers for any of them, and there are three honest ways to get them:

1. **Native drivers, written for this kernel.** A driver per chip, forever. That is where most new operating
   systems disappear, and we will not pretend otherwise.
2. **Foreign drivers in isolated VMs** — a vendor or Linux driver running as **untrusted code in a box the kernel
   controls**, holding only the capabilities it was handed. A driver crash takes down one box, not the machine; a
   malicious driver has nothing to escalate from. This is the plan: Linux is a *device provider*, never the
   operating system.
3. **No driver yet.** For the first rungs, QEMU's virtio devices are the hardware, and that is enough to prove a
   kernel.

Consequence, written before it is discovered: the first bootable Morph has **no Wi-Fi, no panel and no audio**
until a driver box exists for them. That is the price of a kernel that is its own thing.

## The rungs, each with what it proves

| rung | what we build | it proves | falsifier |
|---|---|---|---|
| **M0** | boots in QEMU to a Rust task that prints what the machine is, with **no Linux and no bootloader we did not write** | a kernel of our own reaches userspace | it does not reach the task, or the task cannot report |
| **M1** | capabilities: a task granted one object and refused another, with the refusal coming from the kernel | authority is objects, not users | a task reaches an object the kernel did not grant |
| **M2** | the history: state as signed generations; undo to an earlier generation without a reboot | the machine is its history | an undo loses data, or a tampered generation is accepted |
| **M3** | the failure path: a hard lockup leaves kernel-written evidence, and a bad generation is not descended from | the machine explains itself without userspace | no evidence after a lockup, or a bad generation boots again |
| **M4** | the seal: measured boot, and a statement of what was booted that a verifier accepts | the machine can prove itself | a tampered build produces a statement that verifies |
| **M5** | the first driver VM: one real device (a panel), hosted by a Linux driver inside a VM the kernel does not trust | hardware without trusting a driver | the driver VM reaches memory or a device it was not given |

**M0 is next, and it is small.** One rung at a time; nothing is claimed before its falsifier has been run.

## What the existing suite already decides for it

The security arm's attacks are not Linux-specific, they are questions about a machine:

- `attack-console.sh` — does the console demand a credential before it gives a shell? Under Morph the answer
  must be structural: there is no console shell to reach without a capability.
- `attack-disk.sh` — is a credential readable off the disk at rest? Under Morph, state is sealed or it is not
  state.
- `attack-update.sh` — is an unsigned or tampered release refused?
- `attack-probe.sh` — what listens, and under what authority?

They are kept here as `acceptance/` because they are the standard the new kernel will be held to, and because a
kernel for a new type of computer that cannot pass them is a different computer with the same holes.

## Why the kernel, and not an operating system assembled above one

Because the four properties above cannot be enforced from above. A policy applied by a platform is a policy that
platform can decline; a sandbox built by the thing being sandboxed is a promise. A capability is an **object the
kernel either handed you or did not** — there is no path around it, because there is no path.

That is the whole argument, and it is why this project writes a kernel instead of configuring one: the
properties that make this a different kind of computer are kernel properties, not settings.
