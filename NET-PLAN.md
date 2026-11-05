# The network a show rides on — plan before code

A show is audio packets on a clock. AirMatrix's engine sends RTP over UDP; the control surface talks to it too. This
plan is the order in which the Core earns that, each step falsifiable on its own, and it starts from what the Core
already has — a clock, a tick, two contexts, per-task maps, priorities, and a ledger that refuses what it cannot prove.

## What is already true, and what it buys
- **`clock` + `tick`**: the Core knows when it is. A 1 ms cadence is measured (1.002 ms), and the timer interrupts it.
- **`prio`**: the audio class takes every pick while it is ready. That is the property a jitter-sensitive path needs.
- **`cert`**: a generation cannot become history unless `Σ(Cᵢ/Tᵢ) ≤ 1` holds — so the *show's* schedule is arithmetic.
- **`box`**: a foreign component is validated once at load, not masked per access.
- **`ab`**: a bad update falls back instead of bricking the machine in a venue.

## The order, each with its falsifier

1. **Virtio-net, one device, no IP.** Discover the virtio-mmio window QEMU's DTB describes (the reader already finds
   nodes by `compatible`), bring the device up, and transmit one frame the host also sees.
   *Falsifier*: the host side (`-netdev socket`/pcap) records a frame whose bytes we state in advance; a mutation that
   skips ring kick-off produces nothing.
2. **ARP and a reply.** Answer for our own address.
   *Falsifier*: the host's `ping` (ICMP echo would be step 3, so here: an ARP request gets an ARP reply in the pcap).
3. **IPv4 + ICMP echo.** The smallest end-to-end proof that the stack is not lying.
   *Falsifier*: `ping` from the host, answered; a bad checksum mutation is dropped (no reply) and the gate reads that.
4. **UDP.** Ports, lengths, checksum. A single datagram in each direction.
   *Falsifier*: a host-side `nc -u` echo whose payload the Core prints; a mutation that ignores the length reads past
   the datagram and the check dies.
5. **RTP.** Sequence numbers, timestamps in the Core's own clock, a fixed 48 kHz × 2-channel 24-bit packet layout.
   *Falsifier*: `ffmpeg`/`gst` receive and decode the stream, or `tcpdump` shows monotonic sequence and timestamp
   deltas within a stated bound — the bound is the measurement, not a claim.
6. **Ledger-backed state.** The show's configuration (routes, gains, which engine is live) lives in the ledger, so a
   change is a generation: undo without a reboot, and a certificate before it applies.
   *Falsifier*: change a gain, undo it, and the audio returns to the previous gain; offer a generation whose
   certificate fails and the head does not move.
7. **The control API.** A small request/response on UDP: `status`, `set gain`, `who is live`. It reports from the ledger
   so a control surface cannot see a state the audio path is not in.
   *Falsifier*: `status` before and after a committed generation differs in exactly the field that changed.
8. **Conductor-mode access points.** These are the *network* side of "one engine is a show": the APs the desk talks to.
   They are listed here as the step after the engine speaks, and their protocol is the conductor's, not this document's.

## What "never drops" needs on top of all of it
The two engines are live backup, so the property is not throughput: it is **detection and takeover inside the buffer
the APs hold**. That is a certificate across two machines, and it comes after step 7 — it needs the control API to say
who is live and the ledger to make the switch a generation, not a guess.

## What this plan deliberately does not do
- **No TCP.** A show's audio is UDP; TCP's retransmit is the wrong repair for a late packet. TCP can come later for
  other reasons, not for this one.
- **No DHCP or DNS.** Addresses are stated in the ledger.
- **No TLS.** Integrity and authority are the capability and certificate machinery, not a handshake bolted on.
- **No IPv6 until the show works.** It doubles the code and adds nothing to a wired desk.

## MEASURED STATE, 2026-11-04 — step 1 is discovery, and it corrected two assumptions

The Core now asks the firmware where the network is, and the answers were not what the plan assumed:

