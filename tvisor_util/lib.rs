#![no_std]

extern crate alloc;

#[cfg(target_arch = "aarch64")]
use core::arch::asm;

#[cfg(test)]
extern crate std;

pub mod debug_util;

pub mod aarch64_reg;
pub mod el2_translation;
pub mod fdt;
pub mod guest_fdt;
pub mod heap_allocator;
pub mod linux_boot;
pub mod memory_map;
pub mod mmio;
pub mod page;
pub mod page_allocator;
pub mod platform;
pub mod stage2_translation;
pub mod system_info;
pub mod virtual_timer;

pub use page::*;

pub fn halt() -> ! {
    loop {
        #[cfg(target_arch = "aarch64")]
        unsafe {
            asm!("wfe", options(nomem, nostack, preserves_flags))
        };
        #[cfg(not(target_arch = "aarch64"))]
        core::hint::spin_loop();
    }
}
