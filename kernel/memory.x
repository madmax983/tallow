/* Linker script for the Tallow kernel image (ESP32-S3).
 *
 * The ROM bootloader loads our image segments from flash and jumps to
 * ENTRY(_start). All code/data lives in HP SRAM reached through the IRAM
 * bus (0x4037_0000 .. 0x403E_0000), which is executable. The stack is set up
 * manually in _start at the top of the DRAM view (0x3FCF_FFE0), so no RAM
 * section is needed here.
 */

ENTRY(_start);

MEMORY
{
    /* 448 KiB of HP SRAM via the IRAM bus (executable) */
    IRAM : ORIGIN = 0x40370000, LENGTH = 448K
}

SECTIONS
{
    .text : ALIGN(4)
    {
        /* Xtensa code loads constants with PC-relative `l32r`, whose pools
           live in `.literal*` sections. The Xtensa linker requires each
           literal to be placed BEFORE the code that loads it ("dangerous
           relocation: l32r: literal placed after use" otherwise), so all
           literals go first. NOTE: do NOT KEEP() _start ahead of the
           literals — its own literal pool must precede it too. The ELF
           entry address (ENTRY(_start)) is what the ROM jumps to, not the
           section base, so _start does not need to be first. */
        *(.literal .literal.*);
        *(.text .text.*);
        *(.rodata .rodata.*);
    } > IRAM

    /* Discard everything else the toolchain might emit; we want exactly one
       LOAD segment so esptool.py elf2image produces a clean boot image. */
    /DISCARD/ :
    {
        *(.comment .comment.*);
        *(.note .note.*);
        *(.eh_frame .eh_frame.*);
    }
}
