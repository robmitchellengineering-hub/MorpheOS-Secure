#!/usr/bin/env bash
# scripts/m0.sh — the M0 falsifier: does the Core run, and does it say so?
#
# WHY A SCRIPT AND NOT A UNIT TEST. A unit test would need the thing under test to be a program on a host OS. M0's
# whole claim is that there is no OS underneath: the only honest check boots the Core on emulated hardware and reads
# what it wrote to the serial port. So this builds the Core for `x86_64-unknown-none`, boots it in QEMU, and greps.
#
# AND THE CHECK MUST BE ABLE TO FAIL. `--self-test` builds the same image with the banner replaced by a deliberately
# wrong line and requires the check to go RED. A gate that has never failed is a gate nobody has tested.
#
#   scripts/m0.sh              # build, boot, assert
#   scripts/m0.sh --self-test  # prove the assertion can fail
#
# Needs: rustup with the pinned toolchain (rust-toolchain.toml adds x86_64-unknown-none), qemu-system-x86_64.
set -uo pipefail
cd "$(dirname "$0")/.."

MODE=${1:-check}
WORK=${M0_WORK:-target/m0}
case "$MODE" in check|--self-test) ;; *) echo "usage: m0.sh [--self-test]" >&2; exit 2 ;; esac

command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here — this needs a Rust toolchain" >&2; exit 2; }
command -v qemu-system-x86_64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-x86_64 here — nothing can boot the Core" >&2; exit 2; }

command -v as >/dev/null 2>&1 || { echo "REFUSED: no assembler for morph-loader" >&2; exit 2; }
command -v objcopy >/dev/null 2>&1 || { echo "REFUSED: no objcopy to flatten the Core" >&2; exit 2; }

echo "== building the Core for x86_64-unknown-none"
if ! cargo build -q -p morph-core --target x86_64-unknown-none --release; then
    echo "FAIL: the Core did not build" >&2
    exit 1
fi
CORE_ELF=target/x86_64-unknown-none/release/morph-core
[ -f "$CORE_ELF" ] || { echo "FAIL: no ELF at $CORE_ELF" >&2; exit 1; }

# The Core becomes a flat binary: the loader copies those bytes to 2 MiB and jumps to the first one. objcopy starts
# the file at the lowest section address (2 MiB), so the blob has no leading padding to strip.
mkdir -p "$WORK"
if ! objcopy -O binary "$CORE_ELF" "$WORK/core.bin"; then
    echo "FAIL: the Core did not flatten" >&2
    exit 1
fi
echo "   core.bin: $(wc -c < "$WORK/core.bin") bytes"

echo "== building morph-loader (32-bit, the Core embedded)"
if ! as --32 -I "$WORK" -o "$WORK/loader.o" loader/loader.S; then
    echo "FAIL: the loader did not assemble" >&2
    exit 1
fi
if ! ld -m elf_i386 -T loader/loader.ld -o "$WORK/loader.elf" "$WORK/loader.o"; then
    echo "FAIL: the loader did not link" >&2
    exit 1
fi
ELF="$WORK/loader.elf"

BANNER="MORPHEOS-CORE 0.0.1: the Core is running; no Linux, no syscalls, no processes"
EXPECT="$BANNER"
if [ "$MODE" = "--self-test" ]; then
    # The mutation: the same build, the same boot, a line that must NOT be found. If the check passes here it is
    # asserting nothing.
    EXPECT="MORPHEOS-CORE 0.0.1: THIS LINE IS DELIBERATELY WRONG"
fi

mkdir -p "$WORK"
LOG="$WORK/serial.log"
rm -f "$LOG"

echo "== booting morph-loader -> the Core in QEMU (our own 32-bit entry, our GDT, our page tables, long mode)"
# -kernel loads the ELF through QEMU's multiboot loader. isa-debug-exit is how the Core reports an exit code with no
# OS to return to. No display: the serial line is the whole verdict.
timeout 60 qemu-system-x86_64 \
    -kernel "$ELF" \
    -serial "file:$LOG" \
    -display none \
    -no-reboot \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    >/dev/null 2>&1
qemu_rc=$?

echo "== what the Core wrote to COM1"
sed -e 's/\r$//' "$LOG" 2>/dev/null | head -20

if grep -qF "$EXPECT" "$LOG" 2>/dev/null; then
    found=1
else
    found=0
fi

if [ "$MODE" = "--self-test" ]; then
    if [ "$found" -eq 1 ]; then
        echo "FAIL (self-test): the check passed on a line that is deliberately wrong — it asserts nothing" >&2
        exit 1
    fi
    echo "ok (self-test): the check goes red when the Core's line is wrong"
    exit 0
fi

if [ "$found" -ne 1 ]; then
    echo "FAIL: the Core did not report itself. qemu exit $qemu_rc, serial log at $LOG" >&2
    exit 1
fi
echo "ok: the Core booted and reported itself — $BANNER"
