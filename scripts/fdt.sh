#!/usr/bin/env bash
# scripts/fdt.sh — the M0b-preparation falsifier: does the Core read the UART's address from the firmware's device tree?
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
MODE=${1:-check}
WORK=${FDT_WORK:-target/fdt}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: fdt.sh [--self-test]" >&2; exit 2 ;; esac
command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }
FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features fdt-mutant-skip-walk"; fi
export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (FDT)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES || { echo "FAIL: the Core did not build" >&2; exit 1; }
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"; mkdir -p "$WORK"; rm -f "$LOG"
timeout "${FDT_TIMEOUT:-25}" qemu-system-aarch64 -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" -nographic -monitor none -serial stdio > "$LOG" 2>&1
echo "== what the Core wrote (FDT lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^FDT:" | head -5
AGREE="FDT: the tree AGREES with the constant — on the Pi the tree wins"
if [ "$MODE" = "--self-test" ]; then
    grep -qF "$AGREE" "$LOG" && { echo "FAIL (self-test): the walk was disabled and the tree still answered" >&2; exit 1; }
    grep -qF "FDT: FAIL no arm,pl011 node was found in the tree" "$LOG" || { echo "FAIL (self-test): the disabled walk did not report" >&2; exit 1; }
    echo "ok (self-test): with the walk disabled there is no answer, so the address comes from the firmware's tree"
    exit 0
fi
grep -qF "FDT: FAIL" "$LOG" && { echo "FAIL: the Core reported its own failure" >&2; exit 1; }
grep -qF "$AGREE" "$LOG" || { echo "FAIL: the device tree did not agree with the constant this boot uses" >&2; exit 1; }
echo "ok: the UART's address was read from the firmware's device tree, and it matches what the boot uses"
