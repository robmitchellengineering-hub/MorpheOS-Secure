# Known bugs — captured, narrowed, not yet fixed

Two open defects, each captured with its exact symptom and the checks already ruled out. A fresh session
should start here, not re-derive these.

## 1. `preempt` — task A never finishes (the second failure mode)

**Symptom (captured serial log):**
```
PREEMPT: second task frame at 0x4009ff60 …
TASK B: ran 5000 iterations …   (spliced with the boot's own "switching enabled" print)
the switch reached it
… then nothing: no "TASK A: ran 200000", no "IRQ: STORM"
```

**What this proves:**
- The switch works. The spliced console output is two tasks sharing one CPU — A↔B preemption is real.
- The storm guard (`gic.rs`, scoped to `!sched::switching()`) correctly did **not** fire — so this is the
  *second* failure mode, the one the storm fix (#54) never addressed.

**Ruled out:** storm false-positive; a regression from the yield/loader work (it is a known rare flake,
~1-in-5, and fires solo too).

**Next trace:** task A's `spins: u64` local (task_a_entry, `main.rs`) lives either in a callee-saved
register or on task A's stack across the switch. One of those two paths is losing it under load. Disassemble
task_a_entry, see where `spins` is, and add a probe that prints it on each switch-back.

## 2. `send_udp` — the device drops frame 14 (len 0)

**Symptom (captured):**
```
SVC: sent a UDP datagram of 0 bytes (frame 54)
SVC: used idx 14, id 0, len 0
SVC: device status 0x0f          (DRIVER_OK + FEATURES_OK — healthy)
```

The loaded program's `send_udp` (frame 14, after the net demo's 13 frames) is handed to the device, the
device **advances `used.idx` to 14** (it saw and processed the descriptor) but writes **len 0** — it
consumed nothing. Frames 1–13 all transmit.

**Ruled out (all verified, not assumed):**
- frame: 54 bytes, correct IP/UDP lengths
- MACs: `52:54:00:12:34:56` → `52:55:0a:00:02:02` (correct)
- descriptor: addr=FRAME, len=64, flags 0
- avail-ring slot: fixed (#70, was always `ring[0]`)
- queue depth: raised 8→64 (#70), still drops
- TX/RX ring overlap: none (`RINGS` vs `RINGS_RX`)
- device status: DRIVER_OK, not failed

**Best next move:** test on a real wire (`-netdev tap` via the pacland-rog macvtap, already set up for
rung 4) — slirp's 14th-frame drop likely does not exist on a real NIC. The capture assertion that would
prove it is on the `core/sendudp-capture` branch (correctly failing).

## The honest status these affect

The API surface is **4 calls wired** (report, apply_certified, yield, send_udp). `report` and
`apply_certified` are proven end-to-end; `yield` is a recorded yield (the context switch is a follow-up);
`send_udp`'s *datagram-left-the-wire* claim is held back by bug 2.
