#!/usr/bin/env bash
# scripts/sched.sh — the M5c policy falsifier: does the Core choose between two tasks, or only ever the same one?
#
# The claim is a SEQUENCE, not a count: a scheduler that always picks task 0 records switches happily and never runs
# anything else. That is the failure this checks for.
#
# AND THE CHECK MUST BE ABLE TO FAIL: `--self-test` builds with `sched-mutant-always-first`, which always returns task
# 0. The alternating line must then be absent and the FAIL line present.
set -uo pipefail
cd "$(dirname "$0")/.."

MODE=${1:-check}
WORK=${SCHED_WORK:-target/sched}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: sched.sh [--self-test]" >&2; exit 2 ;; esac

command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }

FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features sched-mutant-always-first"; fi

export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (M5c policy)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
if ! cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES; then
    echo "FAIL: the Core did not build" >&2
    exit 1
fi
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"
mkdir -p "$WORK"
rm -f "$LOG"
timeout "${SCHED_TIMEOUT:-25}" qemu-system-aarch64 \
    -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" \
    -nographic -monitor none -serial stdio \
    > "$LOG" 2>&1

echo "== what the Core wrote (SCHED lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^SCHED:" | head -4

ALTERNATES="SCHED: picks 1,0,1,0,1,0 — the choice alternates, so a second task really runs"
FAILED="SCHED: FAIL the choice did not alternate"

if [ "$MODE" = "--self-test" ]; then
    if grep -qF "$ALTERNATES" "$LOG"; then
        echo "FAIL (self-test): always picking task 0 still produced an alternating sequence" >&2
        exit 1
    fi
    grep -qF "$FAILED" "$LOG" || { echo "FAIL (self-test): the mutation did not report a stuck choice" >&2; exit 1; }
    echo "ok (self-test): always picking task 0 is caught, so the check reads the sequence"
    exit 0
fi

grep -qF "$FAILED" "$LOG" && { echo "FAIL: the Core reported a stuck choice" >&2; exit 1; }
grep -qF "$ALTERNATES" "$LOG" || { echo "FAIL: the choice never alternated" >&2; exit 1; }
echo "ok: the Core alternates between two ready tasks — the policy makes a real choice"
