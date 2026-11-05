#!/usr/bin/env bash
# scripts/loader.sh — the loader's milestone 1: does the CPU execute bytes handed to it at runtime?
#
# The boot jumps to raw AArch64 code (mov x0,#0x42; ret) in .text and prints the return. The ONLY way it prints 66 is
# that the CPU ran those bytes. `--self-test` builds the code returning 0, so the gate must observe the wrong result.
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
MODE=${1:-check}
WORK=${LOADER_WORK:-target/loader}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: loader.sh [--self-test]" >&2; exit 2 ;; esac
command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }
FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features load-mutant-wrong-result"; fi
export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (loader)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES || { echo "FAIL: the Core did not build" >&2; exit 1; }
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"; mkdir -p "$WORK"; rm -f "$LOG"
timeout "${LOADER_TIMEOUT:-25}" qemu-system-aarch64 -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" -nographic -monitor none -serial stdio > "$LOG" 2>&1
echo "== what the Core wrote"
sed -e 's/\r$//' "$LOG" | grep -E "^LOAD:" | head -2
GOOD="LOAD: jumped to loaded code, it returned 66"
if [ "$MODE" = "--self-test" ]; then
    grep -qF "$GOOD" "$LOG" && { echo "FAIL (self-test): the wrong-result mutation still returned 66" >&2; exit 1; }
    grep -qF "LOAD: jumped to loaded code" "$LOG" || { echo "FAIL (self-test): the loaded code did not run at all" >&2; exit 1; }
    echo "ok (self-test): with the wrong bytes loaded, the result is wrong — so the check reads execution, not the source"
    exit 0
fi
grep -qF "$GOOD" "$LOG" || { echo "FAIL: the loaded code did not return 66 — the CPU did not run the bytes" >&2; exit 1; }
echo "ok: the CPU executed bytes the Core handed it at runtime, and read the result back"
