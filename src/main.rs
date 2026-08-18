#![no_std]
#![no_main]

use core::arch::asm;

const DEBUG_MEM_START: *mut u8 = 0x0200_0000 as *mut u8;
const DEBUG_MEM_SIZE: usize = 4 * 1024;

#[allow(unused)]
fn write_debug_mem(value: *const u8, size: usize) -> bool {
    if size > DEBUG_MEM_SIZE {
        false
    } else {
        unsafe {
            core::ptr::copy(value, DEBUG_MEM_START, size);
        }
        true
    }
}

fn write_debug_mem_u64(value: u64) {
    unsafe { core::ptr::write_volatile(DEBUG_MEM_START as *mut u64, value) };
}

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
    loop {
        let value = current_el();
        if value != 2 {
            write_debug_mem_u64(value);
            return -1;
        }

        unsafe {
            asm!("wfi");
        }
    }
}

#[panic_handler]
pub fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
