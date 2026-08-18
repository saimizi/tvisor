const DEBUG_MEM_START: *mut u8 = 0x00001000 as *mut u8;
const DEBUG_MEM_SIZE: usize = 0x00001000;

pub unsafe fn write_debug_mem(value: *const u8, size: usize) -> bool {
    if size > DEBUG_MEM_SIZE {
        false
    } else {
        unsafe {
            core::ptr::copy(value, DEBUG_MEM_START, size);
        }
        true
    }
}

pub fn write_debug_mem_u64(value: u64) {
    unsafe { core::ptr::write_volatile(DEBUG_MEM_START as *mut u64, value) };
}
