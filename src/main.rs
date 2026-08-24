#![no_std]
#![no_main]

use core::arch::global_asm;
use tvisor_util::debug_util::{DebugMemError, debug_fini, debug_init, debug_mem_error};
use tvisor_util::diag::DiagState;
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
extern "C" fn rust_main(_argc: isize, _argv: *const *const u8) -> isize {
    let mut ret = 0_isize;
    debug_init();

    'diagnostic: {
        let diag_state = DiagState::dump();

        if diag_state.current_el != 2 {
            debug_mem_error(DebugMemError::InvalidEL2State);
            ret = 1;
            // We can only run at EL2
            break 'diagnostic;
        }

        // we are using little endian, don't support bigendian
        if diag_state.sctlr_el2.as_ref().is_some_and(|s| s.bit_ee()) {
            debug_mem_error(DebugMemError::UnexpectedEL2Endianness);
            ret = 1;
            break 'diagnostic;
        }

        // VBAR_EL2 must be 2048-byte aligned; a misaligned vector base is fatal
        if diag_state
            .vbar_el2
            .as_ref()
            .is_some_and(|v| !v.is_aligned())
        {
            debug_mem_error(DebugMemError::InvalidVectorBaseAlignment);
            ret = 1;
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
        }

        // this should be outputted after endian (EE) is checked
        println!("{}", diag_state);
    }

    debug_fini();

    ret
}

#[panic_handler]
pub fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
