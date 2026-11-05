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
| 2 | `yield` | none | new-frame pointer | **next** — must mirror `irq_entry`: return the frame `sched::on_irq` chooses, and let `fault_entry` switch to it |
| 3 | `send_udp` | `x1` = payload ptr, `x2` = len | bytes sent | **next** — needs the net device base + the ARP-learned gateway MAC threaded out of `fdt_demo` into statics |

## The two hard parts of the next two, stated

- **`yield`** is a *frame switch*, not a value return. The SVC path must become like the IRQ path: `svc_handler` returns
  the next frame (`sched::on_irq(sp)`), `fault_entry` does `mov sp, x0` and `RESTORE_ALL_WITH_STATE`. The comment at
  `main.rs` "For the INTERRUPT path only" is exactly this distinction, and it will need revisiting for SVC — the SVC path
  may use `RESTORE_ALL_WITH_STATE` because it does not rewrite ELR.
- **`send_udp`** needs the device and the gateway MAC that `fdt_demo` already learned via ARP. Store them in statics
  (Rust 2024: use `AtomicU64`/`UnsafeCell`, not `static mut`) during the network demo, then the handler composes
  `udp::datagram(...)` and calls `net::transmit(...)`. The falsifier must be the **capture** (`-object filter-dump`),
  not the console — the network gate rule applies here too.

## Why the result is observable, not assumed

Each syscall's gate reads a *consequence*: `report` prints a line the gate greps; `apply_certified` prints the generation
id and the program returns 66. A mutation that returns a different value fails the gate — the check reads execution, not
the source.
