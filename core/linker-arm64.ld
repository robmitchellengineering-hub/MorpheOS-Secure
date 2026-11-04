/* The Core's memory layout for AArch64. QEMU's `virt` machine loads the kernel image at 0x4008_0000 and enters it
 * at EL1 with the MMU off; the Raspberry Pi 5's UEFI will place it elsewhere, and that address becomes a variable
 * on the Pi rung rather than a constant here. */

ENTRY(_start)

SECTIONS
{
    . = 0x40080000;

    /* FIRST BYTES OF THE IMAGE. The AArch64 boot protocol expects the Image header at offset 0, and the Pi's firmware
       will look for exactly that; measured on QEMU, without this the extracted image did not carry the ARM\x64 marker
       where the protocol says it is. */
    .text.head : ALIGN(16)
    {
        KEEP(*(.text.head))
    }

    .text.entry : ALIGN(16)
    {
        KEEP(*(.text.entry))
    }

    .text : ALIGN(4K)
    {
        *(.text .text.*)
    }

    .rodata : ALIGN(4K)
    {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4K)
    {
        *(.data .data.*)
    }

    .bss : ALIGN(4K)
    {
        *(COMMON)
        *(.bss .bss.*)
    }

    .bss.stack : ALIGN(16)
    {
        stack_bottom = .;
        . += 64K;
        stack_top = .;
    }

    /DISCARD/ :
    {
        *(.eh_frame) *(.comment) *(.note .note.*)
    }
}
