/* The Core's own memory layout. Nothing about this comes from a distribution: it is a linker script we wrote,
 * and the only thing it guarantees on rung M0 is that the multiboot header lands in the first 8 KiB of the image
 * (the loader looks there and nowhere else) and that the stack has somewhere real to live. */

ENTRY(_start)

SECTIONS
{
    /* Multiboot loads us at 1 MiB. Below that is firmware and video memory. */
    . = 1M;

    .multiboot : ALIGN(4)
    {
        KEEP(*(.multiboot))
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

    /* The stack the Core switches to before it runs any Rust. 64 KiB is arbitrary and generous; M0 presses no
     * button on it. */
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
