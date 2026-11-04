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

A from-scratch kernel has no drivers, and this estate's machines are laptops with Wi-Fi (`b43`, `iwlwifi`), a
panel (`i915`, `amdgpu`) and audio (`snd-hda-intel`). Two honest options, and the plan is the second:

1. **Write native drivers** — a driver per chip, forever. That is the graveyard most new operating systems
   disappear into, and it is why the original design chose a Linux hardware abstraction layer in the first
   place.
2. **Host Linux drivers in isolated driver VMs** (doc 03's own scope: *"It would host Linux drivers in isolated
   driver VMs (the 'driver capsule' approach), so we never lose hardware support."*). The kernel trusts the
   driver **not to escape**, not to be correct. A driver crash takes down one VM, not the machine; a malicious
   driver has no capability the kernel did not hand it.

Option 2 means the first bootable Morph does **not** have Wi-Fi, a panel or audio until a driver VM exists for
them. That is the price of a kernel that is its own thing, and it is written here rather than discovered later.

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

## Why this is not a from-scratch entry in the graveyard

The original design's own answer, kept honest: the estate's earlier plan used Linux as a hardware abstraction
layer for its first phases, for the reason above — drivers. Morph exists because the *security claims* cannot be
enforced from userspace: an LSM a distro applies is a policy its platform can decline, and a capability is an
object the kernel either handed you or did not.

So the sequencing is: **the kernel is the product here, the drivers are a compatibility device, and the
acceptance suite decides.** If that trade is wrong, the honest alternative is to keep the Linux HAL — and that
is a different repository.
