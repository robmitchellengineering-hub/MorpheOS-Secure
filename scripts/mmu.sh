#!/usr/bin/env bash
# scripts/mmu.sh — the MMU falsifier: are the page tables ours, and is unmapped memory unreachable?
#
# Three claims:
#   1. the MMU is on, running on tables this repository built (MAIR/TCR/TTBR0/SCTLR, not a firmware inheritance);
#   2. a mapped read still works afterwards (a wrong table would fault on the next instruction);
#   3. memory we never mapped is NOT reachable — the guard page faults, the handler names the address, and the Core
#      keeps running, which is containment rather than politeness.
#
# AND THE CHECK MUST BE ABLE TO FAIL. `--self-test` builds with `--features mmu-mutant-map-guard`, which maps the guard
# page; the fault must then NOT happen, and the check must go red.
set -uo pipefail
cd "$(dirname "$0")/.."

MODE=${1:-check}
WORK=${MMU_WORK:-target/mmu}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: mmu.sh [--self-test]" >&2; exit 2 ;; esac

command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }

FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features mmu-mutant-map-guard"; fi

export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (MMU)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
if ! cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES; then
    echo "FAIL: the Core did not build" >&2
    exit 1
fi
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"
mkdir -p "$WORK"
rm -f "$LOG"
timeout "${MMU_TIMEOUT:-25}" qemu-system-aarch64 \
    -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" \
    -nographic -monitor none -serial stdio \
    > "$LOG" 2>&1

echo "== what the Core wrote (MMU lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^MMU:|^  MMU:|^MORPHEOS-CORE: reported" | head -12

ENABLED="MMU: ENABLED — SCTLR_EL1.M is set"
MAPPED="MMU: a mapped read still works"
FAULTED="MMU: guard page FAULTED as required"
CONTINUED="MMU: the Core continued past the guard probe"

if [ "$MODE" = "--self-test" ]; then
    if grep -qF "$FAULTED" "$LOG"; then
        echo "FAIL (self-test): the guard page was mapped by the mutation and the fault still happened" >&2
        exit 1
    fi
    if ! grep -qF "MMU: FAIL the guard page was readable" "$LOG"; then
        echo "FAIL (self-test): with the guard mapped the Core did not report the readable page" >&2
        exit 1
    fi
    echo "ok (self-test): with the guard mapped the read succeeds, so the fault is the tables' doing"
    exit 0
fi

missing=""
for line in "$ENABLED" "$MAPPED" "$FAULTED" "$CONTINUED"; do
    grep -qF "$line" "$LOG" || missing="$missing
  - $line"
done
if [ -n "$missing" ]; then
    echo "FAIL: the MMU did not reach the states it must. Missing:$missing" >&2
    exit 1
fi
echo "ok: the MMU is on our tables, a mapped read works, and the unmapped guard page faulted and was survived"
