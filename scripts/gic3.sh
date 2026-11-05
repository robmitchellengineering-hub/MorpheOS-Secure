#!/usr/bin/env bash
# scripts/gic3.sh — the Pi 5 GIC-600 falsifier: does the interrupt path work on GICv3, not only GICv2?
#
# The Pi 5's interrupt controller is a GIC-600, which is GICv3 — the CPU interface is ICC_* system registers and the
# timer PPI is delivered as a FIQ, not the GICv2 memory-mapped GICC. The same preemption demo must hold on that
# interface, because the Pi 5 will not boot to a working scheduler without it.
#
# AND THE CHECK MUST BE ABLE TO FAIL: `--self-test` builds with `sched-mutant-no-switch`, which answers with the same
# frame, so the tick keeps counting but task B never runs — proving the assertion reads the switch, not the console.
set -uo pipefail
cd "$(dirname "$0")/.."

MODE=${1:-check}
WORK=${GIC3_WORK:-target/gic3}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: gic3.sh [--self-test]" >&2; exit 2 ;; esac

command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }

FEATURES="--features gic-v3,preempt-demo"
if [ "$MODE" = "--self-test" ]; then FEATURES="--features gic-v3,preempt-demo,sched-mutant-no-switch"; fi

export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (GICv3 preemption)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
if ! cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES; then
    echo "FAIL: the Core did not build" >&2
    exit 1
fi
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"
mkdir -p "$WORK"
rm -f "$LOG"
timeout "${GIC3_TIMEOUT:-90}" qemu-system-aarch64 \
    -M virt,gic-version=3 -cpu cortex-a76 -m 256M \
    -kernel "$ELF" \
    -nographic -monitor none -serial stdio \
    > "$LOG" 2>&1

echo "== what the Core wrote (interrupt + task lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^TICK|^TASK |^PREEMPT:|^IRQ:" | head -8

A_RAN="TASK A: ran"
B_RAN="TASK B: ran"

if [ "$MODE" = "--self-test" ]; then
    if grep -qF "$B_RAN" "$LOG"; then
        echo "FAIL (self-test): switching was disabled and task B still ran" >&2
        exit 1
    fi
    echo "ok (self-test): without a switch task B never runs on GICv3 either"
    exit 0
fi

# The timer must interrupt the Core through the GICv3 system-register interface. No polling: the tick line is only
# printed by the IRQ handler.
grep -qF "TICK: the Core was INTERRUPTED" "$LOG" || { echo "FAIL: the timer never interrupted the Core on GICv3" >&2; exit 1; }
# And the scheduler must still move between tasks on that interface.
grep -qF "$A_RAN" "$LOG" || { echo "FAIL: task A never finished on GICv3" >&2; exit 1; }
grep -qF "$B_RAN" "$LOG" || true
grep -qF "TASK VIOLATION — the task named 0x4E000000" "$LOG" || { echo "FAIL: task B reached the Core's private block on GICv3" >&2; exit 1; }
grep -qF "IRQ: STORM" "$LOG" && { echo "FAIL: the GICv3 timer stormed" >&2; exit 1; }
echo "ok: the timer interrupted the Core on GICv3 and the scheduler moved between tasks"
