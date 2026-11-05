/* The loadable program's layout. It is NOT the Core: it lives in the Core's free RAM at 0x41000000 (16 MiB above the
 * Core's 0x40080000), which is where the loader places PT_LOAD segments. The engine closure makes the Core's own
 * image (arena + ELF buffer) grow past 0x40200000, so the program moved up to leave the Core room. */

ENTRY(_start)

SECTIONS
{
    . = 0x41000000;

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
