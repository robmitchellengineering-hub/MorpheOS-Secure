/* The loadable program's layout. It is NOT the Core: it lives in the Core's free RAM at 0x40200000, which is where
 * the loader places PT_LOAD segments. */

ENTRY(_start)

SECTIONS
{
    . = 0x40200000;

    .text : ALIGN(16)
    {
        *(.text .text.*)
    }

    .rodata : ALIGN(16)
    {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(16)
    {
        *(.data .data.*)
    }

    .bss : ALIGN(16)
    {
        *(.bss .bss.*)
    }

    /DISCARD/ :
    {
        *(.comment)
        *(.note .note.*)
    }
}
