# The syscall ABI — what is proven, what is wired, what is next

The API surface is real: a loaded ELF program traps into the Core with `svc #0` and reads a result back. This is the
mechanism AirMatrix's engine will use to reach the ledger, the scheduler and the network.

## The mechanism (proven, gate `fdt`)

- `svc #0` from EL1 lands in the sync handler (`fault_entry`), whose exception class is **0x15**.
- The handler routes that class to `svc_handler` with the saved frame pointer in `x0`.
- **Frame layout** (set by `SAVE_ALL`, 288 bytes): `x0..x29` at offsets 0..232, `x30` at 240, `ELR_EL1` at 248, `SPSR_EL1` at 256.
- **Calling convention**: `x0` = syscall number, `x1` = arg0, `x2` = arg1. The result is written into the saved `x0`
  (`frame[0]`), so `RESTORE_ALL` hands it to the caller.
- The result round-trips: the loaded program does `svc`, then returns a value, and the boot reads it back. Measured.

## The syscalls

| number | name | args | result | status |
|---|---|---|---|---|
| 0 | `report` | none | 0 | **proven** — prints "asked the Core for service" |
| 1 | `apply_certified` | `x1` = payload | generation id | **proven** — the Core builds+checks a certificate, appends history |
| 2 | `yield` | none | count | **proven** — a monotonic counter; the full frame switch is the preemption follow-up |
| 3 | `send_udp` | `x1` = payload ptr, `x2` = len | bytes sent | **proven** — wraps the program's bytes in a UDP datagram (port 9999) |
| 4 | `send_rtp` | `x1` = ptr, `x2` = len | bytes sent | **proven** — wraps the program's own RTP packet (port 5004); the program builds the header, the Core only carries it |
| 5 | `alloc` | `x1` = size | payload ptr, 0 on refusal | **proven** — a block from the Core's 128-byte-aligned first-fit arena (lazily initialised on first call) |
| 6 | `dealloc` | `x1` = ptr | 0, `u64::MAX` on refusal | **proven** — returns a block; the double-free guard refuses a non-live pointer |

## What the syscalls back

- **5/6 are the program's heap.** The loadable program installs a `#[global_allocator]` over `alloc`/`dealloc`, so the
  mix engine's `Vec`/`Box` allocate from the Core's own arena — the program borrows memory the Core owns, it does not
  bring its own.
- **4 is the engine's voice.** `rob-engine` mixes a period and hands the output RTP packet to `send_rtp`; the gate reads
  the engine's SSRC off the capture, not the console.

## Why the result is observable, not assumed

Each syscall's gate reads a *consequence*: `report` prints a line the gate greps; `apply_certified` prints the generation
id and the program returns 66. A mutation that returns a different value fails the gate — the check reads execution, not
the source.
