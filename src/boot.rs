use core::{
    arch::{asm, global_asm},
    sync::atomic::{AtomicU64, Ordering},
};

use tvisor_util::aarch64_reg::{MairEl2, SctlrEl2, Sp, SpSel, TcrEl2, Ttbr0El2, VbarEl2};
use tvisor_util::boot_mode::FaultTest;
use tvisor_util::el2_translation::{El2RegisterValues, PAGE_SIZE};
use tvisor_util::println;
use tvisor_util::system_info::PhysRegion;

static PHASE7_RO_CANARY: u64 = 0x726f_6461_7461_5037;
static PHASE7_RW_CANARY: AtomicU64 = AtomicU64::new(0x7277_6461_7461_5037);

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
    bl   private_el2_main
1:
    wfe
    b    1b
    .size __enter_private_el2, . - __enter_private_el2
"#,
);

unsafe extern "C" {
    fn __enter_private_el2(
        fault_test: u64,
        mair_el2: u64,
        tcr_el2: u64,
        ttbr0_el2: u64,
        sctlr_el2: u64,
    ) -> !;
}

pub unsafe fn enter_private_el2(fault_test: FaultTest, registers: El2RegisterValues) -> ! {
    // SAFETY: The caller accepts the documented no-return state transition.
    unsafe {
        __enter_private_el2(
            fault_test as u64,
            registers.mair_el2,
            registers.tcr_el2,
            registers.ttbr0_el2,
            registers.sctlr_el2,
        )
    }
}

#[unsafe(no_mangle)]
extern "C" fn private_el2_main(
    fault_test: u64,
    mair_el2: u64,
    tcr_el2: u64,
    ttbr0_el2: u64,
    sctlr_el2: u64,
) -> ! {
    println!("Phase 6 private EL2 foundations active");
    println!("    SP: {:#018x}", Sp::dump().value);
    if let Some(spsel) = SpSel::dump() {
        println!(" SPSel: {:#018x}", spsel.value);
    }
    if let Some(vbar) = VbarEl2::dump() {
        println!("VBAR_EL2: {:#018x}", vbar.value);
    }
    println!("Phase 7 checkpoint 1: switching EL2 page tables");
    // SAFETY: All values were validated before takeover, table stores were
    // published, and this routine never returns to the inherited regime.
    unsafe { __switch_el2_page_tables(mair_el2, tcr_el2, ttbr0_el2, sctlr_el2, fault_test) }
}

global_asm!(
    r#"
    .section .text.takeover, "ax"
    .global __switch_el2_page_tables
    .type __switch_el2_page_tables, %function
__switch_el2_page_tables:
    // x0=MAIR_EL2, x1=TCR_EL2, x2=TTBR0_EL2, x3=SCTLR_EL2,
    // x4=deliberate fault-test selector. This critical interval is a leaf:
    // it uses no stack, literal pool, call, or return address.

    // Disable EL2 stage1 translation
    // after this setting CPU use PA instead of VA to access memory
    // since u-boot maps tvisor as VA = PA, it is safe here.
    dsb  sy
    mrs  x9, sctlr_el2
    bic  x9, x9, #1
    msr  sctlr_el2, x9
    isb

    // Setting up memory access type, it doesn't affect anything because mmu is currently off.
    msr  mair_el2, x0

    // Install tvisor's page table property like virtual adderss space, translation granule...
    msr  tcr_el2, x1

    // Install tvisor's bootstrap L1 root table
    msr  ttbr0_el2, x2
    isb

    // Invalidates all EL2 stage-1 TLB entries
    tlbi alle2
    dsb  sy
    isb

    // Install tvisor's SCTLR_EL2, including EL2 stage1 translation
    // From here using tvisor's page table, so tvisior must map itself identically VA=PA.
    msr  sctlr_el2, x3
    isb

    // Prepare for calling rust phase7_post_switch()
    // Reorder the still-live expected values into the Rust AAPCS64 argument
    // order: test, MAIR_EL2, TCR_EL2, TTBR0_EL2, SCTLR_EL2.
    mov  x9, x0
    mov  x0, x4
    mov  x4, x3
    mov  x3, x2
    mov  x2, x1
    mov  x1, x9
    b    phase7_post_switch
    .size __switch_el2_page_tables, . - __switch_el2_page_tables
"#,
);

unsafe extern "C" {
    fn __switch_el2_page_tables(
        mair_el2: u64,
        tcr_el2: u64,
        ttbr0_el2: u64,
        sctlr_el2: u64,
        fault_test: u64,
    ) -> !;
}

