# MorpheOS Secure — the security arm

This branch (`secure`) is the **Secure edition** of D17: the same core, with the posture as the product.
Every claim here is stated as a falsifier that can go red, and the attacks that try to make it go red live
in `attacks/`.

The Everyday edition inherits only what costs the user nothing. The flow is **one-way**: a technique proven
here may be adopted upstream; a convenience added upstream is never assumed safe here.

## What lives here

| path | what it is |
|---|---|
| `attacks/attack-probe.sh` | A1–A4 — the network: open ports, unknown key, intended key, no password auth |
| `attacks/attack-console.sh` | A5 — does the console demand a credential before it gives a shell |
| `attacks/attack-update.sh` | A6 — is an unsigned or tampered payload refused |
| `attacks/attack-disk.sh` | A7 — is a credential readable off the disk at rest |

Every attack has a `--self-test` with fixtures that must pass **and** fixtures that must be caught, and an
explicit `UNKNOWN` (exit 2) for "the test could not reach its subject". A test that cannot reach its
subject is not a pass — that rule cost five image builds on 2026-10-30 and is now the first line of
`attacks/attack-console.sh`'s rationale.

The suite's own gate is `scripts/check.sh` under **"the red-team suite can go red"**.

## Where it stands, measured (2026-10-30, on real hardware)

| # | attack | attacker needs | result |
|---|---|---|---|
| A1 | port scan | same network | **stopped** — 22/tcp is the only listener |
| A2 | unknown key | same network | **stopped** — refused |
| A3 | the image's own key | same network | **stopped — by a defect**, not by design: the key is refused to its owner too (PR #166) |
| A4 | password / empty password | same network | **stopped** — publickey only |
| A5 | `Ctrl+Alt+F2` | **hands** | **FAILS** — root, no password (`HARDWARE.md` says so itself) |
| A6 | hostile update | same network | **not installed** — the plain image carries no updater and no CA; the autoupdate scenario does, and there it is signature-gated |
| A7 | the disk alone | the stick | **FAILS** — Wi-Fi PSK plaintext in `/persist/network/system-connections` on unencrypted ext4 |

**The shape of the result:** unenterable from the network, trivially enterable with hands. Closing that
asymmetry is the console credential (S5/E11, Rob's decision) and encryption at rest (S2).

## The walls worth writing down

- **There is no remote entry, and that is a finding, not a defeat.** One listener, key-only, no updater,
  no OpenSSH 9.6p1 bypass in 24.04. A red team that keeps "trying harder" against a closed surface is
  spending money to learn nothing.
- **The browser gives a page, not a shell.** The booted page is entirely local; the only self-initiated
  network fetch is the captive-portal probe. Real phishing surface, not an authentication bypass.
- **A machine that cannot be queried cannot be diagnosed by rebuilding it.** Ship the observation first:
  the hardware report now carries `ssh.txt`, and `morph-phonehome.service` sends it off the machine when
  `/efi/morpheos-phonehome.txt` names a receiver.
