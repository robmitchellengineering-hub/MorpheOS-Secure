# PLAN — the security plan, and what each control becomes when the Core is ours

This is the security arm's plan, carried here because this is the repository that will build the kernel it was
written for. It is not a rewrite: the charter and the measured scorecard are
[acceptance/arm-charter.md](acceptance/arm-charter.md), in that document's own words.

## What "secure" is allowed to mean

Not a superlative. A **list of propositions, each with an attacker that tried to break it**. The estate's method
is claim → falsifier → proof the falsifier can fail → evidence a stranger can re-run, and this edition is where
it is applied to the machine itself rather than to an application.

Forbidden, permanently: *"the most secure computer in the world."* The claim is the attacks on file.

## Who it is for

An organisation whose laptop will be lost, stolen, or plugged into a hostile network — a journalist, a clinic, a
law firm, a field engineer. That buyer accepts friction for proof. Rob's own daily machine is the *everyday*
build's target, never this one's.

## Where the machine stands today (measured, from the arm)

| attacker | today | the control that changes it |
|---|---|---|
| remote, unauthenticated | **stopped** — one listener, key-only | keep it: a listening budget of one, asserted by an attack |
| remote, with the intended credential | stopped **by a defect** — the image's own key is refused to its owner | working per-machine identity, revocable |
| remote, via the browser | **open** — immutable image, manual updates | signed auto-updates + a disposable browsing context |
| network-adjacent (evil twin, hostile DHCP/DNS) | **open** | default-deny, no auto-join, DNSSEC/DoT |
| hands on the machine, 30 s | **root, no password** (`Ctrl+Alt+F2`) | a console credential |
| the disk alone | **the Wi-Fi PSK in clear** | data at rest sealed |
| supply chain | **untested** | signed, reproducible builds and a transparency log |
| the machine's own honesty | **asserted, not proven** | measured boot + attestation |

## The controls, and what they become on a Core of our own

The arm's eight controls were written for a machine with a borrowed kernel. On the Core they stop being
configuration and become structure — the column on the right is the point of writing the kernel at all.

| # | control | the attack that must fail | on our Core it becomes |
|---|---|---|---|
| **S3** | console credential | `Ctrl+Alt+F2` must produce `login:` before any shell | there is no console shell without a capability; the login *is* the capability grant |
| **S2** | data at rest | copy the disk, grep it for `psk=` | state is sealed or it is not state; keys are sealed to the machine |
| **S5** | default-deny network | scan from a second host; anything past the allowance is a finding | a task reaches the network only through a capability it was handed |
| **S4** | per-machine identity | another machine's key must be refused | identity is a key the Core holds and the machine can attest |
| **S6** | anti-rollback updates | a signed-but-older release must be refused | a generation is signed and monotonic; descending is a deliberate act, not an install |
| **S7** | measured boot + attestation | a tampered image attests and must be rejected | the measurement chain starts in our loader, not a vendor's |
| **S8** | disposable browsing | a hostile page must not reach `/persist` or the keys | the browser is a task with no capability to anything it is not shown |
| **S1** | verified boot | modified kernel and initrd must refuse | the loader verifies what it loads because we wrote both |

**Order: S3 → S2 → S5 → S4 → S6 → S7 → S8 → S1** — cheapest first, and the two that need no network and no
machine (console, disk) are closed before anything else. S1 is last because a failed key enrolment bricks a
stick, and it is never enabled on media a person flashes themselves.

The Core's rungs (M0–M5, in [CORE.md](CORE.md)) run **beside** this order: M1–M2 are what make S3 and S2
structural rather than configured, and M4 is what makes S7 real.

## The one-way flow

A control leaves this repository for the everyday build only when (a) the arm's attack against it has been run
and failed, and (b) it has been used for a week without the user noticing. Anything that makes a daily
professional stop and type stays here, however good it is. Evidence moves outward; convenience never moves
inward.

## What is Rob's, not the session's

1. **The buyer** — naming the first real one decides how much friction is acceptable.
2. **Whether this repo publishes releases at all**, or stays internal until the console and disk controls are
   green.
3. **The reference machine with a TPM 2.0** for measured boot (the Legion's fTPM today), and whether it may have
   its firmware keys changed once for enrolment.

## Falsifiers for this plan

- **It is wrong if a claim is published without its attack committed.** That is the forfeit.
- **It is wrong if the everyday build gets slower or redder because of anything here.** This repository does not
  edit it. If it does, that is the defect.
- **It is wrong if a control graduates and the first week of real use produces a complaint** — the graduation
  test is too weak, not the user.
- **It is wrong if any attack reports UNKNOWN and is counted as a pass.** Absent evidence is not evidence.
- **It is wrong if the Core is built to pass this list and not to be a different kind of computer.** The
  controls are the floor, not the point.
