#![no_std]
#![no_main]

use tvisor_util::debug_util::{DebugMemError, debug_fini, debug_init, debug_mem_error};
use tvisor_util::diag::DiagState;
use tvisor_util::println;

#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: isize, _argv: *const *const u8) -> isize {
    let mut ret = 0_isize;
    debug_init();

    let diag_state = DiagState::dump();
    println!("{}", diag_state);

    if diag_state.current_el != 2 {
        debug_mem_error(DebugMemError::InvalidEL2State);
        ret = 1;
    }

    debug_fini();

    ret
}

#[panic_handler]
pub fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
