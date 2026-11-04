# Sealed state — what "prove what it did" means, and where it stops

`morph-session` gives a person a session whose every change is signed. This document says exactly what that
buys, and what it does not, because the alternative — a feature that sounds like integrity — is how trust gets
misplaced.

## The four verbs

```
morph-session keygen  --out PATH
morph-session open    --root DIR --label L [--query strata://query?under=.] --key PATH
morph-session run     --root DIR --key PATH [--isolated] -- <command...>
morph-session undo    --root DIR --key PATH [--at NS]
morph-session history --root DIR [--key PATH]
```

Exit codes: `0` done, `1` refused, `2` misuse. `history` verifies every seal it reads and **refuses** a record
whose signature does not cover it.

A real run on a MorpheOS machine:

```
change 1 → sealed (transition 1793480214732204211)
change 2 → sealed (transition 1793480214834422443)
history before undo: 2 entries
undo     → undone and sealed: 2 paths restored (transition 1793480214862985545)
history AFTER undo:  3 entries — both changes AND the undo, all "verified"
the file now says: one
```

## What it guarantees

1. **Every change is signed.** A transition carries an ed25519 signature over its canonical JSON; the same kind
   of key `morph-attest` uses for the machine's self-description, so one public key checks *what the machine is*
   and *what it did*.
2. **A failed change leaves no lie.** If the command fails, the state is taken back to the moment before it and
   **no seal is written**:
   ```
   refused: the change failed (the command exited 7) and was taken back (2 paths restored); no seal was written
   ```
3. **An undo is itself a change** and is sealed like any other — otherwise "undo" would be the one uncheckable
   verb in the system.
4. **A tampered history is refused**, not silently accepted: editing one field in a stored record makes
   `history` fail with `does not cover these fields`.
5. **A foreign key cannot vouch for your history** — the signature is checked against the key recorded in each
   record, and a different key fails.

## What it does NOT guarantee

**Writes that go around the door.** `SealedSession` is the only writer the API exposes, and `morph-session run`
is the only writer the CLI exposes — but a process with write access to the layer's directory can still change
files without a seal. Mandatory sealing covers *the seal's own door*, not the filesystem beneath it.

Closing that needs the layer to be **read-only to everyone but the session owner** — a mount-level property
(`MS_RDONLY` + a bind mount for the owner's writes), and the next step. Until then:

> A sealed history proves that **what is in it** happened as recorded. It does not prove that **nothing
> happened outside it**.

**Integrity of the machine itself.** Without measured boot (IMA + a TPM PCR, P2 of the kernel plan) the signing
key lives on the machine, so root can sign a lie. That is stated in [ATTESTATION.md](ATTESTATION.md) and is the
reason this is called a claim with an author and a seal rather than proof of innocence.

## Two design bugs this capability found by running it

Both were invisible to unit tests and obvious the moment a person-level sequence ran — which is the argument for
building the command, not only the library.

1. **A change and its undo shared a filename.** Seals were named by `record.at` (the moment *before* the
   change), so an undo overwrote the seal of the change it undid. Every transition now carries `sealed_at`.
2. **The history lived inside the tree the undo rewinds.** Seals sat in `session/.transitions`, so the first
   end-to-end undo deleted the record of what it undid. They live at `<root>/transitions`, outside anything a
   rewind touches. **A history inside the thing it describes is not a history.**

## The second half: impossible where possible, detectable always

Permissions stop everyone but the owner. They cannot stop root — so the product answers that with detection
rather than a promise:

```
$ morph-session verify --deep      # a clean session
history: 1 sealed transition(s), all verified
state: clean (nothing written outside a sealed transition)

$ echo sneaky > $R/s/capsule/layer/home/sneaky.txt     # root, around the seal
$ morph-session verify --deep
history: 1 sealed transition(s), all verified
state: 1 path(s) written outside a sealed transition:
  added outside a seal: capsule/home/sneaky.txt
exit=1
```

Every sealed transition records a **digest** of the state it sealed (path → content hash, over the session's
files and the capsule's layer together). `verify --deep` compares that with the state on disk and names each
added, changed or removed path. A sealed undo is not drift; a write around the seal is.

And permissions, on the same machine:

```
700 .strata   700 transitions   600 key        # owner-only
as nobody: write into the history  → Permission denied
as nobody: overwrite the key       → Permission denied
as nobody: read the history        → Permission denied
```

> **The two halves together:** a change the seal does not know about is either *impossible* (another user) or
> *visible* (root, named path, exit 1). What is left is root editing the store *and* every seal consistently,
> which is no longer a gap in this design but a gap in the machine's root of trust — and that is measured boot
> (IMA + TPM), not permissions.

## Where the state lives

```
<root>/session        the person's files (a strata:// query resolves here)
<root>/capsule        the application's layer, and its isolation root
<root>/.strata        one content-addressed history for both
<root>/transitions    the sealed transitions — outside the rewound trees, on purpose
```
