OUTPUT_FORMAT(elf64-aarch64)
ENTRY(main)

__ALIGN_SIZE = 0x1000;
__LOAD_ADDR = 0x04000000;
__image_start = __LOAD_ADDR;


SECTIONS {
    . = __LOAD_ADDR + SIZEOF_HEADERS;

    .text : ALIGN(__ALIGN_SIZE) {
        *(.text .text.*)
    }

    /*
     * AArch64 has 16 exception-vector slots of 128 bytes, so VBAR_EL2 must
     * point to a 2 KiB-aligned table.  Use the stricter 4 KiB page alignment
     * and reserve a complete page for independent Phase 7 permissions.
     */
    .vectors : ALIGN(__ALIGN_SIZE) {
        __vectors_start = .;
        KEEP(*(.vectors))
        . = ALIGN(__ALIGN_SIZE);
        __vectors_end = .;
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

    /*
     * Reserve one page below the downward-growing boot stack for stack-
     * overflow detection.  It becomes an effective guard page when Phase 7
     * installs tvisor's page tables and leaves this region unmapped.
     */
    .boot_stack_guard (NOLOAD) : ALIGN(__ALIGN_SIZE) {
        __boot_stack_guard_start = .;
        . += 0x1000;
        __boot_stack_guard_end = .;
    }

    .boot_stack (NOLOAD) : ALIGN(__ALIGN_SIZE) {
        __boot_stack_bottom = .;
        . += 0x10000;
        __boot_stack_top = .;
    }

    . = ALIGN(__ALIGN_SIZE);
    __image_end = .;
}
