OUTPUT_FORMAT(elf64-aarch64)
ENTRY(main)

__ALIGN_SIZE = 0x1000;
__LOAD_ADDR = 0x04000000;
__image_start = __LOAD_ADDR;


SECTIONS {
    . = __LOAD_ADDR + SIZEOF_HEADERS;

    .text : ALIGN(__ALIGN_SIZE) {
        __text_start = .;
        *(.text .text.*)
        . = ALIGN(__ALIGN_SIZE);
        __text_end = .;
    }

    .payload : ALIGN(__ALIGN_SIZE) {
        __payload_start = .;
        KEEP(*(.payload .payload.*))
        . = ALIGN(__ALIGN_SIZE);
        __payload_end = .;
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

    .rodata : ALIGN(__ALIGN_SIZE) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        *(.eh_frame_hdr)
        *(.eh_frame)
        . = ALIGN(__ALIGN_SIZE);
        __rodata_end = .;
    }

    .data : ALIGN(__ALIGN_SIZE) {
        __writable_start = .;
        *(.data .data.*)
        *(.got .got.*)
        __bss_start = .;
        *(.bss .bss.*)
        . = ALIGN(16);
        __bss_end = .;
        . = ALIGN(__ALIGN_SIZE);
        __writable_end = .;
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

    /*
     * Linker-owned storage for the initial EL2 stage-1 tables.  Keeping this
     * arena inside tvisor's runtime footprint removes any need to allocate
     * physical pages while U-Boot's translation regime is still active.
     * NOLOAD means startup code must explicitly zero it before use.
     */
    .bootstrap_tables (NOLOAD) : ALIGN(__ALIGN_SIZE) {
        __bootstrap_tables_start = .;
        . += 0x10000;
        __bootstrap_tables_end = .;
    }

    . = ALIGN(__ALIGN_SIZE);
    __image_end = .;

    ASSERT((__bootstrap_tables_start & 0xfff) == 0,
           "bootstrap table arena must be 4 KiB aligned")
    ASSERT((__bootstrap_tables_end & 0xfff) == 0,
           "bootstrap table arena end must be 4 KiB aligned")
    ASSERT((__bootstrap_tables_end - __bootstrap_tables_start) == 0x10000,
           "bootstrap table arena must contain exactly 16 pages")
}
