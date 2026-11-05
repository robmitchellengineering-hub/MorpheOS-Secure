# The loader — the rung that makes the Core host code instead of just containing it

Storage is done (capacity, read, write). The next rung joins it to the box rung (M7d): a binary off the disk,
validated once, jumped to — and it runs.

## The milestones, each falsifiable

1. **Jump to loaded code.** A small aarch64 function (position-independent machine code, a handful of bytes) lives in
   a buffer the Core marks executable, and the Core `blr`s to it. The function writes a marker to the UART and returns.
   *Falsifier*: the marker appears on the serial line; a mutation that jumps to the wrong address faults and the Core
   reports it rather than halting.

2. **From the disk.** The gate writes that same function's bytes to the disk; the Core reads the sector, copies the
   bytes into an executable buffer, validates, and jumps.
   *Falsifier*: the bytes round-trip through the disk AND execute — two facts, one boot.

3. **Box validation at load (M7d, joined).** Before the jump, the loaded bytes are checked against the box's declared
   limits (the `box_admission` rung already has the shape). A binary that claims to run outside its box is refused at
   load, not after it faults.
   *Falsifier*: a lying box is refused and named; the mutation `box-mutant-trust-claim` admits it and the gate goes red.

4. **A real program, later.** An ELF header the Core can parse, a section it can place, and a symbol it can call — the
   step that turns "jump to bytes" into "run a program". This is where AirMatrix's engine eventually lands.

## The two things that are genuinely hard, stated up front
- **Executable memory.** The loaded code must be in a region the MMU maps as executable. The kernel's own image is; a
  `.bss` buffer may not be. Either mark a region executable, or load into the existing executable window — measured, not
  assumed.
- **Position independence.** The jumped-to code cannot rely on absolute addresses unless the loader patches them or the
  code is compiled position-independent. For the first milestone this means hand-written or `-fPIC` bytes.

## The honest order, given the session that built up to here
Do milestone 1 **without** the disk first (jump to a static byte buffer), because it isolates the executable-memory
question from the disk question. Then milestone 2 joins the already-proven disk read. Then milestone 3 joins the
already-proven box admission. Each rung is one fact, and none of them is "it seems to run."
