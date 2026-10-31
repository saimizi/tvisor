#![no_std]

extern crate alloc;

#[cfg(target_arch = "aarch64")]
use core::arch::asm;

#[cfg(test)]
extern crate std;

pub mod cpu_startup;
pub mod cpu_topology;
pub mod debug_util;

pub mod aarch64_reg;
pub mod el2_policy;
pub mod el2_stage1_translation;
pub mod failure;
pub mod fdt;
pub mod gicv2;
pub mod guest_fdt;
pub mod guest_platform;
pub mod guest_stage2_translation;
pub mod heap_allocator;
pub mod linux_boot;
pub mod memory_map;
pub mod mmio;
pub mod page;
pub mod page_allocator;
pub mod pcpu;
pub mod platform;
pub mod publish;
pub mod secondary_backend;
pub mod secondary_boot;
pub mod secondary_entry;
pub mod secondary_mailbox;
pub mod secondary_record;
pub mod secondary_validation;
pub mod startup_publication;
pub mod startup_resources;
pub mod sync;
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
