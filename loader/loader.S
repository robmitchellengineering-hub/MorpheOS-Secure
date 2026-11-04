/* morph-loader — the first thing the machine runs.
 *
 * WHY THIS IS ASSEMBLY AND A SEPARATE ELF. A 64-bit Core cannot be handed to a loader that only knows 32-bit:
 * measured, QEMU answers "Cannot load x86-64 image, give a 32bit one". So the loader is a 32-bit image, and the
 * Core travels inside it as a blob. The loader's job is exactly three things, and they are the three things that
 * have to be *ours* for the A/B rule to mean anything:
 *
 *   1. find the generation it is about to run (M0b-1: the one embedded here; M0b-2: the one on disk);
 *   2. place it where it expects to be;
 *   3. hand the CPU over in long mode, with its own GDT and its own page tables.
 *
 * Nothing here comes from a distribution, a firmware vendor or a boot manager.
 */

    .intel_syntax noprefix

    /* The multiboot header: the only contract with whatever put this file in memory. */
    .section .multiboot, "a"
    .align 4
    .long 0x1BADB002
    .long 0x00000000
    .long -(0x1BADB002 + 0)

    .section .text
    .code32
    .global _start
_start:
    cli

    /* Place the Core at the address its own linker script gave it (2 MiB). The blob is part of this image, so on
     * M0b-1 "loading" is a copy; on M0b-2 this becomes a read of the chosen generation from disk, and the two
     * generations are two blobs in the same structure. */
    mov esi, offset _core_start
    mov edi, CORE_LOAD_ADDR
    mov ecx, _core_end - _core_start
    rep movsb

    /* Our GDT. The loader's is not ours to rely on and after the far jump below it is not in the path. */
    lgdt [gdt_desc]

    /* Identity-map the first 8 MiB with 2 MiB pages: enough for the loader, the Core, its stack and the record
     * area. Nothing above it is mapped: a Core that maps memory it never asked for lies about what it owns. */
    mov edi, offset pd
    mov eax, 0x83
    mov ecx, 4
1:  mov [edi], eax
    add eax, 0x200000
    add edi, 8
    loop 1b

    mov dword ptr [pdpt], offset pd
    or  dword ptr [pdpt], 3
    mov dword ptr [pdpt + 4], 0
    mov dword ptr [pml4], offset pdpt
    or  dword ptr [pml4], 3
    mov dword ptr [pml4 + 4], 0

    /* PAE, then LME, then paging — the order the CPU requires. */
    mov eax, cr4
    or  eax, 0x20
    mov cr4, eax
    mov eax, offset pml4
    mov cr3, eax
    mov ecx, 0xC0000080
    rdmsr
    or  eax, 0x100
    wrmsr
    mov eax, cr0
    or  eax, 0x80000000
    mov cr0, eax

    /* Into 64-bit code. Encoded by hand: the mnemonic for a far jump differs between syntaxes and this must not be
     * a guess. */
    .byte 0xEA
    .long long_mode
    .word 0x08

    .code64
long_mode:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    /* Hand the CPU to the Core at the address it was linked for. The Core's first instruction is at the first byte
     * of the blob, which is why its linker script puts `.text.entry` there. */
    mov rax, CORE_LOAD_ADDR
    jmp rax

2:  hlt
    jmp 2b

    .section .rodata
    .align 8
gdt:
    .quad 0
    .quad 0x00AF9A000000FFFF      /* 64-bit code, present, DPL0, readable */
    .quad 0x00CF92000000FFFF      /* flat data, present, writable */
gdt_end:
gdt_desc:
    .word gdt_end - gdt - 1
    .long gdt                     /* 32-bit lgdt: 2-byte limit + 4-byte base */

    /* The Core, embedded. `as -I target/m0` finds it, and `scripts/m0.sh` builds it first. */
    .section .core, "a"
    .align 16
    .global _core_start
_core_start:
    .incbin "core.bin"
_core_end:

    .section .bss
    .align 4096
pml4:  .skip 4096
pdpt:  .skip 4096
pd:    .skip 4096
