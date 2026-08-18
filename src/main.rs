#![no_std]
#![no_main]

use tvisor_util::debug_util::write_debug_mem_u64;

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
    let value = current_el();
    write_debug_mem_u64(value);
    if value != 2 { 1 } else { 0 }
}

#[panic_handler]
pub fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
