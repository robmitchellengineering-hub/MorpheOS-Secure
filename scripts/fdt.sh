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
# a 64 MiB disk for the block device; created once so the drive exists to be read
python3 scripts/make-disk.py "$WORK/disk.img"
# THE IMAGE PATH: a bare ELF gets no device tree in x0 (measured: x0 == 0), so this gate converts the same binary to an
# arm64 Image and boots THAT. The header is in the source; objcopy makes the flat image the firmware protocol expects.
IMG="$WORK/morph-core.img"
# THE IMAGE IS EXTRACTED FROM THE ELF BY HAND. Measured twice: host objcopy cannot read an aarch64 ELF ("Unable to
# recognise the format"), and this node has no llvm-objcopy. Parsing the program headers is a dozen lines and needs no
# tool at all, which is the honest answer for a machine that may be as bare as the one this Core will run on.
python3 - "$ELF" "$IMG" <<'PATCH'
import struct, sys
elf_path, img_path = sys.argv[1], sys.argv[2]
data = open(elf_path, 'rb').read()
if data[:4] != b'\x7fELF' or data[4] != 2:
    sys.exit("not a 64-bit ELF")
machine = struct.unpack_from('<H', data, 18)[0]
if machine != 183:
    sys.exit(f"not aarch64 (e_machine={machine})")
phoff = struct.unpack_from('<Q', data, 32)[0]
phentsize = struct.unpack_from('<H', data, 54)[0]
phnum = struct.unpack_from('<H', data, 56)[0]
segments = []
for i in range(phnum):
    off = phoff + i * phentsize
    p_type, = struct.unpack_from('<I', data, off)
    p_offset, p_vaddr, p_paddr, p_filesz = struct.unpack_from('<QQQQ', data, off + 8)
    if p_type == 1 and p_filesz:          # PT_LOAD
        segments.append((p_vaddr, p_offset, p_filesz))
if not segments:
    sys.exit("no loadable segments")
base = min(v for v, _, _ in segments)
end = max(v + n for v, _, n in segments)
image = bytearray(end - base)
for vaddr, offset, filesz in segments:
    image[vaddr - base : vaddr - base + filesz] = data[offset : offset + filesz]
# image_size is a field the boot protocol reads, where zero is not a valid Image: patched from the length.
magic = struct.unpack_from('<I', image, 56)[0]
if magic != 0x644d5241:
    sys.exit("the image does not carry the ARM\\x64 marker")
struct.pack_into('<Q', image, 16, len(image))
open(img_path, 'wb').write(bytes(image))
print(f"image {len(image)} bytes, base 0x{base:x}")
PATCH
[ -s "$IMG" ] || { echo "FAIL: the Image was not written" >&2; exit 1; }

timeout "${FDT_TIMEOUT:-25}" qemu-system-aarch64 -M virt -cpu cortex-a76 -m 256M \
    -kernel "$IMG" -nographic -monitor none -serial stdio \
    -netdev user,id=n0 -device virtio-net-device,netdev=n0 \
    -device virtio-blk-device,drive=d0 -drive file="$WORK/disk.img",if=none,id=d0,format=raw \
    -global virtio-mmio.force-legacy=false \
    -object filter-dump,id=f0,netdev=n0,file="$WORK/net.pcap" > "$LOG" 2>&1
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
# STORAGE. The block device's capacity was read, and sector 0 must round-trip the signature the gate wrote: the Core
# reads bytes it did not write, which is the difference between a driver and a printed constant.
grep -qF "4d 4f 52 50 48 45 4f 53" "$LOG" || { echo "FAIL: sector 0 did not round-trip the MORPHEOS signature" >&2; exit 1; }
echo "ok: the Core read sector 0 and got back the signature the disk was written with"
# A write is only proven by the read that follows it: the Core wrote CORE-WR1 to sector 1 and read it back.
grep -qF "BLK: wrote and read back sector 1: CORE-WR1" "$LOG" || { echo "FAIL: the sector write did not round-trip" >&2; exit 1; }
echo "ok: the Core wrote a sector and read it back"
# LOADER MILESTONE 2: code read off the disk, copied into executable memory, and executed — the result must be 66.
grep -qF "LOAD: code loaded from disk returned 66" "$LOG" || { echo "FAIL: code from the disk did not execute" >&2; exit 1; }
echo "ok: the Core loaded foreign code off the disk and executed it"
# ELF: the header off the disk must be a verified 64-bit little-endian AArch64 ELF before the entry point is believed.
grep -qF "ELF: AArch64 header parsed" "$LOG" || { echo "FAIL: the ELF header was not parsed" >&2; exit 1; }
echo "ok: the Core parsed a real ELF header off its disk"
# ELF LOAD: a full ELF off the disk, its PT_LOAD segment placed, its entry called — and it returns 66.
grep -qF "ELF: loaded a compiled program" "$LOG" || { echo "FAIL: the compiled program did not run" >&2; exit 1; }
grep -qF "it returned 66" "$LOG" || { echo "FAIL: the compiled program returned the wrong value" >&2; exit 1; }
echo "ok: a compiled ELF program was placed in memory and executed, returning 66"
# THE API SURFACE: a loaded program traps into the Core via svc and asks for a service; the result must round-trip.
grep -qF "SVC: the loaded program asked the Core for service" "$LOG" || { echo "FAIL: the loaded program's syscall did not reach the Core" >&2; exit 1; }
grep -qF "it returned 66" "$LOG" || { echo "FAIL: the syscall round-trip did not return" >&2; exit 1; }
echo "ok: a loaded program called the Core through svc and got its result back"