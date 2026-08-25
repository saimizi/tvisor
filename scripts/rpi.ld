OUTPUT_FORMAT(elf64-aarch64)
ENTRY(main)

__ALIGN_SIZE = 0x1000;
__LOAD_ADDR = 0x04000000;


SECTIONS {
    . = __LOAD_ADDR + SIZEOF_HEADERS;

    .text : ALIGN(__ALIGN_SIZE) {
        *(.text .text.*)
    }

    .data : ALIGN(__ALIGN_SIZE) {
        *(.data .data.*)
    }

    .rodata : ALIGN(__ALIGN_SIZE) {
        *(.rodata .rodata.*)
    }

    .got : ALIGN(__ALIGN_SIZE) {
        *(.got .got.*)
    }

    .bss : ALIGN(__ALIGN_SIZE) {
        __bss_start = .;
        *(.bss .bss.*)
        . = ALIGN(16);
        __bss_end = .;
    }
}
