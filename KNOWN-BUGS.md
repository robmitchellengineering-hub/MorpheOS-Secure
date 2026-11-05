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

## 2.  — RESOLVED (PR #73)

Three root causes, not one:
1. ** is normal** for virtio-net TX — the device reads the buffer, it does not write, so the used-ring length is
   0 on a successful transmit. It was never the failure signal.
2. **The frame count was the trigger** — the loaded program ran after the RTP's eight packets (frame 14), which the
   device dropped; at frame 6 (before the RTP) the datagram reaches the wire.
3. **The capture check was corrupted** — its  had been written as literal control bytes (a NULL), making
   net.sh a binary file and breaking the ethertype test.

The gate now asserts the datagram's payload (MORPHEOS-SVC) and its checksum off the wire. net and fdt both green.

## The honest status these affect

The API surface is **4 calls wired** (report, apply_certified, yield, send_udp). `report` and
`apply_certified` are proven end-to-end; `yield` is a recorded yield (the context switch is a follow-up);
`send_udp`'s *datagram-left-the-wire* claim is held back by bug 2.
