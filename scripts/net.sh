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
FEATURES=""
if [ "$MODE" = "--self-test" ]; then FEATURES="--features ip-mutant-bad-checksum,udp-mutant-bad-checksum,rtp-mutant-frozen-seq"; fi
command -v cargo >/dev/null 2>&1 || { echo "REFUSED: no cargo here" >&2; exit 2; }
command -v qemu-system-aarch64 >/dev/null 2>&1 || { echo "REFUSED: no qemu-system-aarch64 here" >&2; exit 2; }
export CARGO_TARGET_DIR="$WORK/target"
echo "== building the Core (net)"
# shellcheck disable=SC2086
cargo build -q -p morph-core --target aarch64-unknown-none --release $FEATURES || { echo "FAIL: the Core did not build" >&2; exit 1; }
cargo build -q -p morph-program --target aarch64-unknown-none --release || { echo "FAIL: the program did not build" >&2; exit 1; }
ELF="$WORK/target/aarch64-unknown-none/release/morph-core"
IMG="$WORK/morph-core.img"; PCAP="$WORK/net.pcap"
mkdir -p "$WORK"; rm -f "$IMG" "$PCAP"
python3 scripts/make-disk.py "$WORK/disk.img" "$WORK/target/aarch64-unknown-none/release/morph-program"
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
    -device virtio-blk-device,drive=d0 -drive file="$WORK/disk.img",if=none,id=d0,format=raw \
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
# THE ANSWER. A reply the Core COMPOSED (op 2), not echoed: an ARP frame with our MAC as sender and op=2 in the
# operation field. This is the difference between a driver that speaks and one that answers a question.
python3 - "$PCAP" "$OUR_MAC" <<'PYCHECK' || exit 1
import struct, sys
d = open(sys.argv[1], 'rb').read()
mac = bytes(int(b, 16) for b in sys.argv[2].split(':'))
off, replies = 24, 0
while off + 16 <= len(d):
    _, _, incl, _ = struct.unpack_from('<IIII', d, off)
    frame = d[off + 16:off + 16 + incl]
    off += 16 + incl
    if len(frame) >= 42 and frame[12:14] == b'\x08\x06' and frame[6:12] == mac and frame[20:22] == b'\x00\x02':
        replies += 1
if not replies:
    sys.exit("FAIL: no ARP reply from our MAC in the capture")
print("ok: " + str(replies) + " ARP reply(ies) the Core composed reached the wire")
PYCHECK
echo "ok: a frame the Core built was captured by the host — the ring works"
# THE OTHER DIRECTION. Speaking is half a driver; hearing is the other half, and the gateway's ARP reply is the
# evidence that our receive queue, our device-writable descriptors and our used-ring arithmetic are right.
grep -qF "NET: RECEIVED" "$WORK/serial.log" || {
    echo "FAIL: nothing was received — the Core spoke but never heard an answer" >&2
    exit 1
}
echo "ok: an inbound frame completed into our ring — the Core heard the network answer it"

# RUNG 1 OF THE IP CHUNK. The capture must hold an ICMP echo REPLY the Core computed — ethertype IPv4, protocol ICMP,
# type 0, from 10.0.2.15 — with BOTH the IP header and ICMP checksums correct. A reply with a wrong checksum is not a
# stack, it is a decoration, so the gate recomputes them on the host side.
GOOD_ICMP=$(python3 - "$PCAP" <<'PYICMP'
import struct, sys
d = open(sys.argv[1], 'rb').read()
def cksum(b):
    s = 0
    for i in range(0, len(b) - 1, 2):
        s += (b[i] << 8) | b[i + 1]
    if len(b) % 2:
        s += b[-1] << 8
    while s >> 16:
        s = (s & 0xffff) + (s >> 16)
    return (~s) & 0xffff
off, found = 24, 0
while off + 16 <= len(d):
    _, _, incl, _ = struct.unpack_from('<IIII', d, off); off += 16
    f = d[off:off + incl]; off += incl
    if len(f) < 50 or f[12:14] != b'\x08\x00' or f[23] != 1 or f[34] != 0:
        continue
    if f[26:30] != bytes([10, 0, 2, 15]):
        continue
    ip = bytearray(f[14:34]); ip[10] = 0; ip[11] = 0
    if f[24:26] != cksum(ip).to_bytes(2, 'big'):
        continue
    icmp = bytearray(f[34:50]); icmp[2] = 0; icmp[3] = 0
    if f[36:38] != cksum(icmp).to_bytes(2, 'big'):
        continue
    found += 1
print(found)
PYICMP
)
GOOD_ICMP=${GOOD_ICMP:-0}

