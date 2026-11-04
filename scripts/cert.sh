#!/usr/bin/env bash
# scripts/cert.sh — the M7a falsifier: does the Core check a schedulability certificate exactly, refuse what does not
# fit, name the term that broke it, refuse a lying certificate, and do it all without allocating?
#
# AND THE CHECK MUST BE ABLE TO FAIL: `--self-test` builds with `cert-mutant-skip-check`, which accepts everything, so
# the over-budget and lying cases must be ADMITTED and the gate must go red.
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

MODE=${1:-check}
WORK=${CERT_WORK:-target/cert}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: cert.sh [--self-test]" >&2; exit 2 ;; esac

command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }

FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features cert-mutant-skip-check"; fi

export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (M7a)${FEATURES:+ $FEATURES}"
# shellcheck disable=SC2086
if ! cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES; then
    echo "FAIL: the Core did not build" >&2; exit 1
fi
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
LOG="$WORK/serial.log"; mkdir -p "$WORK"; rm -f "$LOG"
timeout "${CERT_TIMEOUT:-25}" qemu-system-aarch64 -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" -nographic -monitor none -serial stdio > "$LOG" 2>&1

echo "== what the Core wrote (CERT lines)"
sed -e 's/\r$//' "$LOG" | grep -E "^CERT:" | head -6

ADMIT="CERT: fitting graph admitted — utilisation 350000 ppm"
OVER="CERT: over budget REFUSED — the term that broke it is engine"
LIE="CERT: lying certificate REFUSED"
LEDGER_OK="CERT: generation 0 applied with a certificate at 350000 ppm"
LEDGER_REFUSED="CERT: a generation was REFUSED — engine broke the budget, and the ledger's head did not move (0 -> 0)"
BOUNDED="CERT: the check allocated nothing, so a hostile certificate cannot make it expensive"

if [ "$MODE" = "--self-test" ]; then
    grep -qF "$OVER" "$LOG" && { echo "FAIL (self-test): the skip-check mutation still refused the over-budget graph" >&2; exit 1; }
    grep -qF "$LIE" "$LOG" && { echo "FAIL (self-test): the skip-check mutation still caught the lie" >&2; exit 1; }
    echo "ok (self-test): with the check skipped both bad certificates are admitted, so the refusals are the arithmetic's"
    exit 0
fi
grep -qF "CERT: FAIL" "$LOG" && { echo "FAIL: the Core reported its own failure" >&2; exit 1; }
missing=""
for line in "$ADMIT" "$OVER" "$LIE" "$LEDGER_OK" "$LEDGER_REFUSED" "$BOUNDED"; do
    grep -qF "$line" "$LOG" || missing="$missing
  - $line"
done
if [ -n "$missing" ]; then
    echo "FAIL: the certificate check did not reach the states it must. Missing:$missing" >&2; exit 1
fi
echo "ok: exact utilisation checked, over-budget refused and named, a lying certificate refused, no allocation"
