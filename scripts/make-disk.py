#!/usr/bin/env python3
"""Build the disk image the storage and loader rungs read from.

One place for every byte the gates place on the medium, so no nested heredoc has to survive an edit:
  sector 0  : "MORPHEOS" — the read round-trips a signature it did not write
  sector 2  : mov x0,#0x42; ret — loader milestone 2 (code from disk)
  sector 3  : a minimal ELF64 AArch64 header — the ELF header parse
  sector 4  : a full minimal ELF — header + one PT_LOAD + the 8 code bytes (loader milestone 4)
"""
import struct
import sys

path = sys.argv[1]
size = 64 * 1024 * 1024
disk = bytearray(size)

# sector 0: the read round-trip signature
disk[0:8] = b"MORPHEOS"

# sector 2: foreign code the loader jumps to (mov x0,#0x42; ret, little-endian)
disk[1024:1032] = bytes([0x40, 0x08, 0x80, 0xD2, 0xC0, 0x03, 0x5F, 0xD6])

# sector 3: the ELF header the parser verifies
h = bytearray(64)
h[0:4] = b"\x7fELF"
h[4] = 2   # 64-bit
h[5] = 1   # little-endian
h[6] = 1   # ELF version
struct.pack_into("<H", h, 0x10, 2)     # ET_EXEC
struct.pack_into("<H", h, 0x12, 183)   # EM_AARCH64
struct.pack_into("<I", h, 0x14, 1)     # version
struct.pack_into("<Q", h, 0x18, 0x40081000)  # e_entry
disk[1536:1536 + 64] = h

# sector 4-5: a full ELF the loader places and calls — now larger than one sector (a real compiled program would be).
e = bytearray(776)
e[0:4] = b"\x7fELF"
e[4] = 2
e[5] = 1
e[6] = 1
struct.pack_into("<H", e, 0x10, 2)     # ET_EXEC
struct.pack_into("<H", e, 0x12, 183)   # EM_AARCH64
struct.pack_into("<I", e, 0x14, 1)
struct.pack_into("<Q", e, 0x18, 0x40200000)  # e_entry
struct.pack_into("<Q", e, 0x20, 64)    # e_phoff
struct.pack_into("<H", e, 0x34, 64)    # e_ehsize
struct.pack_into("<H", e, 0x36, 56)    # e_phentsize
struct.pack_into("<H", e, 0x38, 2)     # e_phnum = 2 (code + .bss)
def ph(e, i, typ, flags, off, vaddr, filesz, memsz):
    b = 64 + i * 56
    struct.pack_into("<I", e, b, typ)
    struct.pack_into("<I", e, b + 4, flags)
    struct.pack_into("<Q", e, b + 8, off)
    struct.pack_into("<Q", e, b + 16, vaddr)
    struct.pack_into("<Q", e, b + 24, vaddr)
    struct.pack_into("<Q", e, b + 32, filesz)
    struct.pack_into("<Q", e, b + 40, memsz)
    struct.pack_into("<Q", e, b + 48, 0x1000)
ph(e, 0, 1, 5, 176, 0x40200000, 600, 600)   # code: R|X, 600 bytes (135 nops + the syscall tail)
ph(e, 1, 1, 6, 776, 0x40200258, 0, 16)      # .bss: R|W, 16 zero bytes at 0x40200258
nops = bytes([0x1F, 0x20, 0x03, 0xD5]) * 135  # 540 bytes of nop, so the segment spills past one sector
tail = bytes([0x00,0x00,0x80,0xD2, 0x01,0x00,0x00,0xD4, 0x20,0x00,0x80,0xD2, 0x41,0x08,0x80,0xD2, 0x01,0x00,0x00,0xD4, 0x40,0x00,0x80,0xD2, 0x01,0x00,0x00,0xD4, 0x60,0x00,0x80,0xD2, 0x01,0x00,0x00,0xD4, 0x40,0x08,0x80,0xD2, 0x01,0x04,0xA8,0xD2, 0x21,0x60,0x09,0x91, 0x22,0x00,0x40,0xB9, 0x00,0x00,0x02,0x8B, 0xC0,0x03,0x5F,0xD6])
e[176:776] = nops + tail
disk[2048:2048 + 776] = e

open(path, "wb").write(bytes(disk))
print("disk built:", len(disk), "bytes")
