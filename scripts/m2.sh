#!/usr/bin/env bash
# scripts/m2.sh — the M2 falsifier: is the machine its history, and does it refuse a generation that does not check out?
#
# Three claims, all read off the serial line:
#   1. state is generations     — applied, with the parent named;
#   2. undo without a reboot    — walking back one step lands on the parent's payload;
#   3. a tampered generation is REFUSED — a generation corrupted in place is caught by its checksum.
#
# AND THE CHECK MUST BE ABLE TO FAIL. The real check builds with `m2-test-tamper` (the attack happens) and requires the
# refusal. `--self-test` builds with both `m2-test-tamper` and `m2-mutant-skip-verify` (verification off) and requires
# the refusal to be ABSENT — if it is still there, the check is not reading the ledger's verification at all.
set -uo pipefail
cd "$(dirname "$0")/.."

MODE=${1:-check}
WORK=${M2_WORK:-target/m2}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: m2.sh [--self-test]" >&2; exit 2 ;; esac

command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }

FEATURES="--features m2-test-tamper"
if [ "$MODE" = "--self-test" ]; then FEATURES="--features m2-test-tamper,m2-mutant-skip-verify"; fi

export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (M2) $FEATURES"
# shellcheck disable=SC2086
if ! cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES; then
    echo "FAIL: the Core did not build" >&2
    exit 1
fi
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"

LOG="$WORK/serial.log"
mkdir -p "$WORK"
rm -f "$LOG"
timeout "${M2_TIMEOUT:-20}" qemu-system-aarch64 \
    -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" \
    -nographic -monitor none -serial stdio \
    > "$LOG" 2>&1

echo "== what the Core wrote (M2 lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^M2:|^MORPHEOS-CORE: reported" | head -16

GREW="M2: applied generation 1 (parent 0)"
UNDO="M2: undo 1 -> generation 0 reads 0xAAAA ok (no reboot)"
TAMPERED="M2: TAMPERED generation 0 REFUSED (checksum does not match)"

if [ "$MODE" = "--self-test" ]; then
    if grep -qF "$TAMPERED" "$LOG"; then
        echo "FAIL (self-test): verification was disabled and the tampered generation was STILL refused — the check is not reading the ledger" >&2
        exit 1
    fi
    if ! grep -qF "verification is DISABLED" "$LOG"; then
        echo "FAIL (self-test): the mutation did not take effect — no sign that verification was off" >&2
        exit 1
    fi
    echo "ok (self-test): with verification off the tampered generation is accepted, so the refusal is the ledger's"
    exit 0
fi

missing=""
for line in "$GREW" "$UNDO" "$TAMPERED"; do
    grep -qF "$line" "$LOG" || missing="$missing
  - $line"
done
if [ -n "$missing" ]; then
    echo "FAIL: the ledger did not reach the states it must. Missing:$missing" >&2
    exit 1
fi
echo "ok: generations applied, undo landed on the parent without a reboot, and a tampered generation was refused"
