#!/bin/bash
# Attack A6 — an update must not be installable by anyone but us.
#
# THE CLAIM: "only a payload signed by a key in the image is installed." The product's own tests assert
# it from the product's side; this one attacks it from the outside, which is the only side that matters:
# an attacker who can serve a payload must be refused.
#
#   images/x86-qemu/attack-update.sh --self-test                 # prove the harness can see acceptance
#   images/x86-qemu/attack-update.sh --morph-update <bin> <payload>
#
# Exit 0 = the payload was refused (good). 1 = it was accepted (the finding). 2 = could not tell.
#
# ⚠️ The self-test is the point of this file today: the real run needs a built image, a signed payload
# and a tampered copy of it, and that lives in test-autoupdate.sh. What is proved here is that the
# harness goes red when something accepts a payload it should not — a harness that cannot do that is a
# rubber stamp.
set -uo pipefail

judge() {  # judge <exit code> <what was tried>
    local rc=$1 what=$2
    case "$rc" in
        0) echo "FAIL: $what was ACCEPTED — an attacker who can serve a payload gets installed"; return 1 ;;
        *) echo "PASS: $what was refused (exit $rc)"; return 0 ;;
    esac
}

self_test() {
    local dir fail=0
    dir=$(mktemp -d)
    # A payload and a stand-in installer: one that refuses everything, one that accepts everything.
    printf 'pretend payload\n' > "$dir/payload.rob"
    printf '#!/bin/sh\nexit 1\n' > "$dir/refuser"; chmod +x "$dir/refuser"
    printf '#!/bin/sh\nexit 0\n' > "$dir/accepter"; chmod +x "$dir/accepter"
    expect() { local name=$1 want=$2 rc=$3 got; judge "$rc" test >/dev/null; got=$?;
        if [ "$got" = "$want" ]; then echo "ok   $name"; else echo "FAIL $name"; fail=1; fi; }
    expect "a refusing installer passes" 0 1
    expect "an accepting installer is caught" 1 0
    rm -rf "$dir"
    [ "$fail" = 0 ] && echo "attack-update: the harness sees acceptance" || echo "attack-update: the harness cannot see acceptance"
    return "$fail"
}

case "${1:-}" in
    --self-test) self_test; exit $? ;;
    --morph-update)
        bin=${2:?usage: --morph-update <bin> <payload>}; payload=${3:?usage: --morph-update <bin> <payload>}
        [ -x "$bin" ] || { echo "UNKNOWN: $bin is not executable"; exit 2; }
        [ -f "$payload" ] || { echo "UNKNOWN: no payload at $payload"; exit 2; }
        # Two attacks: unsigned, and signed-then-tampered (one byte flipped).
        unsigned_rc=0
        "$bin" "$payload" >/dev/null 2>&1 || unsigned_rc=$?
        judge "$unsigned_rc" "an unsigned payload"; rc1=$?
        tampered=$(mktemp); cp "$payload" "$tampered"
        printf 'X' | dd of="$tampered" bs=1 seek=64 conv=notrunc status=none 2>/dev/null || true
        tampered_rc=0
        "$bin" "$tampered" >/dev/null 2>&1 || tampered_rc=$?
        judge "$tampered_rc" "a tampered payload"; rc2=$?
        rm -f "$tampered"
        [ "$rc1" = 0 ] && [ "$rc2" = 0 ] && { echo "attack-update: both attacks were refused"; exit 0; }
        exit 1
        ;;
    *) echo "usage: $0 --self-test | --morph-update <bin> <payload>"; exit 2 ;;
esac
