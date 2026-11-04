#!/usr/bin/env bash
# scripts/m2b.sh — the M2b falsifier: does the Core allocate through its own allocator, reuse freed memory, and REFUSE
# a double free?
#
# The double free is the attack that matters: a free list that inserts a block twice hands the same bytes to two
# owners. The check requires the refusal line; `--self-test` builds with `--features m2b-mutant-allow-double-free` and
# requires the refusal to be ABSENT and the failure line to be present — if the refusal is still there, the check is
# reading a string rather than the allocator's state.
set -uo pipefail
cd "$(dirname "$0")/.."

MODE=${1:-check}
WORK=${M2B_WORK:-target/m2b}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: m2b.sh [--self-test]" >&2; exit 2 ;; esac

command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }

FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features m2b-mutant-allow-double-free"; fi

export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (M2b)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
if ! cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES; then
    echo "FAIL: the Core did not build" >&2
    exit 1
fi
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"
mkdir -p "$WORK"
rm -f "$LOG"
timeout "${M2B_TIMEOUT:-20}" qemu-system-aarch64 \
    -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" \
    -nographic -monitor none -serial stdio \
    > "$LOG" 2>&1

echo "== what the Core wrote (M2b lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^M2b:|^MORPHEOS-CORE: reported" | head -12

REFUSED="M2b: DOUBLE FREE REFUSED — the block was not inserted twice"
ACCEPTED="M2b: FAIL a double free was accepted; two owners can now hold the same bytes"
REUSED="M2b: the freed block was reused by the next allocation (first fit, as designed)"
FIRST_OK="M2b: free#1 -> accepted=0x1"
SECOND_REFUSED="M2b: free#2 -> accepted=0x0"
SECOND_ACCEPTED="M2b: free#2 -> accepted=0x1"

if [ "$MODE" = "--self-test" ]; then
    # The mutation accepts a second free. Under it, free#2 must be ACCEPTED and the refusal line must be absent —
    # otherwise the check is reading a string, not the allocator's flag.
    if grep -qF "$SECOND_REFUSED" "$LOG"; then
        echo "FAIL (self-test): the mutation accepted the double free but free#2 still reported a refusal" >&2
        exit 1
    fi
    if ! grep -qF "$SECOND_ACCEPTED" "$LOG" || ! grep -qF "$ACCEPTED" "$LOG"; then
        echo "FAIL (self-test): with the guard off the allocator did not report the acceptance" >&2
        exit 1
    fi
    echo "ok (self-test): with the guard off free#2 is accepted, so the refusal is the allocator's own flag"
    exit 0
fi

missing=""
for line in "$REUSED" "$FIRST_OK" "$SECOND_REFUSED" "$REFUSED"; do
    grep -qF "$line" "$LOG" || missing="$missing
  - $line"
done
if [ -n "$missing" ]; then
    echo "FAIL: the allocator did not reach the states it must. Missing:$missing" >&2
    exit 1
fi
echo "ok: allocated and reused through the Core's own allocator, and the second free was refused"
