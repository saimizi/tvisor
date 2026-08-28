use core::arch::{asm, global_asm};

use tvisor_util::aarch64_reg::{Sp, SpSel, VbarEl2};
use tvisor_util::println;

global_asm!(
    r#"
    .section .text.takeover, "ax"
    .global __enter_private_el2
    .type __enter_private_el2, %function
__enter_private_el2:
    // Keep Debug, SError, IRQ, and FIQ masked after entering Rust.  Phase 6
    // owns VBAR_EL2 but has no asynchronous interrupt-controller support yet;
    // synchronous exceptions remain available for the vector-table test.
    msr  daifset, #0xf
    isb
    msr  spsel, #1
    isb
    adrp x9, __boot_stack_top
    add  x9, x9, :lo12:__boot_stack_top
    mov  sp, x9
    adrp x9, __el2_vectors
    add  x9, x9, :lo12:__el2_vectors
    msr  vbar_el2, x9
    isb
    bl   phase6_takeover_main
1:
    wfe
    b    1b
    .size __enter_private_el2, . - __enter_private_el2
"#,
);

unsafe extern "C" {
    fn __enter_private_el2(test_sync_fault: u64) -> !;
}

pub unsafe fn enter_private_el2(test_sync_fault: bool) -> ! {
    // SAFETY: The caller accepts the documented no-return state transition.
    unsafe { __enter_private_el2(u64::from(test_sync_fault)) }
}

#[unsafe(no_mangle)]
extern "C" fn phase6_takeover_main(test_sync_fault: u64) -> ! {
    println!("Phase 6 private EL2 foundations active");
    println!("    SP: {:#018x}", Sp::dump().value);
    if let Some(spsel) = SpSel::dump() {
        println!(" SPSel: {:#018x}", spsel.value);
    }
    if let Some(vbar) = VbarEl2::dump() {
        println!("VBAR_EL2: {:#018x}", vbar.value);
    }
    if test_sync_fault != 0 {
        println!("Triggering deliberate synchronous exception...");
        unsafe { asm!("brk #0x600") };
        println!("Returned from deliberate synchronous exception");
    }
    println!("Phase 6 checkpoint complete; halting");
    loop {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}
