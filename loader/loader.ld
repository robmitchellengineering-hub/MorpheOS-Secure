/* morph-loader's layout. 32-bit ELF, loaded at 64 KiB, multiboot header first in the file because that is the only
 * place a loader looks for it. */

ENTRY(_start)
CORE_LOAD_ADDR = 0x200000;

SECTIONS
{
    . = 0x10000;

    .multiboot : ALIGN(4)
    {
        KEEP(*(.multiboot))
    }

    .text : ALIGN(16)
    {
        *(.text .text.*)
    }

    .rodata : ALIGN(16)
    {
        *(.rodata .rodata.*)
    }

    /* The Core travels here as bytes, not as code: the loader does not execute it and does not understand it. */
    .core : ALIGN(16)
    {
        _core_start = .;
        *(.core)
        _core_end = .;
    }

    .bss : ALIGN(4096)
    {
        *(.bss .bss.*)
    }

    /DISCARD/ :
    {
        *(.eh_frame) *(.comment) *(.note .note.*)
    }
}
