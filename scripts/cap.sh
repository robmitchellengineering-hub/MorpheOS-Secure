#!/usr/bin/env bash
# scripts/cap.sh — the M7c falsifier: may a capability narrow, and can it ever widen?
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
MODE=${1:-check}
WORK=${CAP_WORK:-target/cap}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: cap.sh [--self-test]" >&2; exit 2 ;; esac
command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }
FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features cap-mutant-widen"; fi
export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (M7c)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES || { echo "FAIL: the Core did not build" >&2; exit 1; }
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"; mkdir -p "$WORK"; rm -f "$LOG"
timeout "${CAP_TIMEOUT:-25}" qemu-system-aarch64 -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" -nographic -monitor none -serial stdio > "$LOG" 2>&1
echo "== what the Core wrote (CAP lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^CAP:" | head -3
NARROW="CAP: derived rights 0x1 from 0x3 — narrower, allowed"
REFUSED="CAP: REFUSED widening to 0x6 — held only 0x3"
if [ "$MODE" = "--self-test" ]; then
    grep -qF "$REFUSED" "$LOG" && { echo "FAIL (self-test): widening was enabled and still refused" >&2; exit 1; }
    grep -qF "CAP: FAIL a derivation widened the rights" "$LOG" || { echo "FAIL (self-test): the widened derivation was not reported" >&2; exit 1; }
    echo "ok (self-test): with widening enabled the derivation succeeds, so the refusal is the subset test"
    exit 0
fi
grep -qF "CAP: FAIL" "$LOG" && { echo "FAIL: the Core reported its own failure" >&2; exit 1; }
grep -qF "$NARROW" "$LOG" || { echo "FAIL: a narrower derivation was not allowed" >&2; exit 1; }
grep -qF "$REFUSED" "$LOG" || { echo "FAIL: a widening derivation was not refused" >&2; exit 1; }
echo "ok: rights narrow when asked and refuse to widen — authority only ever decreases"
