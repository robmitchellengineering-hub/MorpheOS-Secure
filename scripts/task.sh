#!/usr/bin/env bash
# scripts/task.sh — the M4a falsifier: does the Core hand the CPU to a task on its OWN stack and take it back?
#
# Two claims: the task runs (and reports where its stack is), and it returns to the Core with the value it must.
# AND THE CHECK MUST BE ABLE TO FAIL: `--self-test` builds with `task-mutant-no-switch`, which hands the task the
# Core's own stack, so the stack address the task reports must NOT be inside the task stack — and if the check still
# passes, it is not reading where the task actually ran.
set -uo pipefail
cd "$(dirname "$0")/.."

MODE=${1:-check}
WORK=${TASK_WORK:-target/task}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: task.sh [--self-test]" >&2; exit 2 ;; esac

command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }

FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features task-mutant-no-switch"; fi

export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (M4a)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
if ! cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES; then
    echo "FAIL: the Core did not build" >&2
    exit 1
fi
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"
mkdir -p "$WORK"
rm -f "$LOG"
timeout "${TASK_TIMEOUT:-25}" qemu-system-aarch64 \
    -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" \
    -nographic -monitor none -serial stdio \
    > "$LOG" 2>&1

echo "== what the Core wrote (M4a lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^M4a:|^ok:|^FAIL" | head -8

RAN="M4a: the task ran on its own stack and returned to the Core"

if [ "$MODE" = "--self-test" ]; then
    if grep -qF "$RAN" "$LOG"; then
        echo "FAIL (self-test): the task was given the Core's stack and the run still looked correct" >&2
        exit 1
    fi
    echo "ok (self-test): without the switch the task does not run as a task, so the check reads where it ran"
    exit 0
fi

if grep -qF "M4a: FAIL" "$LOG"; then
    echo "FAIL: the Core reported its own failure" >&2
    exit 1
fi
if ! grep -qF "$RAN" "$LOG"; then
    echo "FAIL: the task did not run on its own stack and return (log $LOG)" >&2
    exit 1
fi
echo "ok: the task ran on its own stack and the Core resumed with its own stack restored"
