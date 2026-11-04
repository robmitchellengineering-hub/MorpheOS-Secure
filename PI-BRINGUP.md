# M0b — the Raspberry Pi 5: plan for the day the hardware arrives

Needs: a Pi 5, power, and a serial console (UART header or a USB-serial adapter). Everything below is ordered so that
each step is small, separately falsifiable, and does not depend on a later one.

## THE RULE FOR THIS RUNG
**The machine's own device tree is the source of truth, not this document.** QEMU `virt` and the Pi are different
boards, and the honest reason to distrust hard-coded addresses is that this project already paid for it: the GIC was
GICv3 in my memory and GICv2 in the machine's DTB, six rounds to find. So step 1 is *read the DTB*, and every address
below is a hypothesis to confirm or replace.

| what | QEMU `virt` (proven) | Pi 5 (unconfirmed until measured) |
|---|---|---|
| UART | PL011 at `0x0900_0000` | the RP1's PL011 — the constant exists in `uart.rs` as `UART_PI5` and is **a parameter, not a fact**, until the DTB says otherwise |
| code load address | `0x4008_0000` | the arm64 image loads at the low 1 MiB region; the linker script's `0x4008_0000` must become a variable passed in |
| interrupt controller | GICv2 (`arm,gic-v2m-frame`) | to be read from the DTB |
| timer | `CNTPCT_EL0`/`CNTFRQ_EL0` | **the same** — architectural, which is why the clock rung transfers unchanged |

## The steps, each with its falsifier

1. **Firmware hand-off, understood before written.** Boot the Pi with a known-good arm64 image (Raspberry Pi OS) and
   its serial console, and capture: the firmware's console output, the DTB the firmware passes in `x0`, and the UART
   the firmware speaks on. *Falsifier*: the boot's own log on a serial line, saved in `runs/` with its exact bytes.
2. **The Core prints one line.** Core with the UART address the DTB gives, entered where the firmware puts it. If the
   Pi's firmware does not pass through a raw image in the same way, this step is **UEFI (EDK2) instead** and the entry
   becomes a PE32+ application; that is a bigger step and the plan says so rather than pretending it is small.
   *Falsifier*: `MORPHEOS-CORE 0.0.1: the Core is running on aarch64` on the Pi's serial, from a file we built.
3. **Our MMU on the Pi's memory map.** The Pi's RAM base and size come from the DTB; the map in `mmu.rs` is built from
   those numbers instead of QEMU's. *Falsifier*: the guard page still faults, and the Core survives it — the same
   `scripts/mmu.sh` check, on hardware.
4. **The tick.** The interrupt controller from the DTB, the same architectural timer. *Falsifier*: three ticks arrive
   on their own, the `scripts/tick.sh` claim, on hardware.
5. **Two tasks.** The switch and per-task tables, unchanged. *Falsifier*: `scripts/preempt.sh`'s claim, on hardware.
6. **Per-core bring-up.** Secondary cores released, each with its own stack, one of them counting ticks. *Falsifier*:
   a line per core, from the core that wrote it.
7. **M0c, the boot path's A/B.** Two generations on the SD card, one-shot trial, commit on the health signal, fallback
   and a record that survives the failure. *Falsifier*: corrupt the newer generation; the machine still comes up, and
   the record says what it tried.

## What can be done before the hardware is here
- Steps 2–5 are **already written and green on QEMU**; what they need is the address parameters made explicit, which
  is a refactor of `mmu.rs` and `uart.rs` rather than new code.
- The DTB reader is the one genuinely new piece: a small parser for the firmware's device tree that answers "where is
  the UART, where is RAM, which interrupt controller". It can be written and tested on QEMU's DTB — which is a real
  device tree — before the Pi exists.

## What Rob needs to do, and nothing else
1. Say **which Pi 5** (RAM size), and whether a UART header is soldered or a USB-serial adapter is available.
2. Plug it in, with a serial console, and tell me what the first line says. Everything after that is mine.
