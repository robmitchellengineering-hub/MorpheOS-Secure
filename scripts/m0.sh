#!/usr/bin/env bash
# scripts/m0.sh — the M0 falsifier: does the Core run on the Pi's architecture, and does it say so?
#
# WHY A SCRIPT AND NOT A UNIT TEST. A unit test would need the thing under test to be a program on a host OS. M0's
# claim is that there is no OS underneath: the only honest check boots the Core and reads what it wrote to the serial
# line. So this builds the Core for `aarch64-unknown-none`, boots it on QEMU's `virt` machine with the Pi 5's core
# (`cortex-a76`), and greps — the same architecture the Pi runs, so the code under test is the Pi's code.
#
# AND THE CHECK MUST BE ABLE TO FAIL. `--self-test` boots the same image but requires a deliberately wrong line, and
# must go RED. A gate that has never failed is a gate nobody has tested.
#
#   scripts/m0.sh              # build, boot, assert
#   scripts/m0.sh --self-test  # prove the assertion can fail
#
# Needs: rustup with the pinned toolchain (rust-toolchain.toml adds aarch64-unknown-none), qemu-system-aarch64.
set -uo pipefail
cd "$(dirname "$0")/.."

MODE=${1:-check}
WORK=${M0_WORK:-target/m0}
# Each mode gets its own build directory. Measured the hard way: a shared one let the mutation's binary
# be the one the real check booted, so the self-test ran the clean Core and the clean run executed the mutant.
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: m0.sh [--self-test]" >&2; exit 2 ;; esac

command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here — this needs a Rust toolchain" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here — nothing can boot the Core" >&2; exit 2; }

export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core for aarch64-unknown-none (the Pi 5's architecture)"
if ! cargo build -q -p morph-core --target aarch64-unknown-none --release; then
    echo "FAIL: the Core did not build" >&2
    exit 1
fi
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
[ -f "$ELF" ] || { echo "FAIL: no ELF at $ELF" >&2; exit 1; }

BANNER="MORPHEOS-CORE 0.0.1: the Core is running on aarch64; no Linux, no syscalls, no processes"
DONE_MARK="MORPHEOS-CORE: reported; halting"
EXPECT="$BANNER"
if [ "$MODE" = "--self-test" ]; then
    # The mutation: the same build, the same boot, a line that must NOT be found. If the check passes here, it is
    # asserting nothing.
    EXPECT="MORPHEOS-CORE 0.0.1: THIS LINE IS DELIBERATELY WRONG"
fi

mkdir -p "$WORK"
LOG="$WORK/serial.log"
rm -f "$LOG"

echo "== booting it on QEMU virt, cortex-a76, with no Linux anywhere in the path"
# -M virt: QEMU's machine model for the aarch64 world. -cpu cortex-a76: the Pi 5's actual core. -nographic: the serial
# line is the whole verdict. The Core halts after it has reported, and the timeout stops the emulator if it does not.
timeout "${M0_TIMEOUT:-20}" qemu-system-aarch64 \
    -M virt -cpu cortex-a76 -m 256M \
    -kernel "$ELF" \
    -nographic -monitor none -serial stdio \
    > "$LOG" 2>&1
qemu_rc=$?

echo "== what the Core wrote to the serial line"
sed -e 's/\r$//' "$LOG" | head -20

if grep -qF "$EXPECT" "$LOG"; then found=1; else found=0; fi

if [ "$MODE" = "--self-test" ]; then
    if [ "$found" -eq 1 ]; then
        echo "FAIL (self-test): the check passed on a line that is deliberately wrong — it asserts nothing" >&2
        exit 1
    fi
    echo "ok (self-test): the check goes red when the Core's line is wrong"
    exit 0
fi

if [ "$found" -ne 1 ]; then
    echo "FAIL: the Core did not report itself (qemu exit $qemu_rc, log $LOG)" >&2
    exit 1
fi
if ! grep -qF "$DONE_MARK" "$LOG"; then
    echo "FAIL: the banner appeared but the Core never reported that it finished" >&2
    exit 1
fi
echo "ok: the Core booted on aarch64 and reported itself"
