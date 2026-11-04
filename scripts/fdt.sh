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
# THE IMAGE PATH: a bare ELF gets no device tree in x0 (measured: x0 == 0), so this gate converts the same binary to an
# arm64 Image and boots THAT. The header is in the source; objcopy makes the flat image the firmware protocol expects.
IMG="$WORK/morph-core.img"
# HOST OBJCOPY CANNOT READ AN AARCH64 ELF. Measured: "Unable to recognise the format of the input file" while the build
# was green all along. The LLVM objcopy that ships with the Rust toolchain understands every target it can emit.
OBJCOPY=$(command -v llvm-objcopy || true)
if [ -z "$OBJCOPY" ]; then
    SYSROOT=$(rustc --print sysroot)
    for candidate in "$SYSROOT"/lib/rustlib/*/bin/llvm-objcopy; do
        [ -x "$candidate" ] && OBJCOPY="$candidate" && break
    done
fi
[ -n "$OBJCOPY" ] || { echo "REFUSED: no llvm-objcopy to make an Image from an aarch64 ELF" >&2; exit 2; }
echo "== objcopy: $OBJCOPY"
if ! "$OBJCOPY" -O binary "$ELF" "$IMG"; then
    echo "FAIL: could not make the Image" >&2; exit 1
fi
# image_size is a field the boot protocol reads, and a zero there is not a valid Image. It is patched here from the
# image's own length rather than guessed: offset 8 text_offset, 16 image_size, both little-endian 64-bit.
python3 - "$IMG" <<'PATCH'
import struct, sys
path = sys.argv[1]
data = bytearray(open(path, 'rb').read())
magic = struct.unpack_from('<I', data, 56)[0]
if magic != 0x644d5241:
    sys.exit("the image does not carry the ARM\x64 marker")
struct.pack_into('<Q', data, 16, len(data))
open(path, 'wb').write(data)
print(f"image_size = {len(data)}")
PATCH
timeout "${FDT_TIMEOUT:-25}" qemu-system-aarch64 -M virt -cpu cortex-a76 -m 256M \
    -kernel "$IMG" -nographic -monitor none -serial stdio > "$LOG" 2>&1
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
