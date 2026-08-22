#![no_std]
#![no_main]

use tvisor_util::debug_util::{DebugMemError, debug_fini, debug_init, debug_mem_error};
use tvisor_util::*;

#[inline(always)]
fn current_el() -> u64 {
    let value: u64;

    unsafe {
        core::arch::asm!(
            "mrs {value}, CurrentEL",
            value = out(reg) value,
        );
    }

    (value >> 2) & 0b11
}

#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: isize, _argv: *const *const u8) -> isize {
    let mut ret = 0_isize;
    debug_init();

    let value = current_el();
    println!("EL2: {}", value);
    if value != 2 {
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
