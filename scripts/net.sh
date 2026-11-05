#!/usr/bin/env bash
# scripts/net.sh — the network milestone: does a frame we built actually reach the wire?
#
# THE CLAIM IS THE PCAP, not the console. A driver that says it transmitted proves nothing; a capture that contains our
# bytes proves the device read our ring, understood it, and put the frame on the wire.
#
# THIS GATE IS DELIBERATELY RED, and it is NOT in scripts/all.sh yet. Measured 2026-11-04: the device reaches DRIVER_OK,
# its MAC is read from config space, the rings are placed — and the descriptor is never completed, so the pcap holds
# only its 24-byte file header. A red gate that names the missing milestone is honest; a green gate that proves nothing
# is the thing this project keeps paying for.
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
MODE=${1:-check}
WORK=${NET_WORK:-target/net}
case "$MODE" in --self-test) WORK="$WORK/selftest" ;; *) WORK="$WORK/check" ;; esac
case "$MODE" in check|--self-test) ;; *) echo "usage: net.sh [--self-test]" >&2; exit 2 ;; esac
command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }
export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (net)"
# shellcheck disable=SC2086
cargo build -q -p morph-core --target aarch64-unknown-none --release || { echo "FAIL: the Core did not build" >&2; exit 1; }
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
IMG="$WORK/morph-core.img"; PCAP="$WORK/net.pcap"
mkdir -p "$WORK"; rm -f "$IMG" "$PCAP"
python3 - "$ELF" "$IMG" <<'PATCH'
import struct, sys
data = open(sys.argv[1], 'rb').read()
phoff = struct.unpack_from('<Q', data, 32)[0]
phentsize = struct.unpack_from('<H', data, 54)[0]
phnum = struct.unpack_from('<H', data, 56)[0]
seg = []
for i in range(phnum):
    off = phoff + i * phentsize
    t, = struct.unpack_from('<I', data, off)
    p_offset, p_vaddr, _, p_filesz = struct.unpack_from('<QQQQ', data, off + 8)
    if t == 1 and p_filesz:
        seg.append((p_vaddr, p_offset, p_filesz))
base = min(v for v, _, _ in seg)
end = max(v + n for v, _, n in seg)
img = bytearray(end - base)
for v, o, n in seg:
    img[v - base:v - base + n] = data[o:o + n]
struct.pack_into('<Q', img, 16, len(img))
open(sys.argv[2], 'wb').write(bytes(img))
PATCH
timeout "${NET_TIMEOUT:-40}" qemu-system-aarch64 -M virt -cpu cortex-a76 -m 256M \
    -kernel "$IMG" -nographic -monitor none -serial stdio \
    -netdev user,id=n0 -device virtio-net-device,netdev=n0 \
    -global virtio-mmio.force-legacy=false \
    -object filter-dump,id=f0,netdev=n0,file="$PCAP" > "$WORK/serial.log" 2>&1

echo "== what the Core wrote (NET lines)"
sed -e 's/\r$//' "$WORK/serial.log" | grep -E "^NET:|never completed" | head -8

OUR_MAC="52:54:00:12:34:56"
if [ ! -s "$PCAP" ]; then
    echo "FAIL: no capture at all — the harness did not record the wire" >&2; exit 1
fi
SIZE=$(stat -c%s "$PCAP")
FRAMES=$(python3 - "$PCAP" <<'PY'
import struct, sys
d = open(sys.argv[1], 'rb').read()
off, n = 24, 0
while off + 16 <= len(d):
    _, _, incl, _ = struct.unpack_from('<IIII', d, off)
    off += 16 + incl
    n += 1
print(n)
PY
)
echo "== capture: $SIZE bytes, $FRAMES frame(s)"
if [ "$FRAMES" -eq 0 ]; then
    echo "FAIL: the capture holds no frames — the device completed nothing, so nothing reached the wire" >&2
    echo "      (this is the current measured state: rings placed, descriptor never completed)" >&2
    exit 1
fi
# Our frame is an ARP request: ethertype 0x0806, sender hardware our MAC.
python3 - "$PCAP" "$OUR_MAC" <<'PY' || exit 1
import struct, sys
d = open(sys.argv[1], 'rb').read()
mac = bytes(int(b, 16) for b in sys.argv[2].split(':'))
off, found = 24, 0
while off + 16 <= len(d):
    _, _, incl, _ = struct.unpack_from('<IIII', d, off)
    frame = d[off + 16:off + 16 + incl]
    off += 16 + incl
    if len(frame) >= 42 and frame[12:14] == b'\x08\x06' and frame[22:28] == mac:
        found += 1
if not found:
    sys.exit("FAIL: no ARP request from our MAC in the capture")
print(f"ok: {found} ARP request(s) from {sys.argv[2]} reached the wire")
PY
echo "ok: a frame the Core built was captured by the host — the ring works"