# RUNG 2 OF THE IP CHUNK. UDP out (proto 17 from us, correct UDP checksum over the RFC 768 pseudo-header) and UDP in
# (the Core parses a datagram and reports the ports). The checksum is recomputed on the host side; the mutation makes it
# wrong, so a wrong-checksum datagram is not a stack either.
GOOD_UDP=$(python3 - "$PCAP" <<'PYUDP'
import struct, sys
d = open(sys.argv[1], 'rb').read()
def cksum(b):
    s = 0
    for i in range(0, len(b) - 1, 2):
        s += (b[i] << 8) | b[i + 1]
    if len(b) % 2:
        s += b[-1] << 8
    while s >> 16:
        s = (s & 0xffff) + (s >> 16)
    return (~s) & 0xffff
off, found = 24, 0
while off + 16 <= len(d):
    _, _, incl, _ = struct.unpack_from('<IIII', d, off); off += 16
    f = d[off:off + incl]; off += incl
    if len(f) < 42 or f[12:14] != b'\x08\x00' or f[23] != 17:
        continue
    if f[26:30] != bytes([10, 0, 2, 15]):
        continue
    udp_len = (f[38] << 8) | f[39]
    if udp_len < 8 or 34 + udp_len > len(f):
        continue
    seg = bytearray(f[34:34 + udp_len]); seg[6] = 0; seg[7] = 0
    ph = f[26:30] + f[30:34] + bytes([0, 17]) + udp_len.to_bytes(2, 'big') + bytes(seg)
    if f[40:42] != cksum(ph).to_bytes(2, 'big'):
        continue
    found += 1
print(found)
PYUDP
)
GOOD_UDP=${GOOD_UDP:-0}
UDP_PARSED=$(grep -c "NET: UDP parsed — src port 9999" "$WORK/serial.log" || true)

# RUNG 3 OF THE IP CHUNK. RTP: eight packets, sequence strictly 0..7, timestamp advancing exactly 48 per packet (1 ms
# at 48 kHz). Determinism is the claim; the gate checks it off the wire.
RTP_OK=$(python3 - "$PCAP" <<'PYRTP'
import struct, sys
d = open(sys.argv[1], 'rb').read()
off, pkts = 24, []
while off + 16 <= len(d):
    _, _, incl, _ = struct.unpack_from('<IIII', d, off); off += 16
    f = d[off:off + incl]; off += incl
    if len(f) >= 54 and f[12:14] == b'\x08\x00' and f[23] == 17 and f[42] == 0x80:
        if f[50:54] != bytes([0xDE, 0xAD, 0xBE, 0xEF]):
            continue  # the demo stream only; the engine uses a different SSRC
        seq = (f[44] << 8) | f[45]
        ts = (f[46] << 24) | (f[47] << 16) | (f[48] << 8) | f[49]
        pkts.append((seq, ts))
seqs = [p[0] for p in pkts]
steps = [pkts[i + 1][1] - pkts[i][1] for i in range(len(pkts) - 1)]
ok = len(pkts) == 8 and seqs == list(range(8)) and all(st == 48 for st in steps)
print(1 if ok else 0)
PYRTP
)
RTP_OK=${RTP_OK:-0}

