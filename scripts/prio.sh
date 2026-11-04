#!/usr/bin/env bash
# scripts/prio.sh — the M5d falsifier: does the audio class take every pick while it is ready?
#
# The claim is in the DECISION, not the timing: six picks with one audio task and one normal task ready must all be
# audio. A timing assertion on a 4-core VM would flake; the policy is where the claim lives, and the timing is its
# consequence.
#
# AND THE CHECK MUST BE ABLE TO FAIL: `--self-test` builds with `sched-mutant-ignore-priority`, which round-robins and
# so takes a normal task's turn while audio is ready.
set -uo pipefail
cd "$(dirname "$0")/.."

MODE=${1:-check}
WORK=${PRIO_WORK:-target/prio}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: prio.sh [--self-test]" >&2; exit 2 ;; esac

export PATH="$HOME/.cargo/bin:$PATH"
command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }

FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features sched-mutant-ignore-priority"; fi

export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (M5d)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
if ! cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES; then
    echo "FAIL: the Core did not build" >&2
    exit 1
fi
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"
mkdir -p "$WORK"
rm -f "$LOG"
timeout "${PRIO_TIMEOUT:-25}" qemu-system-aarch64 -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" -nographic -monitor none -serial stdio > "$LOG" 2>&1

echo "== what the Core wrote (PRIORITY lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^PRIORITY:" | head -3

AUDIO_TOOK_ALL="PRIORITY: the audio class took every pick — a normal task cannot starve it"
FAILED="PRIORITY: FAIL a normal task took a pick the audio class should have had"

if [ "$MODE" = "--self-test" ]; then
    grep -qF "$AUDIO_TOOK_ALL" "$LOG" && { echo "FAIL (self-test): ignoring priority still let audio take every pick" >&2; exit 1; }
    grep -qF "$FAILED" "$LOG" || { echo "FAIL (self-test): the mutation did not report the stolen pick" >&2; exit 1; }
    echo "ok (self-test): with priority ignored a normal task takes a pick, so the check reads the policy"
    exit 0
fi
grep -qF "$FAILED" "$LOG" && { echo "FAIL: a normal task took a pick from the audio class" >&2; exit 1; }
grep -qF "$AUDIO_TOOK_ALL" "$LOG" || { echo "FAIL: the priority decision was never reached" >&2; exit 1; }
echo "ok: the audio class took every pick — priority is decided, not hoped for"
