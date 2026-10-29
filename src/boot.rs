use alloc::format;
use core::{
    arch::{asm, global_asm},
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use tvisor_util::debug_util::stop;
use tvisor_util::fdt::fdt;
use tvisor_util::memory_map::MemoryMap;
use tvisor_util::platform::discover_memory_map;
use tvisor_util::system_info::{PhysAddr, PhysRegion};
use tvisor_util::*;
use tvisor_util::{aarch64_reg::*, gicv2::global};

use crate::{
    guest::get_host_console_irq,
    heap::{self, stats},
    mm,
};

static PHASE7_RO_CANARY: u64 = 0x726f_6461_7461_5037;
static PHASE7_RW_CANARY: AtomicU64 = AtomicU64::new(0x7277_6461_7461_5037);
// Boot-only scratch storage is claimed once; it is never shared with workers.
// Keep the large map out of the private stack without nesting a boot-map mutex
// around allocator, mapping, heap and console operations.
struct BootMemoryMap {
    claimed: AtomicBool,
    map: UnsafeCell<MemoryMap>,
}
// SAFETY: only the successful one-time claimant can obtain the mutable view.
// No other accessor exposes this storage to workers or interrupt handlers.
unsafe impl Sync for BootMemoryMap {}
impl BootMemoryMap {
    fn claim(&'static self) -> Option<&'static mut MemoryMap> {
        if self.claimed.swap(true, Ordering::AcqRel) {
            return None;
        }
        // SAFETY: the successful claim exclusively owns this storage forever.
        Some(unsafe { &mut *self.map.get() })
    }
}
static MEMORY_MAP: BootMemoryMap = BootMemoryMap {
    claimed: AtomicBool::new(false),
    map: UnsafeCell::new(MemoryMap::empty()),
};

unsafe extern "C" {
    fn __enter_private_el2(
        mair_el2: u64,
        tcr_el2: u64,
        ttbr0_el2: u64,
        sctlr_el2: u64,
        pcpu: *mut tvisor_util::pcpu::PcpuContext<crate::vcpu::Vcpu>,
        stack_top: u64,
    ) -> !;
    fn __switch_el2_stage1_page_tables(
        mair_el2: u64,
        tcr_el2: u64,
        ttbr0_el2: u64,
        sctlr_el2: u64,
    ) -> !;
    static __image_start: u8;
    static __image_end: u8;
}

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
    // x4 = prepared boot CPU context, x5 = private stack top.
    mov  sp, x5
    msr  tpidr_el2, x4
    isb
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

pub unsafe fn enter_private_el2(el2_stage1_page_table: mm::El2Stage1PageTable) -> ! {
    if !is_page_aligned(el2_stage1_page_table.root_pa as usize) {
        println!("EL2 stage-1 page table is not page aligned");
        stop()
    }

    let tcr_el2 = TCR_EL2_RES1
        | TCR_EL2_T0SZ_FOR_VA_39_BIT
        | TCR_EL2_IRGN0_MEM_WB_RA_WA
        | TCR_EL2_ORGN0_MEM_WB_RA_WA
        | TCR_EL2_SH0_INNER_SHAREABLE
        | ((el2_stage1_page_table.pa_range as u64) << 16);

    const SCTLR_EL2_RES1: u64 =
        (0b11 << 28) | (0b11 << 22) | (1 << 18) | (1 << 16) | (1 << 11) | (0b11 << 4);
    const SCTLR_EL2_VALUE: u64 =
        SCTLR_EL2_RES1 | (1 << 0) | (1 << 2) | (1 << 3) | (1 << 12) | (1 << 19);

    let (pcpu, stack_top) = crate::pcpu::boot_entry();
    unsafe {
        __enter_private_el2(
            MAIR_EL2_VALUE,
            tcr_el2,
            el2_stage1_page_table.root_pa,
            SCTLR_EL2_VALUE,
            pcpu,
            stack_top,
        )
    }
}

#[unsafe(no_mangle)]
extern "C" fn private_el2_main(mair_el2: u64, tcr_el2: u64, ttbr0_el2: u64, sctlr_el2: u64) -> ! {
    crate::pcpu::assert_boot_cpu_idle();
    println!("Phase 6 private EL2 foundations active");
    println!("    SP: {:#018x}", Sp::dump().value);
    if let Some(spsel) = SpSel::dump() {
        println!(" SPSel: {:#018x}", spsel.value);
    }
    if let Some(vbar) = VbarEl2::dump() {
        println!("VBAR_EL2: {:#018x}", vbar.value);
    }
    println!("Phase 7 checkpoint 1: switching EL2 stage-1 page tables");
    // SAFETY: All values were validated before takeover, table stores were
    // published, and this routine never returns to the inherited regime.
    unsafe { __switch_el2_stage1_page_tables(mair_el2, tcr_el2, ttbr0_el2, sctlr_el2) }
}

global_asm!(
    r#"
    .section .text.takeover, "ax"
    .global __switch_el2_stage1_page_tables
    .type __switch_el2_stage1_page_tables, %function
__switch_el2_stage1_page_tables:
    // x0=MAIR_EL2, x1=TCR_EL2, x2=TTBR0_EL2, x3=SCTLR_EL2,
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

    // Install tvisor's EL2 L1 root table
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

    // Same parameter order  MAIR_EL2, TCR_EL2, TTBR0_EL2, SCTLR_EL2.
    b    post_switch_page_tables
    .size __switch_el2_stage1_page_tables, . - __switch_el2_stage1_page_tables
"#,
);

fn check_switched_environment(
    expected_mair: u64,
    expected_tcr: u64,
    expected_ttbr0: u64,
    expected_sctlr: u64,
) {
    let stack_canary = 0x7476_6973_6f72_5037_u64;
    println!("Phase 7 checkpoint 2: tvisor EL2 stage-1 page tables active");
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
}

#[unsafe(no_mangle)]
extern "C" fn post_switch_page_tables(
    expected_mair: u64,
    expected_tcr: u64,
    expected_ttbr0: u64,
    expected_sctlr: u64,
) -> ! {
    'wait: {
        check_switched_environment(expected_mair, expected_tcr, expected_ttbr0, expected_sctlr);
        crate::pcpu::report_prepared();

        // After EL2 stage1 is mapped, we recheck the console IRQ
        let Some(gicv2) = global() else { break 'wait };
        if get_host_console_irq() >= gicv2.num_interrupts() {
            break 'wait;
        }

        let image_start = core::ptr::addr_of!(__image_start) as u64;
        let image_end = core::ptr::addr_of!(__image_end) as u64;
        let tvisor_image =
            match PhysRegion::from_bounds(PhysAddr::new(image_start), PhysAddr::new(image_end)) {
                Ok(region) => region,
                Err(error) => {
                    println!("Platform discovery failed: invalid tvisor image: {}", error);
                    break 'wait;
                }
            };

        let Some(fdt) = fdt() else {
            println!("No fdt found");
            break 'wait;
        };

        let live_dtb: PhysRegion = (*fdt).into();
        let Some(linux_image_source) = crate::guest::linux_image_source() else {
            println!("No Linux Image source was supplied by U-Boot");
            break 'wait;
        };
        let linux_image_pages = match PhysRegion::new_aligned(
            linux_image_source.start(),
            linux_image_source.size(),
            PAGE_SIZE as u64,
        ) {
            Ok(region) => region,
            Err(error) => {
                println!("Linux Image source cannot be page-aligned: {}", error);
                break 'wait;
            }
        };

        let Some(memory_map) = MEMORY_MAP.claim() else {
            println!("Boot memory map already claimed");
            break 'wait;
        };
        if let Err(error) = discover_memory_map(
            *fdt,
            PhysAddr::new(fdt.data().as_ptr() as usize as u64),
            tvisor_image,
            memory_map,
        ) {
            println!("Platform discovery failed: {}", error);
            break 'wait;
        }
        if let Err(error) = memory_map.insert_reserved(linux_image_pages) {
            println!("Cannot reserve Linux Image source: {}", error);
            break 'wait;
        }
        if let Err(error) = memory_map.finalize() {
            println!("Cannot finalize Linux Image source reservation: {}", error);
            break 'wait;
        }

        let memory_map: &MemoryMap = memory_map;
        let Some(startup) = crate::smp::startup_contract() else {
            println!("No validated CPU startup-memory description");
            break 'wait;
        };
        if let Err(error) = startup.verify_memory_map(*fdt, memory_map) {
            println!("Secondary holding-memory reservation failed: {:?}", error);
            break 'wait;
        }
        println!(
            "Phase 11.0 DT firmware reservations verified before allocation; spin-table cells={}",
            startup.release_cells().len()
        );
        let initialized = match mm::initialize_allocator_after_takeover(memory_map, live_dtb) {
            Ok(initialized) => initialized,
            Err(error) => {
                println!("Allocator initialization failed: {}", error);
                break 'wait;
            }
        };

        println!(
            "Phase 8 allocator initialized after takeover: RAM={} reserved={} in-use={} unused={} DTB={}",
            initialized.stats.ram_pages,
            initialized.stats.reserved_pages,
            initialized.stats.in_use_pages,
            initialized.stats.unused_pages,
            initialized.live_dtb,
        );

        // Bitmap metadata lives in the already mapped tvisor image. Dynamic
        // stack allocation touches only those bitmaps until RAM is mapped.
        if let Err(error) =
            crate::pcpu::prepare_secondary_cpus(memory_map, live_dtb, linux_image_pages)
        {
            println!("Secondary CPU preparation failed: {}", error);
            break 'wait;
        }

        // Initialize heap allocator
        let heap = match heap::initialize(None) {
            Ok(heap) => heap,
            Err(error) => {
                println!("Heap initialization failed: {}", error);
                break 'wait;
            }
        };

        let initialize_heap_state = format!(
            "Rust heap initialized: arena=[{}, {}) bytes={} free={}",
            heap.arena_start, heap.arena_end, heap.stats.arena_bytes, heap.stats.free_bytes,
        );
        println!("{}", initialize_heap_state);

        let Ok(current_heap) = stats() else {
            println!("Failed to get current heap stats");
            break 'wait;
        };

        let current_heap_state = format!(
            "Rust heap now: arena=[{:#018x}, {:#018x}) bytes={} free={}",
            current_heap.arena_start,
            current_heap.arena_start + current_heap.arena_bytes,
            current_heap.arena_bytes,
            current_heap.free_bytes
        );
        println!("{}", current_heap_state);

        match crate::guest::run_guest() {
            Ok(()) => println!("Linux guest exited cleanly; halting"),
            Err(error) => println!("Linux guest execution failed: {}", error),
        }
    }

    loop {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}
