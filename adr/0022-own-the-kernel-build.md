# 0022 · We build and patch the Linux kernel ourselves; a kernel from scratch stays out of scope

Status: **Accepted** — 2026-11-04. Rob: *"get started on the security devision and the kustum
kernal"*, after the account model was decided (ADR 0020) and after correcting the target: the
security line is **a separate arm in this repository**, not the everyday build
([doc 26](../26-secure-arm.md)). The kernel is built in that arm first — K1 in doc 26 — and the
everyday build inherits it when a comparison against Ubuntu's kernel is boring.
[ADR 0001](0001-linux-kernel-first.md) stands and is not superseded: we still run Linux and
still do not write a kernel. What changes is that the Linux we run is one **we build, configure
and patch**, rather than one we borrow.

## Context

Every claim this system makes is enforced by userland today:

| claim | enforced by | what it is worth |
|---|---|---|
| "never breaks, heals itself" | A/B slots and `morph-heal`, driven by a userspace health check | nothing if the failure is the userspace that decides |
| "an app cannot exfiltrate" | seccomp/landlock profiles applied by systemd units | a policy the process's platform can decline to apply |
| "the machine explains itself" | `journalctl`, our own logs, phone-home | a log that dies with the userspace writing it |
| "this is MorpheOS" | `os-release` | `uname -s` still says Linux |
| "we can fix the hardware" | nobody | we borrow Ubuntu's drivers and cannot patch a quirk |

The kernel is also the only place some of these can be decided at all: a slot the kernel marks
healthy survives a userspace that never starts, and an LSM is kernel-side by definition.

The alternative — a kernel from scratch — was considered and rejected. It loses Firefox,
PipeWire and every driver on the hardware we actually own, for purity rather than a product.
Doc 03 already places that work later: Phase 4+ is **Morph**, a capability-based Rust
microkernel, for high-assurance profiles, *after* the model is proven on a borrowed kernel.
This ADR does not open it, and K1–K3 do not depend on it.

## Decision

**Own the build, in four phases, with the Ubuntu kernel kept as the other A/B slot until a
comparison is boring.**

| phase | work | what it proves |
|---|---|---|
| **K1 · own the build** | our configuration fragment, `uname -s` = MorpheOS, `-morpheos` release suffix, KSPP hardening, modules signed by us, the 2012 laptop's own drivers | the machine runs our kernel and still has Wi-Fi, audio and a panel |
| **K2 · own the failure path** | `kexec` rollback, `pstore`/ramoops flight recorder, slot health decided in the kernel, no initramfs | the machine heals without userspace, and explains a crash it did not survive |
| **K3 · own the policy** | eBPF LSM per service, IMA + TPM measured boot, `verity` on the root, no module loading from disk | an attack is denied by the kernel and the log names the policy |
| **(not planned here)** | the model: no systemd, the app as a kernel object, versioning at the VFS, livepatch | — |

1. **The name goes in the kernel.** `uname -s` returns *MorpheOS* and `uname -r` carries
   `-morpheos`. A wrapper cannot do this, because the check is the syscall. If the tree has no
   configuration for the sysname, we patch `include/linux/uts.h`: we own the tree.
2. **The config is ours, and it is small.** Only the subsystems this hardware uses. A subsystem
   we do not build cannot be attacked, and a config that is too small fails on the laptop rather
   than in the VM — so the laptop is part of the check, not an afterthought.
3. **Modules are signed by us**, and unsigned modules are refused. That is the first place trust
   is anchored in the kernel rather than in userspace policy.
4. **The Ubuntu kernel stays in the other slot** for the first release, so every regression is a
   measurable comparison boot by boot, not a guess.

## Consequences

- **We now own the CVE patch stream.** Rebuild against `stable` weekly in CI from the first
  merge; a kernel we patch is a kernel we track. Livepatch is a later phase, not a plan.
- **Driver regressions are ours to fix** — which is the point (a `b43` quirk, `i915` on a 2012
  panel), and also the cost.
- **The build leaves the orchestrator.** Kernel compiles run on a Linux node, never on the Mac
  (AGENTS.md H16: the same rule that makes `cargo test` on macOS a lie).
- **A custom kernel is not a faster kernel.** It is faster only where we remove work (no
  initramfs, `drm` lease, RT scheduling), and those are measured, not asserted.
- **An LSM we wrote is a claim until an attack fails against it.** K3's falsifier is that a
  denial happens in the kernel and names its policy; a denial in userspace does not count.

## What Rob decided (2026-11-04)

*"get started on the security devision and the kustum kernal"* — the kernel track begins at K1
(own the build), with the Ubuntu kernel kept as the other slot. A kernel from scratch was not
chosen and is not opened by this record.
