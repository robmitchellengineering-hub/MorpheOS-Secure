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

# sector 4: a full minimal ELF the loader places and calls
e = bytearray(128)
e[0:4] = b"\x7fELF"
e[4] = 2
e[5] = 1
e[6] = 1
struct.pack_into("<H", e, 0x10, 2)     # ET_EXEC
struct.pack_into("<H", e, 0x12, 183)   # EM_AARCH64
struct.pack_into("<I", e, 0x14, 1)
struct.pack_into("<Q", e, 0x18, 0x40200000)  # e_entry = the code's load address (free RAM, not .text)
struct.pack_into("<Q", e, 0x20, 64)    # e_phoff
struct.pack_into("<H", e, 0x34, 64)    # e_ehsize
struct.pack_into("<H", e, 0x36, 56)    # e_phentsize
struct.pack_into("<H", e, 0x38, 1)     # e_phnum
struct.pack_into("<I", e, 64, 1)       # p_type = PT_LOAD
struct.pack_into("<I", e, 68, 5)       # p_flags = R|X
struct.pack_into("<Q", e, 72, 120)     # p_offset
struct.pack_into("<Q", e, 80, 0x40200000)  # p_vaddr
struct.pack_into("<Q", e, 88, 0x40200000)  # p_paddr
struct.pack_into("<Q", e, 96, 28)      # p_filesz
struct.pack_into("<Q", e, 104, 28)     # p_memsz
struct.pack_into("<Q", e, 112, 0x1000)  # p_align
e[120:148] = bytes([0x00,0x00,0x80,0xD2, 0x01,0x00,0x00,0xD4, 0x20,0x00,0x80,0xD2, 0x41,0x08,0x80,0xD2, 0x01,0x00,0x00,0xD4, 0x40,0x08,0x80,0xD2, 0xC0,0x03,0x5F,0xD6])
disk[2048:2048 + 128] = e

open(path, "wb").write(bytes(disk))
print("disk built:", len(disk), "bytes")
