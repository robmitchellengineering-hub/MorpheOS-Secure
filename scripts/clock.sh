#!/usr/bin/env bash
# scripts/clock.sh — the M5a falsifier: does the Core read the hardware's counter, know its frequency, and honour a
# deadline measured in real time?
#
# AND THE CHECK MUST BE ABLE TO FAIL: `--self-test` builds with `tick-mutant-frozen`, a clock that always reads 0.
# The advance must then be zero and the deadline must be missed, so the check goes red.
set -uo pipefail
cd "$(dirname "$0")/.."

MODE=${1:-check}
WORK=${CLOCK_WORK:-target/clock}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: clock.sh [--self-test]" >&2; exit 2 ;; esac

command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }

FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features tick-mutant-frozen"; fi

export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (M5a)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
if ! cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES; then
    echo "FAIL: the Core did not build" >&2
    exit 1
fi
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"
mkdir -p "$WORK"
rm -f "$LOG"
timeout "${CLOCK_TIMEOUT:-25}" qemu-system-aarch64 \
    -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" \
    -nographic -monitor none -serial stdio \
    > "$LOG" 2>&1

echo "== what the Core wrote (TICK lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^TICK:" | head -8

if [ "$MODE" = "--self-test" ]; then
    if grep -qF "TICK: the counter advances" "$LOG" || grep -qF "TICK: the deadline was honoured" "$LOG"; then
        echo "FAIL (self-test): the frozen clock still passed a check — the check is not reading the counter" >&2
        exit 1
    fi
    if ! grep -qF "TICK: FAIL the counter did not advance" "$LOG"; then
        echo "FAIL (self-test): the frozen clock did not report the failure it must" >&2
        exit 1
    fi
    echo "ok (self-test): with the clock frozen both checks go red, so they read the hardware's counter"
    exit 0
fi

missing=""
grep -qE "^TICK: CNTFRQ_EL0 = 0x0*[1-9a-f]" "$LOG" || missing="$missing
  - a non-zero CNTFRQ_EL0"
grep -qF "TICK: the counter advances" "$LOG" || missing="$missing
  - the counter advancing"
grep -qF "TICK: the deadline was honoured in real time (within 3 ms of a 1 ms target)" "$LOG" || missing="$missing
  - a 1 ms deadline honoured"
if grep -qF "TICK: FAIL" "$LOG"; then
    echo "FAIL: the Core reported its own failure in the clock check" >&2
    exit 1
fi
if [ -n "$missing" ]; then
    echo "FAIL: the clock did not reach the states it must. Missing:$missing" >&2
    exit 1
fi
echo "ok: the hardware's counter advances, its frequency is known, and a 1 ms deadline was honoured"
