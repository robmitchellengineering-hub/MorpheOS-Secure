#!/usr/bin/env bash
# scripts/box.sh — the M7d falsifier: is a box validated ONCE at load, and is a lying claim refused there?
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
MODE=${1:-check}
WORK=${BOX_WORK:-target/box}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: box.sh [--self-test]" >&2; exit 2 ;; esac
command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }
FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features box-mutant-trust-claim"; fi
export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (M7d)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES || { echo "FAIL: the Core did not build" >&2; exit 1; }
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"; mkdir -p "$WORK"; rm -f "$LOG"
timeout "${BOX_TIMEOUT:-25}" qemu-system-aarch64 -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" -nographic -monitor none -serial stdio > "$LOG" 2>&1
echo "== what the Core wrote (BOX lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^BOX:" | head -3
ADMITTED="BOX: admitted vst-box — claim checked once against limit 0x4000, no per-access mask needed"
REFUSED="BOX: REFUSED a claim of 0x9fff against limit 0x4000 signed vendor-signature — refused at LOAD, not at an access"
if [ "$MODE" = "--self-test" ]; then
    grep -qF "$REFUSED" "$LOG" && { echo "FAIL (self-test): trusting the claim still refused the lying box" >&2; exit 1; }
    grep -qF "BOX: FAIL a box that claimed memory it was not granted was admitted" "$LOG" || { echo "FAIL (self-test): the lying box was not reported as admitted" >&2; exit 1; }
    echo "ok (self-test): trusting the claim admits the liar, so the refusal is the load-time check"
    exit 0
fi
grep -qF "BOX: FAIL" "$LOG" && { echo "FAIL: the Core reported its own failure" >&2; exit 1; }
grep -qF "$ADMITTED" "$LOG" || { echo "FAIL: an honest box was not admitted" >&2; exit 1; }
grep -qF "$REFUSED" "$LOG" || { echo "FAIL: a lying claim was not refused at load" >&2; exit 1; }
echo "ok: a box is admitted once at load on its claim, and a lie is refused there — not at an access"
