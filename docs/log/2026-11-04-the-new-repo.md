# 2026-11-04 · DeepSeek · the secure edition gets its own repository, around its own kernel

Rob, across this session: the secure line is *"a separate arm in the morphos repo"*, then *"I never asked you to
work on the everyday version"*, and then the thing this repository is built on — the custom kernel he has meant
all along is **a kernel that is its own thing**, *"a new type of computer"*, not the Linux we configure, harden
and rename. Spelled out when asked directly: *"clearly not what i have ever meant in this session talking about
our custom kernal that is its own thing a new type of computer."*

## What this repository is

The secure edition, with **the Core** (`morph-core`) as its privileged component: capability-based, Rust, high-assurance first. `CORE.md` is its
scope and its first rung. **No line of the Core exists** — that sentence is in the README, in the kernel document and
here, because the alternative is what this session kept doing: treating a built-and-renamed Linux as if it were
the destination.

## What is carried in

- `acceptance/arm-charter.md` and `acceptance/attacks/` — the four attackers (A1–A7) with fixtures that must pass
  **and** be caught, and an explicit UNKNOWN. This is the standard the new kernel will be judged by.
- `SEALED-STATE.md`, `ATTESTATION.md`, `MEASURED-BOOT.md` — what the machine must be able to prove about itself,
  and exactly where each claim stops.
- The attacks the earlier work produced, kept as the standard this kernel will be judged by. The kernel that
  earlier work built — a Linux, configured and renamed — is **not** this project's direction and is not carried
  here. Rob, when the framing slipped back into it: *"You just said building linux ourselves ... thats not what i
  want to do, we are building a new type of operating system."*

## What is deliberately out

The everyday image, its builder and its design set. The Linux kernel we built stays in the MorpheOS repository as
the platform the arm's tests ran on: not copied here, not imported, not edited from here.

## Next

Rung **M0**: boot in QEMU to a Rust task that reports what the machine is, with no Linux and no bootloader we did
not write. It is small, it is falsifiable, and it is the first thing that would make any sentence about Morph more
than a plan.

## What the session paid to get here

The everyday build was modified to prove the arm's kernel, and it broke; it has been reverted (MorpheOS #182). The
violations are filed in the mesh register as V-42 to V-49, including that one, and the confusion about which
kernel this project is about — a Linux build called "ours" — is now written down in this repository so it cannot
be repeated.
