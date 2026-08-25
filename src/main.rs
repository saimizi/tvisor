#![no_std]
#![no_main]

use core::arch::global_asm;
use dtoolkit::{Node, Property};
use tvisor_util::aarch64_reg::CurrentEL;
use tvisor_util::debug_util::{DebugMemError, debug_fini, debug_init, debug_mem_error};
use tvisor_util::diag::{DiagState, should_collect_full_diagnostics};
use tvisor_util::fdt::{fdt_address_from_uboot_args, fdt_init};
use tvisor_util::println;

global_asm!(
    r#"
    .section .text.main, "ax"
    .global main
    .type main, %function
main:
    sub  sp, sp, #112
    stp  x18, x19, [sp, #0]
    stp  x20, x21, [sp, #16]
    stp  x22, x23, [sp, #32]
    stp  x24, x25, [sp, #48]
    stp  x26, x27, [sp, #64]
    stp  x28, x29, [sp, #80]
    str  x30, [sp, #96]

    // U-Boot's `go` command jumps into the bytes loaded by TFTP and does not
    // process the ELF NOBITS segment. Initialize Rust's zero-initialized
    // statics before entering any Rust code.
    adrp x9, __bss_start
    add  x9, x9, :lo12:__bss_start
    adrp x10, __bss_end
    add  x10, x10, :lo12:__bss_end
1:
    cmp  x9, x10
    b.hs 2f
    str  xzr, [x9], #8
    b    1b
2:

    bl   rust_main
    str  x0, [sp, #104]

    ldp  x18, x19, [sp, #0]
    ldp  x20, x21, [sp, #16]
    ldp  x22, x23, [sp, #32]
    ldp  x24, x25, [sp, #48]
    ldp  x26, x27, [sp, #64]
    ldp  x28, x29, [sp, #80]
    ldr  x30, [sp, #96]
    ldr  x0, [sp, #104]
    add  sp, sp, #112
    ret
    .size main, . - main
"#,
);

#[unsafe(no_mangle)]
extern "C" fn rust_main(argc: isize, argv: *const *const u8) -> isize {
    let mut ret = 0_isize;
    debug_init();

    'main: {
        // Validate the execution level before reading any trap-sensitive
        // registers. In particular, ID-group register reads performed at EL1
        // can be redirected to EL2 by HCR_EL2.TID3.
        let current_el = CurrentEL::dump();
        if !should_collect_full_diagnostics(current_el.current_el()) {
            debug_mem_error(DebugMemError::InvalidEL2State);
            println!("CurrentEL: {:#018x}", current_el.value);
            ret = 1;
            // We can only run at EL2
            break 'main;
        }

        let diag_state = DiagState::dump();

        // we are using little endian, don't support bigendian
        if diag_state.sctlr_el2.as_ref().is_some_and(|s| s.bit_ee()) {
            debug_mem_error(DebugMemError::UnexpectedEL2Endianness);
            ret = 1;
            break 'main;
        }

        // VBAR_EL2 must be 2048-byte aligned; a misaligned vector base is fatal
        if diag_state
            .vbar_el2
            .as_ref()
            .is_some_and(|v| !v.is_aligned())
        {
            debug_mem_error(DebugMemError::InvalidVectorBaseAlignment);
            ret = 1;
            break 'main;
        }

        // The processor must report EL2 support; EL2 == 0 is inconsistent with
        // executing at EL2.
        if diag_state
            .id_aa64pfr0_el1
            .as_ref()
            .is_some_and(|r| r.el2() == 0)
        {
            debug_mem_error(DebugMemError::UnsupportedEL2Feature);
            ret = 1;
            break 'main;
        }

        // this should be outputted after endian (EE) is checked
        println!("{}", diag_state);

        // SAFETY: U-Boot invokes tvisor using its standalone-application ABI,
        // which supplies argc readable, NUL-terminated strings through argv.
        let dtb_base = match unsafe { fdt_address_from_uboot_args(argc, argv) } {
            Ok(address) => address,
            Err(error) => {
                println!("Invalid U-Boot FDT argument: {}", error);
                ret = 1;
                debug_mem_error(DebugMemError::InvalidDTB);
                break 'main;
            }
        };

        println!("DTB base: {:#x}", dtb_base as usize);

        // SAFETY: The fdt= argument names U-Boot's live working DTB, whose
        // complete totalsize range remains readable while tvisor can return to
        // U-Boot.
        match unsafe { fdt_init(dtb_base) } {
            Ok(fdt) => {
                println!(
                    "DTB version={}, size={:#x}",
                    fdt.version(),
                    fdt.data().len()
                );

                match fdt.root().property("model") {
                    Some(property) => match property.as_str() {
                        Ok(model) => println!("DTB model={}", model),
                        Err(error) => println!("DTB model is invalid: {}", error),
                    },
                    None => println!("DTB model=<missing>"),
                }
            }
            Err(error) => {
                println!("Failed to parse DTB: {}", error);
                ret = 1;
                debug_mem_error(DebugMemError::InvalidDTB);
                break 'main;
            }
        }
    }

    debug_fini();

    ret
}

#[panic_handler]
pub fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("PANIC: {}", info);
    loop {}
}
