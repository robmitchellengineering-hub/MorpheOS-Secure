#!/usr/bin/env bash
# scripts/m1.sh — the M1 falsifier: does the Core hand out authority as OBJECTS, and refuse what was not handed?
#
# The claim: a task naming an object it does not hold is refused BY THE CORE, and the refusal names both the task and
# the object. Two directions are checked, so a Core that refuses everything fails as loudly as one that allows
# everything.
#
# AND THE CHECK MUST BE ABLE TO FAIL. `--self-test` builds the same Core with `--features m1-mutant-grant`, which hands
# `task_bench` the object the real build refuses. The check must then go RED, because the refusal line is absent. If it
# still passes, the test is asserting a hard-coded string rather than the Core's table.
#
#   scripts/m1.sh              # build, boot, assert both directions
#   scripts/m1.sh --self-test  # prove the assertion can fail
set -uo pipefail
cd "$(dirname "$0")/.."

MODE=${1:-check}
WORK=${M1_WORK:-target/m1}
# Each mode gets its own build directory. Measured the hard way: a shared one let the mutation's binary
# be the one the real check booted, so the self-test ran the clean Core and the clean run executed the mutant.
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: m1.sh [--self-test]" >&2; exit 2 ;; esac

command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }

FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features m1-mutant-grant"; fi

export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (M1)${FEATURES:+ with the mutation: $FEATURES}"
# shellcheck disable=SC2086
if ! cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES; then
    echo "FAIL: the Core did not build" >&2
    exit 1
fi
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
[ -f "$ELF" ] || { echo "FAIL: no ELF at $ELF" >&2; exit 1; }

mkdir -p "$WORK"
LOG="$WORK/serial.log"
rm -f "$LOG"

timeout "${M1_TIMEOUT:-20}" qemu-system-aarch64 \
    -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" \
    -nographic -monitor none -serial stdio \
    > "$LOG" 2>&1

echo "== what the Core wrote"
sed -e 's/\r$//' "$LOG" | head -24

# The four verdicts the Core must have reached, with the two refusals coming from the Core's table.
ALLOW_BENCH="M1: ALLOWED  task_bench -> object measurement"
DENY_BENCH="M1: REFUSED  task_bench -> object plugin-state (no capability)"
ALLOW_PLUGIN="M1: ALLOWED  task_plugin -> object plugin-state"
DENY_PLUGIN="M1: REFUSED  task_plugin -> object measurement (no capability)"

missing=""
for line in "$ALLOW_BENCH" "$DENY_BENCH" "$ALLOW_PLUGIN" "$DENY_PLUGIN"; do
    grep -qF "$line" "$LOG" || missing="$missing
  - $line"
done

if [ "$MODE" = "--self-test" ]; then
    # With the mutation on, the Core MUST allow what it refuses in the real build. If the denial lines are still
    # present, the mutation did nothing and the check is not testing the table.
    if grep -qF "$DENY_BENCH" "$LOG" || grep -qF "$DENY_PLUGIN" "$LOG"; then
        echo "FAIL (self-test): the mutation did not change the verdict — the check is not reading the Core's table" >&2
        exit 1
    fi
    echo "ok (self-test): granting the capability changed the verdict, so the check is the Core's table"
    exit 0
fi

if [ -n "$missing" ]; then
    echo "FAIL: the Core did not reach the four verdicts it must. Missing:$missing" >&2
    exit 1
fi
echo "ok: the Core handed out objects (both directions allowed) and refused what was not handed (both directions named)"
