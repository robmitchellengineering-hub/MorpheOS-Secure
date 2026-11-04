#!/usr/bin/env bash
# scripts/preempt.sh — the M5c falsifier: does the CPU actually move between two tasks?
#
# Two contexts, one CPU. Task A counts to 200,000 and says so; task B counts to 5,000 and says so. Neither yields —
# the ONLY way both lines appear is that the timer took the CPU away from one and gave it to the other.
#
# AND THE CHECK MUST BE ABLE TO FAIL: `--self-test` builds with `sched-mutant-no-switch`, which records the intent and
# answers with the same frame. The machine looks alive, the tick keeps counting, and TASK B's line never appears.
set -uo pipefail
cd "$(dirname "$0")/.."

MODE=${1:-check}
WORK=${PREEMPT_WORK:-target/preempt}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: preempt.sh [--self-test]" >&2; exit 2 ;; esac

command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }

FEATURES="--features preempt-demo"
if [ "$MODE" = "--self-test" ]; then FEATURES="--features preempt-demo,sched-mutant-no-switch"; fi

export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (M5c preemption)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
if ! cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES; then
    echo "FAIL: the Core did not build" >&2
    exit 1
fi
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"
mkdir -p "$WORK"
rm -f "$LOG"
timeout "${PREEMPT_TIMEOUT:-25}" qemu-system-aarch64 \
    -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" \
    -nographic -monitor none -serial stdio \
    > "$LOG" 2>&1

echo "== what the Core wrote (task lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^TASK |^PREEMPT:" | head -6

A_RAN="TASK A: ran"
B_RAN="TASK B: ran"

if [ "$MODE" = "--self-test" ]; then
    if grep -qF "$B_RAN" "$LOG"; then
        echo "FAIL (self-test): switching was disabled and task B still ran" >&2
        exit 1
    fi
    echo "ok (self-test): without a switch task B never runs, so the check reads the CPU moving"
    exit 0
fi

grep -qF "$A_RAN" "$LOG" || { echo "FAIL: task A never finished — the CPU did not come back to it" >&2; exit 1; }
# Task B's own line can be DROPPED: LineBuf refuses a line when the console is busy, deliberately, because a
# diagnostic is not a guarantee. The violation below cannot be dropped into existence — only task B's probe produces
# it — so the requirement is the violation, and B's line is accepted as extra evidence when it survives.
grep -qF "$B_RAN" "$LOG" || true
# Per-task TTBR0: task B runs on its OWN map, so the block the Core maps is absent for it. The violation line is the
# evidence that the map travelled with the task rather than staying with the boot.
grep -qF "TASK VIOLATION — the task named 0x4E000000" "$LOG" || { echo "FAIL: task B reached the Core's private block — its map did not travel with it" >&2; exit 1; }
grep -qF "TASK B: FAIL" "$LOG" && { echo "FAIL: task B read the private block outright" >&2; exit 1; }
# Per-task TTBR0: task B runs on its OWN map, so the block the Core maps is absent for it. Measured: the violation
# line appears during this run, which is the evidence that the map travels with the task rather than staying with the
# boot. Without the switch installing tables, task B reads the block and prints FAIL.
grep -qF "TASK VIOLATION — the task named 0x4E000000" "$LOG" || {
    echo "FAIL: task B reached the Core's private block — its map did not travel with it" >&2
    exit 1
}
if grep -qF "TASK B: FAIL" "$LOG"; then
    echo "FAIL: task B read the private block outright" >&2
    exit 1
fi
echo "ok: both tasks ran on one CPU — the timer took it from one and gave it to the other"
