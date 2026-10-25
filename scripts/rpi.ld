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
    }

    /* save binary size to put it at last */
    .bss (NOLOAD) : ALIGN(__ALIGN_SIZE) {
        __bss_start = .;
        *(.bss .bss.*)
        . = ALIGN(16);
        __bss_end = .;
        . = ALIGN(__ALIGN_SIZE);
        __writable_end = .;
    }

    /* Four physical-CPU slots: each has a 4 KiB guard then a 64 KiB stack.
     * EL2 stage-1 maps stacks individually and never maps the guard pages.
     * Rust validates these constants against the shared layout model. */
    .pcpu_stacks (NOLOAD) : ALIGN(__ALIGN_SIZE) {
        __pcpu_stacks_start = .;
        . += 4 * (0x1000 + 0x10000);
        __pcpu_stacks_end = .;
    }

    /*
     * Linker-owned storage for tvisor's runtime EL2 stage-1 tables.  Keeping this
     * arena inside tvisor's runtime footprint removes any need to allocate
     * physical pages while U-Boot's translation regime is still active.
     * NOLOAD means startup code must explicitly zero it before use.
     */
    .el2_stage1_tables (NOLOAD) : ALIGN(__ALIGN_SIZE) {
        __el2_stage1_tables_start = .;
        . += 0x10000;
        __el2_stage1_tables_end = .;
    }

    . = ALIGN(__ALIGN_SIZE);
    __image_end = .;

    ASSERT((__pcpu_stacks_start & 0xfff) == 0,
           "per-CPU stack arena must be page aligned")
    ASSERT((__pcpu_stacks_end - __pcpu_stacks_start) == 0x44000,
           "four guarded stacks must occupy 272 KiB")
    ASSERT(__pcpu_stacks_end <= __el2_stage1_tables_start,
           "per-CPU stacks overlap the table arena")
    ASSERT((__el2_stage1_tables_start & 0xfff) == 0,
           "EL2 stage-1 table arena must be 4 KiB aligned")
    ASSERT((__el2_stage1_tables_end & 0xfff) == 0,
           "EL2 stage-1 table arena end must be 4 KiB aligned")
    ASSERT((__el2_stage1_tables_end - __el2_stage1_tables_start) == 0x10000,
           "EL2 stage-1 table arena must contain exactly 16 pages")
}
