#!/usr/bin/env bash
# scripts/tick.sh — the M5b falsifier, first half: does the EL1 timer's comparator fire when it is told to?
#
# This is the half of a tick that has nothing to do with the GIC: arm the timer, read its own status, measure when it
# fired. The interrupt half (GIC, IRQ handler, preemption) is the next rung and is NOT claimed here.
#
# AND THE CHECK MUST BE ABLE TO FAIL: `--self-test` builds with `tick-mutant-armed-far`, which arms the timer a
# geological age away. The "fired" line must then be ABSENT.
set -uo pipefail
cd "$(dirname "$0")/.."

MODE=${1:-check}
WORK=${TICK_WORK:-target/tick}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: tick.sh [--self-test]" >&2; exit 2 ;; esac

command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }

FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features tick-mutant-armed-far"; fi

export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (M5b)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
if ! cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES; then
    echo "FAIL: the Core did not build" >&2
    exit 1
fi
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"
mkdir -p "$WORK"
rm -f "$LOG"
timeout "${TICK_TIMEOUT:-30}" qemu-system-aarch64 \
    -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" \
    -nographic -monitor none -serial stdio \
    > "$LOG" 2>&1

echo "== what the Core wrote (timer lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^TICK:|^IRQ:" | head -4

INTERRUPTED="TICK: the Core was INTERRUPTED"
REAL="TICK: the tick is real"
if [ "$MODE" = "--self-test" ]; then
    if grep -qF "$INTERRUPTED" "$LOG"; then
        echo "FAIL (self-test): the timer was armed far out and interrupts still arrived — the check is not reading the GIC" >&2
        exit 1
    fi
    echo "ok (self-test): with the timer armed far out no tick arrives, so the count is the interrupt's"
    exit 0
fi
if grep -qF "TICK: FAIL" "$LOG"; then
    echo "FAIL: the Core reported its own failure in the interrupt check" >&2
    exit 1
fi
grep -qF "$INTERRUPTED" "$LOG" || { echo "FAIL: no interrupt reached the Core" >&2; exit 1; }
grep -qF "$REAL" "$LOG" || { echo "FAIL: interrupts arrived but the Core did not report the tick as real" >&2; exit 1; }
echo "ok: the timer interrupted the Core with no polling, and the count is the interrupt's"
