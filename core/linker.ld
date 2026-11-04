/* The Core's memory layout. Loaded at 2 MiB by `morph-loader`, which is why the first byte of the image is the
 * Core's entry point: the loader copies the blob to exactly this address and jumps to it, knowing nothing else.
 *
 * Nothing about this comes from a distribution, a firmware vendor or a boot manager. */

ENTRY(_start)

SECTIONS
{
    . = 2M;

    /* First in the image, so that the flat binary begins with the entry point the loader jumps to. */
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

    /* The stack the Core's first instruction switches to. 64 KiB is generous and M0b presses no button on it. */
    .bss.stack : ALIGN(16)
    {
        stack_bottom = .;
        . += 64K;
        stack_top = .;
    }

    /DISCARD/ :
    {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.comment)
        *(.note .note.*)
    }
}