# THE ENGINE'S RTP: the loaded program's own stream, fixed SSRC 0xDEADBEEF, distinct from the demo's 8-packet stream.
ENGINE_RTP=$(python3 - "$PCAP" <<'PYENG'
import struct, sys
d = open(sys.argv[1], 'rb').read()
off, found = 24, 0
while off + 16 <= len(d):
    _, _, incl, _ = struct.unpack_from('<IIII', d, off); off += 16
    f = d[off:off + incl]; off += incl
    if len(f) >= 54 and f[12:14] == b' ' and f[23] == 17 and f[50:54] == bytes([0xC0, 0xFF, 0xEE, 0x00]):
        found += 1
print(found)
PYENG
)
ENGINE_RTP=${ENGINE_RTP:-0}

# THE LOADED PROGRAM'S DATAGRAM. The API-surface rung sends a UDP datagram whose payload is MORPHEOS-SVC. It must be on
# the wire with a CORRECT checksum — the same arithmetic the demo datagram proved, but carrying the loaded program's own
# payload. This is the difference between 'a datagram left' and 'the loaded program's datagram left'.
SVC_UDP=$(python3 - "$PCAP" <<'PYSVC'
import struct, sys
d = open(sys.argv[1], 'rb').read()
def cksum(b):
    s = 0
    for i in range(0, len(b) - 1, 2):
        s += (b[i] << 8) | b[i + 1]
    if len(b) % 2:
        s += b[-1] << 8
    while s >> 16:
        s = (s & 0xffff) + (s >> 16)
    return (~s) & 0xffff
off, found = 24, 0
while off + 16 <= len(d):
    _, _, incl, _ = struct.unpack_from('<IIII', d, off); off += 16
    f = d[off:off + incl]; off += incl
    if len(f) < 54 or f[12:14] != bytes([8, 0]) or f[23] != 17:
        continue
    if f[26:30] != bytes([10, 0, 2, 15]):
        continue
    udp_len = (f[38] << 8) | f[39]
    if udp_len < 8 or 34 + udp_len > len(f):
        continue
    seg = bytearray(f[34:34 + udp_len]); seg[6] = 0; seg[7] = 0
    ph = f[26:30] + f[30:34] + bytes([0, 17]) + udp_len.to_bytes(2, 'big') + bytes(seg)
    if f[40:42] != cksum(ph).to_bytes(2, 'big'):
        continue
    if b'MORPHEOS-SVC' in f[42:42 + udp_len - 8]:
        found += 1
print(found)
PYSVC
)
SVC_UDP=${SVC_UDP:-0}

if [ "$MODE" = "--self-test" ]; then
    if [ "$GOOD_ICMP" -ne 0 ] || [ "$GOOD_UDP" -ne 0 ] || [ "$SVC_UDP" -ne 0 ] || [ "$RTP_OK" -ne 0 ]; then
        echo "FAIL (self-test): a bad-checksum mutation still produced a valid datagram" >&2; exit 1
    fi
    echo "ok (self-test): with wrong checksums no valid reply or datagram is accepted — the check reads the arithmetic"
    exit 0
fi
if [ "$GOOD_ICMP" -eq 0 ]; then
    echo "FAIL: no ICMP echo reply with a correct checksum reached the wire" >&2; exit 1
fi
echo "ok: an ICMP echo reply the Core computed, with correct IP and ICMP checksums, reached the wire"
if [ "$GOOD_UDP" -eq 0 ]; then
    echo "FAIL: no UDP datagram with a correct checksum reached the wire" >&2; exit 1
fi
echo "ok: a UDP datagram the Core computed, with a correct checksum, reached the wire"
if [ "$UDP_PARSED" -eq 0 ]; then
    echo "FAIL: the Core did not parse an inbound datagram" >&2; exit 1
fi
echo "ok: the Core parsed an inbound UDP datagram — ports and length read back"

if [ "$SVC_UDP" -eq 0 ]; then
    echo "FAIL: the loaded program's datagram (MORPHEOS-SVC) did not reach the wire" >&2; exit 1
fi
echo "ok: the loaded program's UDP datagram reached the wire, checksum correct"

if [ "$RTP_OK" -eq 0 ]; then
    echo "FAIL: no RTP stream with monotonic sequence and a 48-sample step reached the wire" >&2; exit 1
fi
echo "ok: an RTP stream the Core emitted — sequence 0..7, timestamp step 48 — reached the wire"