# Rules for this repository

**This repository is the secure edition, with a kernel of its own. It does not contain the everyday MorpheOS
image, builder or design set, and it does not edit them.** Rob, 2026-11-04: *"Revert everything you've done to the
everyday version, it was already working on hardware and is a different project."*

1. **The everyday build is a different repository.** Nothing here changes it. If a control needs a change in the
   everyday build to graduate, that change is a separate pull request in that repository, with its own attack.
2. **A claim with no attack is not a claim.** [PLAN.md](PLAN.md) lists the propositions; `acceptance/attacks/`
   decides them. A suite that cannot reach its subject reports UNKNOWN, and UNKNOWN is not a pass.
3. **The kernel is its own thing.** [KERNEL.md](KERNEL.md) is its scope. A design question answered with
   "because Linux/Unix/NT does it that way" is not answered.
4. **Nothing is claimed before its rung has run.** M0–M5 in [KERNEL.md](KERNEL.md) are rungs, not milestones to
   announce. Today the only true sentence about the kernel is that no line of it exists.
5. **Drivers are a compatibility device, not a project.** Linux drivers are hosted in isolated VMs; the kernel
   trusts a driver not to escape, never to be correct.
6. **One worktree per session, and stage explicit paths.** A blanket stage of every changed file in a shared
   checkout is refused by the session hook, and it is refused for a reason: it cannot tell your work from someone
   else's. Check what you staged before you commit.
7. **Never push to `main`.** Every change lands through a pull request, and a green pull request merges itself.
8. **Never publish a credential, and never print one into a transcript.** Reference where it lives instead.

## Where the plan comes from

- `PLAN.md` — the security plan, and what each control becomes on a kernel of our own.
- `acceptance/arm-charter.md` — the security arm's charter and measured scorecard, in its own words.
- `SEALED-STATE.md`, `ATTESTATION.md`, `MEASURED-BOOT.md` — what the machine must be able to prove about itself,
  and exactly where each claim stops.
- `adr/0022-own-the-kernel-build.md` — the step before this one, kept because it explains why the tests exist.
