#!/usr/bin/env bash
# scripts/ab.sh — the M0c falsifier: does the boot path try a new generation once, commit only what reported healthy,
# fall back when it did not, and remember the failure?
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
MODE=${1:-check}
WORK=${AB_WORK:-target/ab}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: ab.sh [--self-test]" >&2; exit 2 ;; esac
command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }
FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features ab-mutant-commit-always"; fi
export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (M0c)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES || { echo "FAIL: the Core did not build" >&2; exit 1; }
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"; mkdir -p "$WORK"; rm -f "$LOG"
timeout "${AB_TIMEOUT:-25}" qemu-system-aarch64 -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" -nographic -monitor none -serial stdio > "$LOG" 2>&1
echo "== what the Core wrote (AB lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^AB:" | head -7
GOOD="AB: the boot path is a decision, and it never commits what did not report healthy"
FALLBACK="AB: an unhealthy trial falls back to the good generation, and the record remembers it"
if [ "$MODE" = "--self-test" ]; then
    grep -qF "$GOOD" "$LOG" && { echo "FAIL (self-test): commit-always still passed the decision" >&2; exit 1; }
    grep -qF "AB: FAIL an unhealthy trial became permanent" "$LOG" || { echo "FAIL (self-test): the mutation did not report" >&2; exit 1; }
    echo "ok (self-test): committing what booted makes the unhealthy trial permanent, so the check reads the policy"
    exit 0
fi
grep -qF "AB: FAIL" "$LOG" && { echo "FAIL: the Core reported its own failure" >&2; exit 1; }
grep -qF "$FALLBACK" "$LOG" || { echo "FAIL: an unhealthy trial did not fall back" >&2; exit 1; }
grep -qF "$GOOD" "$LOG" || { echo "FAIL: the boot decision never reached its verdict" >&2; exit 1; }
echo "ok: a trial is one-shot, a commit needs health, a failure falls back and is remembered"
