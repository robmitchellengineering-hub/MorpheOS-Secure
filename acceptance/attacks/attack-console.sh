#!/bin/bash
# Attack A5 — the console must ask before it gives a shell.
#
# THE CLAIM: "the console is protected." Today it is not: `HARDWARE.md` says it in its own words —
# Ctrl+Alt+F2 gives "a root shell, with no password". Anyone with hands is root in thirty seconds,
# which is the largest single finding in the red-team record. This script exists to keep that claim
# falsifiable: it reads what the console actually showed and says whether a credential was demanded.
#
#   images/x86-qemu/attack-console.sh --log <capture>   # judge a capture (any machine)
#   images/x86-qemu/attack-console.sh --self-test       # prove the judgement can go both ways
#   images/x86-qemu/attack-console.sh --vm <image>      # boot it in QEMU and do it for real (Linux)
#
# Exit 0 = the console demanded a credential. 1 = it gave a shell without one (the finding).
# 2 = could not tell — which is a failure of the test, never a pass.
set -uo pipefail

verdict() {  # verdict <file> -> prints PASS/FAIL/UNKNOWN and exits
    local log=$1
    [ -f "$log" ] || { echo "UNKNOWN: no capture at $log"; return 2; }
    # A credential was demanded if the console asks for it after the switch to VT2.
    if grep -aqE '(^|[[:space:]])(login:|Password:)' "$log"; then
        echo "PASS: the console asked for a credential"
        return 0
    fi
    # It gave a shell without asking if a root prompt appears with no login line before it.
    if grep -aqE '(^|[[:space:]])(root@[^ ]*)?[#$][[:space:]]*$' "$log"; then
        echo "FAIL: the console gave a shell without a credential (Ctrl+Alt+F2 is root)"
        return 1
    fi
    echo "UNKNOWN: the capture shows neither a prompt nor a shell — the test did not reach the console"
    return 2
}

self_test() {
    local dir fail=0
    dir=$(mktemp -d)
    printf 'Welcome to MorpheOS\nmorpheos-a login: ' > "$dir/protected.txt"
    printf 'morpheos-a login: root\nPassword: \n' > "$dir/protected2.txt"
    printf '\nMorpheosOS\n\nroot@morpheos-a:~# ' > "$dir/open.txt"
    printf 'booting...\n' > "$dir/nothing.txt"
    expect() { local name=$1 want=$2 got; verdict "$3" >/dev/null; got=$?;
        if [ "$got" = "$want" ]; then echo "ok   $name"; else echo "FAIL $name (got $got, want $want)"; fail=1; fi; }
    expect "a login prompt passes" 0 "$dir/protected.txt"
    expect "a login plus password prompt passes" 0 "$dir/protected2.txt"
    expect "a bare root shell is caught" 1 "$dir/open.txt"
    expect "a capture that never reached the console is UNKNOWN" 2 "$dir/nothing.txt"
    rm -rf "$dir"
    [ "$fail" = 0 ] && echo "attack-console: the judgement can go both ways" || echo "attack-console: the judgement cannot fail"
    return "$fail"
}

case "${1:-}" in
    --self-test) self_test; exit $? ;;
    --log) shift; verdict "${1:?usage: --log <capture>}"; exit $? ;;
    --vm)
        img=${2:?usage: --vm <image>}
        # Real run: only on a machine that can drive QEMU. The switch to VT2 is sent on the serial
        # console; the capture is the serial log the boot driver already writes.
        command -v qemu-system-x86_64 >/dev/null 2>&1 || { echo "UNKNOWN: no qemu here (this runs on the Linux node)"; exit 2; }
        out=$(mktemp)
        images/x86-qemu/test-shell.sh "$img" 2>/dev/null | grep -aE 'login:|root@|#' > "$out" || true
        verdict "$out"; rc=$?
        rm -f "$out"; exit "$rc"
        ;;
    *) echo "usage: $0 --self-test | --log <capture> | --vm <image>"; exit 2 ;;
esac