```
NET: a virtio-mmio window at 0xa000000 is described by the firmware
NET: header magic 0x74726976  version 1  device id 0
NET: the window is reachable but is not a modern virtio-net
```

Three facts, each earned in one boot:

1. **The window is readable.** The page tables already reach `0xa000000`, so no MMU work is needed before a driver —
   the first thing the plan would otherwise have got wrong.
2. **It is the LEGACY interface (version 1), not modern (version 2).** The register layout differs and
   `GuestPageSize` is part of the bring-up. Code written against version 2 would have been wrong in a way that fails
   quietly.
3. **The first window is an EMPTY SLOT** (`device id 0`). The tree's first `virtio,mmio` node holds nothing, so the
   driver's first job is **enumeration** — pick the window whose `device id` is 1 (net) — and not setup.

**Also open:** an `EXCEPTION` follows that boot, after the probe, and it is not yet attributed. The Core reported it
rather than stopping (which is the fault rung working), but no driver code should be written until it is understood —
a fault during device probing is exactly the kind of thing that turns into a mystery three rungs later.

### What this changes in the order above
- **Step 1 gains a first half:** teach the tree walker (or a bounded scan) to find *every* `virtio-mmio` window and
  choose by device id. The base comes from the tree; on QEMU the stride between windows is `0x200`.
- **The register constants must be the legacy ones**, and the plan's step 1 should state that explicitly rather than
  assuming "modern, version 2" as it originally did.
- **The `EXCEPTION` is attributed before the ring is touched.**

### The editorial lesson, recorded because it cost a round
An edit keyed on exact surrounding text failed because a previous insertion had changed that text's indentation, and
the script aborted before writing — the same class of failure as the line-range move that swallowed five demos. In a
file that edits have reshaped, **anchor on a line verified in this session, or rewrite the whole block.**

## DRIVER STATE, 2026-11-04 — the device is up, the wire is still silent

A virtio-net driver now exists and brings the device up. It does **not** yet send a frame, and the plan must say so.

### Measured, in order
```
NET: a virtio-mmio window at 0xa000000 is described by the firmware
NET: window 31 at 0xa003e00 — version 1, device 1
NET: the network device is at 0xa003e00 — chosen by its own device id
NET: features offered 0x39bf8064, status now 0xf
NET: the device's MAC is 52:54:00:12:34:56
NET: the queue's rings are in our own memory, addressed by page frame
the device never completed the descriptor
```

So: the transport is **legacy (version 1)**, the device is at **slot 31** (not the first window the tree names),
the status handshake reaches **`DRIVER_OK`**, and the **MAC is read from the device's own config space** — a value only a
real device can supply. Then the descriptor ring is offered and **never completed**, and the host-side capture holds a
pcap header and nothing else: **no frame left the machine.**

### Facts that are clean, so do not re-check them
| fact | reading |
|---|---|
| status after the kick | `0xf`, no `DEVICE_NEEDS_RESET` (bit 6) — the device has not rejected the queue |
| `QueuePFN` readback | `0x4009c` — our rings' page frame landed, inside RAM |
| magic / version / device id | `"virt"` / legacy / id 1 |
| features | `0x39bf8064`, one bit accepted |

### The open question, stated so it can be settled by observation
`QueueNum` readback is **0** after writing 8, **with `QueueSel = 0` written first**. Two possibilities: the register is
write-only in this implementation, or the write path to the queue is not what the driver believes. Selecting the queue
explicitly changed nothing, so the next step is **comparison, not another hypothesis**: run a stock legacy driver
(Linux's `virtio_mmio`) against the same QEMU invocation and dump the register sequence it actually performs while a
frame goes out. Six measurements have been made; the seventh should be a known-good trace.

### What is deliberately not in this driver yet
No receive path, no protocol stack, no checksums, no retries. The milestone is one frame the host's pcap contains —
nothing above it is worth writing until that is true.
