#![no_std]
#![no_main]

use tvisor_util::debug_util::{DebugMemError, debug_fini, debug_init, debug_mem_error};
use tvisor_util::diag::DiagState;
use tvisor_util::println;

#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: isize, _argv: *const *const u8) -> isize {
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
