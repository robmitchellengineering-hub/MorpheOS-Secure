#!/usr/bin/env bash
# scripts/isolation.sh — the M4b falsifier: does the task get an address space of its OWN, with a block that does not
# exist in it?
#
# The claim is not "the Core refused the task". It is "the address is absent from the task's map", which is a stronger
# and different statement: refusal can be argued with, an absent mapping cannot.
#
# AND THE CHECK MUST BE ABLE TO FAIL: `--self-test` builds with `task-mutant-share-tables`, which gives the task the
# Core's own tables. The read must then succeed and the check must go red.
set -uo pipefail
cd "$(dirname "$0")/.."

MODE=${1:-check}
WORK=${ISO_WORK:-target/isolation}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: isolation.sh [--self-test]" >&2; exit 2 ;; esac

command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }

FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features task-mutant-share-tables"; fi

export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (M4b)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
if ! cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES; then
    echo "FAIL: the Core did not build" >&2
    exit 1
fi
ELF="$WORK/target/aarch64-unknown-none/release/morphe-core" 2>/dev/null || true
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"
mkdir -p "$WORK"
rm -f "$LOG"
timeout "${ISO_TIMEOUT:-25}" qemu-system-aarch64 \
    -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" \
    -nographic -monitor none -serial stdio \
    > "$LOG" 2>&1

echo "== what the Core wrote (M4b lines)"
sed -e 's/\r$//' "$LOG" | grep -E "M4b:|M4a: task frame" | head -8

VIOLATION="M4b: TASK VIOLATION — the task named 0x4E000000, which is absent from its own map"
ABORTED="M4c: TASK ABORTED — the Core resumed on its own stack, the evidence stays, the boot continues"
DONE_MARK="MORPHEOS-CORE: reported; halting"
REACHED="M4b: FAIL the task read the Core's memory"

if [ "$MODE" = "--self-test" ]; then
    if grep -qF "$VIOLATION" "$LOG"; then
        echo "FAIL (self-test): the task was given the Core's tables and was still stopped — the check is not the map's doing" >&2
        exit 1
    fi
    if ! grep -qF "$REACHED" "$LOG"; then
        echo "FAIL (self-test): with the Core's tables the task did not report reaching the private block" >&2
        exit 1
    fi
    echo "ok (self-test): with the Core's tables the task reads the block, so the violation is the map's doing"
    exit 0
fi

if grep -qF "$REACHED" "$LOG"; then
    echo "FAIL: the task read memory its map should not contain" >&2
    exit 1
fi
if ! grep -qF "$VIOLATION" "$LOG"; then
    echo "FAIL: the task never hit the violation it must (log $LOG)" >&2
    exit 1
fi
# M4c: the machine must have SURVIVED the task. A violation with no abort is still a dead machine.
if ! grep -qF "$ABORTED" "$LOG"; then
    echo "FAIL: the violation was reported but the Core never aborted the task and continued" >&2
    exit 1
fi
if ! grep -qF "$DONE_MARK" "$LOG"; then
    echo "FAIL: the Core did not reach its own end after the abort - the machine did not survive the task" >&2
    exit 1
fi
echo "ok: the task's map lacked the block, the violation was caught, the task was aborted and the Core reached its end"
