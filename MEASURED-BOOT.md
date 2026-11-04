# Measured boot — what this machine can prove about what it booted

This document exists because the honest answer today is **less than the phase is called**. It records what is
true on this platform right now, with the commands that show it, and what remains before "measured boot" means
what people assume.

## What the platform actually has

| question | answer | how it was established |
|---|---|---|
| Does our kernel have IMA? | **yes** — `CONFIG_IMA=y`, `CONFIG_INTEGRITY=y`, `CONFIG_INTEGRITY_SIGNATURE=y` | the built kernel's `.config` on the build host |
| Is IMA live in the running image? | **yes** — `/sys/kernel/security/ima/` with `ascii_runtime_measurements`, `policy`, `violations` | listed inside the VM |
| Is it measuring anything? | **almost nothing** — 1 entry, an all-zero boot aggregate | `wc -l` → 1; first entry hashes zeros |
| Is there a TPM? | **yes, since 2026-10-31** — `swtpm` 0.7.3 installed on the VM host, and `VM_TPM=1` attaches `tpm-tis` | `swtpm --version`; the guest kernel logs `tpm_tis MSFT0101:00: 2.0 TPM (device-id 0x1, rev-id 1)` |

The one measurement is the kernel's boot aggregate with a zero digest: **IMA is present but no policy is
loaded**, so it has not measured the kernel, the initramfs or any file. That is not a defect in MorpheOS; it is
the default state of the feature on this kernel.

**The TPM arrived during this phase.** `swtpm` 0.7.3 is installed on the VM host and `images/x86-qemu/run-vm.sh`
attaches it when `VM_TPM=1` is set, with the state directory beside the disk (a TPM's identity lasts exactly as
long as its disk). A boot with it produced, in the guest's own kernel log:

```
ACPI: TPM2 0x000000007FB76000 00004C (v04 BOCHS  BXPC     00000001 BXPC 00000001)
tpm_tis MSFT0101:00: 2.0 TPM (device-id 0x1, rev-id 1)
```

So the machine now has a **TPM 2.0** to extend measurements into. What is still missing for a quote is the
policy that makes IMA measure anything, and a stable forwarded port on the TPM VM so PCRs can be read and
recorded from inside it — both are work, not unknowns.

## What is delivered, and is real

`morph-attest boot` — a signed statement of what the kernel measured:

```
morpheos-measured-boot v1
boot_id: b681e1b8-0496-4833-b52b-c3caa6a702e6
ima_entries: 1
ima_log_blake3: 046c2c9b09938945b7ea4ad5474d5afa091a67910ae649acc4dba559ccadbe9a
tpm_quote: none (no TPM on this machine)
```

With `--key` it is signed with the same ed25519 discipline as the machine's self-description, and without a key
it prints the claim **and says it has no author**. The payload is built by a tested library function
(`boot_payload`), with tests pinning three properties: the same log and boot give the same bytes, a different
log gives different bytes, a different boot gives different bytes, and the text carries `tpm_quote: none` where
a verifier reads it — so a signed log can never be mistaken for a hardware quote.

**What that proves:** the log this machine presented was not edited after it was signed, and it belongs to this
boot of this machine.

**What it does not prove:** that the log is *complete* (nothing is measured yet), and that the machine is
telling the truth (the key lives on the machine — see [ATTESTATION.md](ATTESTATION.md)).

## The gap to real measured boot, in order

1. **A policy.** `ima_policy=tcb` on the kernel command line (or a custom policy written to
   `/sys/kernel/security/ima/policy`) makes IMA actually measure the kernel, the initramfs and the files a boot
   reads. Without it, "measured boot" is a directory that exists.
2. **A TPM.** With `swtpm` (or real hardware) the measurements extend into PCRs, and the machine can produce a
   **quote** over those PCRs signed by a key sealed to the TPM — the thing that makes the claim un-fakeable by
   root. `swtpm` is not installed on the VM host today, which is why this document says "none".
3. **Verification of the quote** against a known-good PCR value, which is what a third party compares.

Until 1 and 2 exist, the accurate description of what MorpheOS can say about its boot is: **"here is a signed
digest of what this kernel measured, and the kernel measured almost nothing."** That sentence is worth more than
a claim of measured boot, because it can be checked.
