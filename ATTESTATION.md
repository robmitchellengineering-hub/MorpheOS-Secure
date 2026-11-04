# Attestation — what MorpheOS can prove about itself, and what it cannot

> **Carried from the security arm, 2026-10-31.** Written against the earlier, Linux-based arm, and kept here because what it states — what a machine can prove about itself, and exactly where the claim stops — is the standard the Core must meet. Documents it cites that stayed in the MorpheOS repository are linked by URL.


Rob, 2026-10-31: *"what if there was a 3rd better option built for today, MorpheOS"*.

Every operating system asks to be trusted and shows you a logo. The thing a third platform can do that the
other two mostly do not is **state what it is in a form you can check yourself**, offline, without a vendor
service. That is `morph-attest`.

## What it says

Run on a MorpheOS machine:

```
$ morph-attest sign --key /persist/attest/key
{
  "statement": {
    "format": 1,
    "system": "MorpheOS 1.0",
    "kernel_release": "6.8.0-147-generic",
    "kernel_sysname": "Linux",
    "slot": "a",
    "boot_id": "b681e1b8-…",
    "machine_id": "cc981727…",
    "lsm": ["lockdown", "capability", "landlock", "yama", "integrity"],
    "posture": {
      "console": "credential-required",
      "lockdown": "absent",
      "root": "read-only",
      "secure_boot": "unknown"
    }
  },
  "signature": "…",
  "key": "f57058c5…"
}
```

Note the second line after the name: `kernel_sysname: Linux`. On a stock-kernel MorpheOS image that is the
**truth**, and the tool says it. On a machine running the kernel we build, the same field reads `MorpheOS`.
An attestation tool that flattered the product would be worthless.

## What it proves, exactly

1. These facts were assembled by a process holding the attestation key.
2. Every field is what it was when signed — one changed byte fails:

```
$ morph-attest verify tampered.json --trusted-key f57058c5…
refused: the signature does not cover these fields
```

3. The statement is about **this boot** (`boot_id`) and **this installation** (`machine_id`), so it cannot
   be replayed against another machine or another boot.
4. A statement whose facts have moved on is reported **stale**, with the fields that changed — not silently
   accepted:
   ```
   stale: the signature is good but the machine has moved on
     kernel_release: 6.8.0-morpheos -> 6.8.0-morpheos-next
   ```
5. With no key, it **refuses to speak** rather than printing something a reader might mistake for verified:
   ```
   $ morph-attest sign --key /persist/attest/absent
   morph-attest: no attestation key at /persist/attest/absent: No such file or directory
   exit 1
   ```

## What it does NOT prove — the honest part

**It is not proof of innocence.** Without measured boot, the attestation key lives on the machine, so anyone
with root can sign a lie. What it gives you is a *claim with an author and a seal*:

- a statement you were shown cannot be edited afterwards;
- a statement is bound to a machine and a boot;
- a machine whose *report* disagrees with its *behaviour* becomes detectable, because the report is checkable
  by a third party who was never trusted by the machine.

**What would make it a real proof:** measured boot — `IMA` over the kernel, initramfs and early userspace,
with the measurements extended into a TPM PCR and the key sealed to that PCR state. That is **K3** of
[the Secure arm](https://github.com/robmitchellengineering-hub/MorpheOS/blob/main/docs/26-secure-arm.md) ([ADR 0022](https://github.com/robmitchellengineering-hub/MorpheOS/blob/main/docs/adr/0022-own-the-kernel-build.md)): our kernel already carries `CONFIG_IMA=y` and
`CONFIG_INTEGRITY_SIGNATURE=y`. Until then this document says "signed claim", never "proof of integrity".

## Why a platform needs this

- **Support can stop guessing.** A ticket can carry a statement that says which slot, which kernel, which
  posture — and the sender cannot doctor it afterwards.
- **Fleet verification without a vendor.** An operator holds one public key and can check any machine's
  statement offline. No cloud account, no per-seat fee.
- **It makes our own claims falsifiable.** "MorpheOS says it is MorpheOS" stops being a screenshot and
  becomes a value anyone can recompute — and a wrong one is visible.

## Usage

```
morph-attest show                                  # the statement, unsigned
morph-attest keygen --out /persist/attest/key      # make a key (refuses to overwrite, 0600)
morph-attest sign --key /persist/attest/key        # the signed statement, pretty JSON
morph-attest verify FILE [--trusted-key HEX]       # 0 trusted, 1 refused or stale, 2 misuse
```

Exit codes are the contract, because a script has to be able to act on this: `0` trusted, `1` refused or
stale (with the reason on stderr), `2` misuse.

## The key

Provisioned into `/persist` at first use — **never baked into the image**. A key published in an artefact is
a key everybody has, which makes every statement signed with it worthless (the reason
`no credential in a published artefact` is a standing SOP rule).
