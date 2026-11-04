# CERTIFICATES — proofs, exact arithmetic, and what they buy a kernel

Rob, 2026-11-04: *"we did a bunch of bare metal pure algebraic calculation proofs in the pacland repo, can a method like
this be used to speed up any part of the kernel and operating system, ie for example machine code might be an example"*
— and then: *"Yes do it all, fold it to the plan and continue."*

This document folds it in. The pacland method is: **exact computation with a certificate** (Lean 4 for the criteria,
no floating point in the decision, a checkable witness instead of a measurement). It does not make arithmetic faster.
It makes **decisions cheap and checks unnecessary**, which is where a kernel actually spends its time.

## The principle

> A proof at build time buys speed at run time only when it lets you **delete a check**.

Three places that is true in this machine, in the order they should be built.

## 1 · Schedulability certificates — the "one engine never drops" claim, in arithmetic

Whether a set of periodic tasks fits its deadlines is an **exact rational inequality**:

```
Σ (Cᵢ / Tᵢ) ≤ 1                      utilisation test (EDF) — necessary and sufficient for deadlines = periods
∀i:  Rᵢ ≤ Dᵢ                          exact response-time test, for the general case (RTA iteration, rational bounds)
```

Today you would **measure**: run the graph, watch for a dropped buffer. That is a claim about one run on one machine.
With a certificate it is arithmetic: the audio graph hands the Core a witness — the costs, the periods, the deadlines,
and the inequality that was checked — and the Core checks it in a fixed number of `u128` multiplications.

**Where it lands**: a generation (M2's ledger) carries its certificate. `apply()` refuses a generation whose
certificate does not check. Adding a plugin, a node or a scene mid-show becomes a **proof obligation** instead of a
hope, and the refusal names the term that broke the budget.

**Falsifiers, and they are the rung**:
- a graph that fits is admitted, and the certificate's terms are printed;
- the **same graph with one task's cost raised** until the sum exceeds 1 is **refused**, and the refusal names that task;
- a certificate whose arithmetic is wrong (a mutant that reports a smaller cost than it uses) is refused;
- **the certificate is checked in bounded time and bounded memory** — no unbounded denominators, no allocation in the
  check path. A certificate checker that can be made to allocate is a denial-of-service primitive.

**Honest limit**: the Core checks, it does not prove. The *criterion* is proved in Lean; the Core carries a small exact
checker over fixed-size rationals. A Core containing a proof assistant is a Core nobody can audit.

## 2 · Capabilities as an algebra, with derivation proved

Rights form a lattice: a capability derived from another may only ever **narrow**. That is a theorem in Lean
(monotonicity of derivation, no composition exceeding its parent), and the runtime consequence is the fastest possible
implementation of a proved property — a **bitset AND**. The proof is what lets the check be one instruction.

This is a small piece of work and it closes a hole M1 leaves open: today the table refuses what was not granted; it
does not yet *prove* that a granted capability cannot be widened by any sequence of derivations.

## 3 · Machine code — three things, in increasing order of effort

- **Proof-carrying code (the real win).** Software fault isolation masks or branches on every memory access. A
  certificate that the code stays in its box is validated **once, at load**, and the per-access check disappears —
  the classic 5–30% class of win for sandboxed code. It fits M4b exactly: the box is already *"an address that does
  not exist in its map"*; a certificate makes the **load** the proved thing rather than every access.
- **Certifying compilation.** Lean → C → machine code with a proof of the translation (CompCert's discipline). It
  rarely makes the output faster; it makes *"the code the Core runs is the code that was proved"* true — which is the
  claim M6 and M9 need, and the one a compiler bug would otherwise break silently.
- **Superoptimisation with a proof.** Rewrite short machine-code sequences to shorter equivalents and prove
  equivalence in Lean. It genuinely makes machine code faster and smaller, and for a handful of paths — context
  switch, IPC fast path, packet parse — it is tractable. It is not a general optimiser and will not be sold as one.

## What it will not do

Memcpy, DSP inner loops, cache behaviour, SIMD throughput. Those are empirical, not algebraic: no theorem makes a
cache line arrive sooner. A proof makes a *decision* cheap or a *check* unnecessary; it does not make arithmetic wider.

## The rungs this adds

| rung | what it proves | falsifier |
|---|---|---|
| **M7a** | a generation carries a schedulability certificate, and `apply()` refuses one that does not check | a fitting graph is admitted; the same graph with one raised cost is refused and the term is named; a lying certificate is refused |
| **M7b** | the check is bounded: no allocation, no unbounded denominators | an adversarial certificate cannot make the check allocate or spin |
| **M7c** | capability derivation cannot widen | a proof in Lean, and a test that every derivation path in the Core narrows or is refused |
| **M7d** | a box's code is validated once at load, not per access | the same driver box runs with the per-access mask removed, and the certificate check is the only cost |

**Order**: M7a → M7b → M7c → M7d, and M7a comes **before** the network rung, because it is the property that makes a
show's "never drops" a measurement-free claim — the thing this OS can say that a Linux-based system cannot.

---

## STATE, 2026-11-04 — M7a, M7c and M7d are DONE and measured

**M7a — the schedulability certificate.** `core/src/cert.rs`. `Σ(Cᵢ/Tᵢ) ≤ 1` decided by cross-multiplication
(`D = Π Tⱼ`, `S = Σ Cᵢ·(D/Tᵢ)`, claim `S ≤ D`) — integers throughout, no floating point. At most three terms,
each below 2³², so `D ≤ 2⁹⁶` inside `u128`; **a certificate outside the bound is refused, not computed**, because a
witness a hostile party can make expensive is a denial-of-service primitive.

- `ledger::apply_certified` checks first and appends only if the arithmetic holds: **a generation that cannot prove
  it fits never becomes history** (measured: head `0 -> 0` after the refusal).
- Falsifiers, all green in `scripts/cert.sh`: a fitting graph admitted at 350000 ppm; the same graph with `engine`'s
  cost raised is REFUSED and `engine` is named; a lying certificate REFUSED with both values printed; **the check
  allocated nothing** (the allocator's counter is read before and after, so the bound is measured, not promised).
  Mutation `cert-mutant-skip-check` admits both bad certificates, so the refusals are the arithmetic's.

**M7c — a capability may narrow and never widen.** `core/src/cap.rs`. `Rights` is three bits; `derive()` performs only
when the wanted rights are a **subset** of the held ones. Widening is not a policy the Core applies — it is an
operation that does not exist. `scripts/cap.sh` measures both directions; `cap-mutant-widen` makes it exist.

**M7d — a box validated once at load.** `core/src/box_admission.rs`. A box states where its code lives and the highest
address it will touch; the Core checks that claim **once**, and if it holds the box runs without a mask or bound check
per access — the 5–30% a sandbox otherwise costs. `box-mutant-trust-claim` admits the liar.
**Honest scope:** this is the ADMISSION SHAPE, not an ELF verifier; proving the claim of real machine code is M7d's
remaining work.

**M7b — still open.** The bounded check is bounded *by construction* (three terms, 2³², `u128`) and the bound is
measured by the allocator counter. What is NOT done is the Lean side: the criterion is the EDF utilisation theorem, and
this repository checks it rather than proving it. Do not describe the check as proved.
