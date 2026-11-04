#!/bin/bash
# Attack probe: the claims the default x86 image makes about its own exposure, attempted rather
# than asserted (Rob, 2026-10-29: "a claim that is not shown to fail is false").
#
#   images/x86-qemu/attack-probe.sh <host> [--key <path>]
#
# Exit 0 only if every claim HELD. A claim that could not be tested is reported as UNKNOWN and
# fails — the same rule the rest of this repository applies to green checks.
#
#   A1  only sshd answers on the network            (the image runs no other network service)
#   A2  an unknown key is refused                   (no unauthorised remote entry)
#   A3  the image's own admin key is accepted       (the remote-admin path actually works)
#   A4  no password login is offered                (an empty/weak root password is unusable)
#
# The console claim (VT2 gives root with no password) cannot be tested over the network and is
# NOT checked here; HARDWARE.md documents it and it is the finding this suite exists to close.
set -uo pipefail

HOST=${1:?usage: attack-probe.sh <host> [--key <path>]}
KEY="$HOME/.ssh/mesh-admin"
[ "${2:-}" = "--key" ] && KEY=${3:?--key needs a path}

fail=0
say() { printf '%-6s %s\n' "$1" "$2"; }
pass() { say "ok" "$1"; }
bad() { say "FAIL" "$1"; fail=1; }
unk() { say "UNKNOWN" "$1"; fail=1; }

# A1 — a scan of the ports a misconfigured image would leave open. Deliberately not all 65535:
# this runs in CI against a VM, and the interesting set is services, not ephemeral ports.
OPEN=""
for p in 21 22 23 25 53 80 111 139 443 445 587 631 993 995 2049 3306 3389 5432 5900 6379 \
         8080 8443 9090 27017; do
    if nc -z -G 1 "$HOST" "$p" 2>/dev/null; then OPEN="$OPEN $p"; fi
done
UNEXPECTED=$(printf '%s\n' $OPEN | grep -v '^22$' | tr '\n' ' ')
if [ -z "${OPEN// /}" ]; then
    unk "A1 no port answered, not even 22 — the probe could not test the machine"
elif [ -n "${UNEXPECTED// /}" ]; then
    bad "A1 ports open beyond sshd:$UNEXPECTED"
else
    pass "A1 only sshd answers (22 open, nothing else of 25 services)"
fi

ssh_try() {  # ssh_try <key> -> prints the last line, exits 0 when it logged in
    ssh -i "$1" -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
        -o ConnectTimeout=6 -o PreferredAuthentications=publickey root@"$HOST" true 2>&1 | tail -1
}

# A2 — no key, no entry.
NOKEY_DIR=$(mktemp -d)
ssh-keygen -q -t ed25519 -N '' -f "$NOKEY_DIR/id" 2>/dev/null
out=$(ssh -i "$NOKEY_DIR/id" -o BatchMode=yes -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null -o ConnectTimeout=6 root@"$HOST" true 2>&1 | tail -1)
case "$out" in
    *"Permission denied"*) pass "A2 an unknown key is refused" ;;
    "")                    unk "A2 no answer from sshd" ;;
    *)                     bad "A2 an unknown key was NOT refused: $out" ;;
esac
rm -rf "$NOKEY_DIR"

# A3 — and the intended path must work, or the machine has no remote administration at all.
if [ ! -f "$KEY" ]; then
    unk "A3 no key at $KEY to test the admin path"
else
    out=$(ssh -i "$KEY" -o BatchMode=yes -o StrictHostKeyChecking=no \
            -o UserKnownHostsFile=/dev/null -o ConnectTimeout=6 root@"$HOST" true 2>&1 | tail -1)
    case "$out" in
        "")                     pass "A3 the image's admin key is accepted" ;;
        *"Permission denied"*)  bad "A3 the image's OWN admin key is refused — the remote-admin path this image is built to provide does not work: $out" ;;
        *)                      bad "A3 unexpected ssh result: $out" ;;
    esac
fi

# A4 — password authentication must not even be offered, so no password can be guessed.
# sshd names the methods it will accept in the refusal itself: "Permission denied (publickey)."
denied=$(ssh -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
           -o ConnectTimeout=6 -o PreferredAuthentications=none root@"$HOST" true 2>&1 | tail -1)
methods=$(printf '%s\n' "$denied" | sed -n 's/.*Permission denied (\(.*\)).*/\1/p')
case "$methods" in
    "")        unk "A4 could not read the server's offered methods (${denied:-no answer})" ;;
    *password*) bad "A4 the server offers password authentication ($methods)" ;;
    *publickey*) pass "A4 only publickey is offered ($methods); no password to guess" ;;
    *)         bad "A4 unexpected methods offered: $methods" ;;
esac

[ "$fail" = 0 ] && echo "attack-probe: every claim held" || echo "attack-probe: at least one claim did not hold"
exit "$fail"