#[unsafe(no_mangle)]
extern "C" fn phase7_post_switch(
    fault_test: u64,
    expected_mair: u64,
    expected_tcr: u64,
    expected_ttbr0: u64,
    expected_sctlr: u64,
) -> ! {
    let stack_canary = 0x7476_6973_6f72_5037_u64;
    println!("Phase 7 checkpoint 2: tvisor EL2 page tables active");
    let actual_mair = MairEl2::dump().expect("EL2 MAIR readback").value;
    let actual_tcr = TcrEl2::dump().expect("EL2 TCR readback").value;
    let actual_ttbr0 = Ttbr0El2::dump().expect("EL2 TTBR0 readback").value;
    let actual_sctlr = SctlrEl2::dump().expect("EL2 SCTLR readback").value;
    println!("  MAIR_EL2={:#018x}", actual_mair);
    println!("   TCR_EL2={:#018x}", actual_tcr);
    println!(" TTBR0_EL2={:#018x}", actual_ttbr0);
    println!(" SCTLR_EL2={:#018x}", actual_sctlr);
    assert_eq!(actual_mair, expected_mair);
    assert_eq!(actual_tcr, expected_tcr);
    assert_eq!(actual_ttbr0, expected_ttbr0);
    assert_eq!(actual_sctlr, expected_sctlr);
    assert_eq!(stack_canary, 0x7476_6973_6f72_5037);
    assert_eq!(PHASE7_RO_CANARY, 0x726f_6461_7461_5037);
    PHASE7_RW_CANARY.store(0x5037_7772_6974_6162, Ordering::Relaxed);
    assert_eq!(
        PHASE7_RW_CANARY.load(Ordering::Relaxed),
        0x5037_7772_6974_6162
    );
    println!("Phase 7 checkpoint 3: register, stack, and image validation passed");
    phase8_allocator_test();
    if fault_test == FaultTest::Sync as u64 {
        println!("Triggering deliberate synchronous exception under tvisor tables...");
        unsafe { asm!("brk #0x600") };
        println!("Returned from deliberate synchronous exception under tvisor tables");
    }
    if fault_test == FaultTest::Guard as u64 {
        unsafe extern "C" {
            static __boot_stack_guard_start: u8;
        }
        let guard = core::ptr::addr_of!(__boot_stack_guard_start).cast_mut();
        println!(
            "Triggering deliberate guard-page write at {:#x}...",
            guard.addr()
        );
        // SAFETY: This opt-in negative test deliberately faults and never
        // returns; the private EL2 handler reports the translation fault.
        unsafe { core::ptr::write_volatile(guard, 0) };
    }
    if fault_test == FaultTest::Unmapped as u64 {
        const UNMAPPED_TEST_VA: usize = 0x2000_0000;
        println!("Triggering deliberate unmapped read at {UNMAPPED_TEST_VA:#x}...");
        // SAFETY: This opt-in negative test deliberately faults and never
        // returns; the private EL2 handler reports the translation fault.
        let _ = unsafe { core::ptr::read_volatile(UNMAPPED_TEST_VA as *const u8) };
    }
    println!("Phase 7 checkpoint complete; halting");
    loop {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}

fn phase8_allocator_test() {
    let initial = crate::mm::phase8_initialize().expect("Phase 8 allocator initialization");
    println!(
        "Phase 8 allocator active: total={} allocated={} free={}",
        initial.total_pages, initial.allocated_pages, initial.free_pages
    );

    let low_region = crate::mm::allocator_region(0).expect("Phase 8 low allocator region");
    let mut last_index = 0;
    while crate::mm::allocator_region(last_index + 1).is_some() {
        last_index += 1;
    }
    let high_region =
        crate::mm::allocator_region(last_index).expect("Phase 8 high allocator region");
    let low = crate::mm::allocate_page_in(low_region).expect("allocate low test page");
    let high = crate::mm::allocate_page_in(high_region).expect("allocate high test page");
    assert_ne!(low, high);

    let reclaimed_base = crate::mm::reclaimed_test_page().expect("reclaimed U-Boot test page");
    let reclaimed_region =
        PhysRegion::new(reclaimed_base, PAGE_SIZE).expect("reclaimed page region");
    let reclaimed =
        crate::mm::allocate_page_in(reclaimed_region).expect("allocate reclaimed U-Boot page");
    assert_ne!(reclaimed, low);
    assert_ne!(reclaimed, high);

    for (page, pattern) in [
        (low, 0x5038_4c4f_5750_4147_u64),
        (high, 0x5038_4849_4748_5047_u64),
        (reclaimed, 0x5038_5245_434c_4149_u64),
    ] {
        let pointer = page.value() as *mut u64;
        // SAFETY: the allocator returned a mapped, exclusively owned Normal
        // RAM page. Volatile accesses force the hardware validation traffic.
        unsafe {
            core::ptr::write_volatile(pointer, pattern);
            assert_eq!(core::ptr::read_volatile(pointer), pattern);
        }
    }
    println!(
        "Phase 8 page test: low={} high={} reclaimed={}",
        low, high, reclaimed
    );

    crate::mm::free_page(low).expect("free low test page");
    crate::mm::free_page(high).expect("free high test page");
    crate::mm::free_page(reclaimed).expect("free reclaimed test page");
    let reused = crate::mm::allocate_page().expect("reallocate first-fit page");
    assert_eq!(reused, low);
    crate::mm::free_page(reused).expect("free reused page");

    let final_stats = crate::mm::allocator_stats().expect("Phase 8 allocator statistics");
    assert_eq!(final_stats.allocated_pages, 0);
    assert_eq!(final_stats.free_pages, final_stats.total_pages);
    println!("Phase 8 checkpoint complete: allocator validation passed");
}
