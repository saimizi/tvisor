//! Guest platform initialization, Stage-2 translation setup, and Phase 9 test runner.

use core::arch::global_asm;

use tvisor_util::aarch64_reg::{IdAa64Mmfr0El1, VmpidrEl2};
use tvisor_util::el2_translation::{PAGE_SIZE, TranslationError, pa_bits_from_parange};
use tvisor_util::guest_fdt::{GuestFdtConfig, GuestMemoryRegion, build_guest_dtb};
use tvisor_util::page_allocator::AllocatorError;
use tvisor_util::println;
use tvisor_util::stage2_translation::{
    Stage2Access, Stage2Allocator, Stage2Exec, Stage2Mapping, Stage2MemoryType,
    Stage2RegisterValues, Stage2TableSet, stage2_register_values,
};
use tvisor_util::system_info::PhysAddr;

use crate::mm;
use crate::vcpu::{__vcpu_run, VcpuContext, VcpuExit, VcpuExitReason};

pub const GUEST_PAYLOAD_IPA: u64 = 0x4000_0000;
pub const GUEST_SCRATCH_IPA: u64 = 0x4000_1000;
pub const GUEST_GUARD_IPA: u64 = 0x4000_2000;
pub const GUEST_STACK_IPA: u64 = 0x4000_3000;
pub const GUEST_STACK_TOP_IPA: u64 = 0x4000_4000;
pub const GUEST_DTB_IPA: u64 = 0x4010_0000;

unsafe extern "C" {
    static __payload_start: u8;
    static __payload_end: u8;
}

global_asm!(
    r#"
    .section .payload, "ax"
    .global __el1_test_payload
    .type __el1_test_payload, %function
__el1_test_payload:
    // 1. Initialize EL1 stack pointer to 0x4000_4000 (top of stack page [0x4000_3000, 0x4000_4000))
    // Note: [0x4000_2000, 0x4000_3000) is the unmapped stack guard page.
    mov  x9, #0x40000000
    add  x9, x9, #0x4000
    mov  sp, x9

    // 2. Checkpoint 1: Memory write and read test in scratch data page [0x4000_1000, 0x4000_2000)
    // Write pattern 0x5039_5041_594c_4f41 ("P9PAYLOA") to 0x4000_1000
    movz x10, #0x4f41
    movk x10, #0x594c, lsl #16
    movk x10, #0x5041, lsl #32
    movk x10, #0x5039, lsl #48
    mov  x11, #0x40000000
    add  x11, x11, #0x1000
    str  x10, [x11]
    ldr  x12, [x11]
    cmp  x10, x12
    b.ne .Lfail_mem

    // Signal Checkpoint 1 via HVC #0 with x0 = 1, x1 = read pattern
    mov  x0, #1
    mov  x1, x12
    hvc  #0

    // 3. Checkpoint 2: System register verification
    // Verify CurrentEL is EL1 (bits [3:2] == 0b01 -> CurrentEL value == 0x04)
    mrs  x13, CurrentEL
    lsr  x14, x13, #2
    and  x14, x14, #0x3
    cmp  x14, #1
    b.ne .Lfail_current_el

    // Verify MPIDR_EL1 has bit 30 (UP) set and Aff0 == 0
    mrs  x15, MPIDR_EL1
    tbz  x15, #30, .Lfail_mpidr_u
    and  x16, x15, #0xff
    cbnz x16, .Lfail_mpidr_aff

    // Read SCTLR_EL1 to verify accessibility
    mrs  x16, SCTLR_EL1

    // Signal Checkpoint 2 via HVC #0 with x0 = 2, x1 = MPIDR_EL1
    mov  x0, #2
    mov  x1, x15
    hvc  #0

    // 4. Checkpoint 3: Deliberate Stage-2 Translation Fault
    // Attempt to read from unmapped guest IPA 0x3000_0000
    movz x17, #0x3000, lsl #16
    ldr  x18, [x17]

    // If it did not fault, report failure via HVC #4
    mov  x0, #0xdead
    mov  x1, x18
    hvc  #4
    b    .Lhang

.Lfail_mem:
    mov  x0, #0xdead
    mov  x1, x12
    hvc  #1
    b    .Lhang

.Lfail_current_el:
    mov  x0, #0xdead
    mov  x1, x13
    hvc  #2
    b    .Lhang

.Lfail_mpidr_u:
.Lfail_mpidr_aff:
    mov  x0, #0xdead
    mov  x1, x15
    hvc  #3
    b    .Lhang

.Lhang:
    wfe
    b    .Lhang
    .size __el1_test_payload, . - __el1_test_payload
"#
);

#[inline]
unsafe fn dcache_line_size() -> usize {
    let ctr: u64;
    unsafe {
        core::arch::asm!(
            "mrs {ctr}, CTR_EL0",
            ctr = out(reg) ctr,
            options(nostack, preserves_flags)
        );
    }
    let dminline = ((ctr >> 16) & 0xf) as u32;
    4 << dminline
}

#[inline]
unsafe fn clean_dcache_poc(start: usize, end: usize) {
    let line_size = unsafe { dcache_line_size() };
    let mut addr = start & !(line_size - 1);
    while addr < end {
        unsafe {
            core::arch::asm!(
                "dc cvac, {addr}",
                addr = in(reg) addr,
                options(nostack, preserves_flags),
            );
        }
        addr += line_size;
    }
    unsafe {
        core::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));
    }
}

#[inline]
unsafe fn invalidate_icache_all() {
    unsafe {
        core::arch::asm!(
            "ic ialluis",
            "dsb ish",
            "isb",
            options(nostack, preserves_flags)
        );
    }
}

/// Activates Stage-2 translation and sets virtualization registers for the guest VM.
///
/// Ensures descriptor writes are published (`dsb ishst`), installs virtualization registers,
/// invalidates prior guest TLB entries for VMID 1, and synchronizes context with `isb`.
#[inline]
unsafe fn activate_stage2(regs: &Stage2RegisterValues) {
    unsafe {
        // 1. Ensure descriptor writes are visible to hardware table walkers
        core::arch::asm!("dsb ishst", options(nostack, preserves_flags));

        // 2. Install the Stage-2 translation context while HCR_EL2.VM remains
        // clear. The following ISB makes the new VTTBR_EL2 VMID and VTCR_EL2
        // configuration visible to the current-VMID TLBI.
        core::arch::asm!(
            "msr VTCR_EL2, {vtcr}",
            "msr VTTBR_EL2, {vttbr}",
            "msr CPTR_EL2, {cptr}",
            vtcr = in(reg) regs.vtcr_el2,
            vttbr = in(reg) regs.vttbr_el2,
            cptr = in(reg) regs.cptr_el2,
            options(nostack, preserves_flags),
        );
        VmpidrEl2 {
            value: regs.vmpidr_el2,
        }
        .write();
        core::arch::asm!("isb", options(nostack, preserves_flags));

        // 3. Invalidate guest TLBs for the newly installed VMID and synchronize.
        core::arch::asm!(
            "tlbi vmalls12e1is",
            "dsb ish",
            "isb",
            options(nostack, preserves_flags)
        );

        // 4. Enable the fully initialized Stage-2 context. The final ISB
        // ensures the new HCR_EL2 controls apply before entering the guest.
        core::arch::asm!(
            "msr HCR_EL2, {hcr}",
            "isb",
            hcr = in(reg) regs.hcr_el2,
            options(nostack, preserves_flags),
        );
    }
}

/// Deactivates Stage-2 translation and cleanly invalidates the guest VM context.
///
/// Crucially, `tlbi vmalls12e1is` is executed while `VTTBR_EL2` still has the guest's
/// VMID installed so that invalidation targets the correct VMID. Stage-2 translation
/// and `VTTBR_EL2` are only cleared after invalidation is fully synchronized.
#[inline]
unsafe fn deactivate_stage2() {
    unsafe {
        // 1. Invalidate all Stage 1 & 2 translations for the current VMID while VTTBR_EL2 is active
        core::arch::asm!("tlbi vmalls12e1is", options(nostack, preserves_flags));

        // 2. Complete invalidation across Inner Shareable domain and synchronize
        core::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));

        // 3. Disable only Stage-2 translation, preserving the other HCR_EL2
        // controls selected by tvisor. CPTR_EL2 is deliberately left at its
        // valid Phase-9 value because it contains architecturally RES1 fields
        // and must not be cleared with xzr.
        core::arch::asm!(
            "mrs x9, HCR_EL2",
            "bic x9, x9, #1",
            "msr HCR_EL2, x9",
            "isb",
            "msr VTTBR_EL2, xzr",
            "isb",
            out("x9") _,
            options(nostack, preserves_flags),
        );
    }
}

/// Resource tracker that records every allocated guest page and provides transactional rollback.
pub struct GuestResourceManager {
    allocated_pages: [u64; 32],
    allocated_count: usize,
}

impl GuestResourceManager {
    pub const fn new() -> Self {
        Self {
            allocated_pages: [0; 32],
            allocated_count: 0,
        }
    }

    pub fn allocate_page(&mut self) -> Result<u64, AllocatorError> {
        if self.allocated_count >= self.allocated_pages.len() {
            return Err(AllocatorError::Exhausted);
        }
        let page = mm::allocate_page()?;
        let pa = page.value();
        unsafe {
            core::ptr::write_bytes(pa as *mut u8, 0, PAGE_SIZE as usize);
        }
        self.allocated_pages[self.allocated_count] = pa;
        self.allocated_count += 1;
        Ok(pa)
    }

    pub fn allocated_pages(&self) -> &[u64] {
        &self.allocated_pages[..self.allocated_count]
    }

    pub fn allocated_count(&self) -> usize {
        self.allocated_count
    }

    /// Releases all tracked pages in reverse allocation order (LIFO).
    ///
    /// Decrements `allocated_count` only after each page is successfully returned
    /// to the page allocator. If a free operation fails, the error is immediately
    /// returned and the remaining pages remain recorded in `allocated_pages`.
    pub fn rollback(&mut self) -> Result<(), AllocatorError> {
        while self.allocated_count > 0 {
            let last_idx = self.allocated_count - 1;
            let pa = self.allocated_pages[last_idx];
            mm::free_page(PhysAddr::new(pa))?;
            self.allocated_pages[last_idx] = 0;
            self.allocated_count -= 1;
        }
        Ok(())
    }
}

unsafe impl Stage2Allocator for &mut GuestResourceManager {
    fn allocate_table_page(&mut self) -> Result<u64, TranslationError> {
        self.allocate_page()
            .map_err(|_| TranslationError::TableExhausted)
    }
}

pub fn run_phase9_guest_test() {
    println!("Phase 9: Preparing guest execution environment...");

    let initial_stats = mm::allocator_stats().expect("get allocator stats");

    // 1. Processor PARange verification
    let mmfr0 = IdAa64Mmfr0El1::dump().expect("ID_AA64MMFR0_EL1 is available at EL2");
    let parange = mmfr0.parange();

    let mut res_manager = GuestResourceManager::new();
    let mut stage2_active = false;

    // Helper closure to build and run guest environment, rolling back on error
    let setup_result = (|| -> Result<(), ()> {
        // 2. Allocate individual 4 KiB physical backing pages for guest regions
        let payload_pa = res_manager
            .allocate_page()
            .map_err(|_| println!("  [ERR] Failed to allocate payload backing page"))?;
        let scratch_pa = res_manager
            .allocate_page()
            .map_err(|_| println!("  [ERR] Failed to allocate scratch backing page"))?;
        let stack_pa = res_manager
            .allocate_page()
            .map_err(|_| println!("  [ERR] Failed to allocate stack backing page"))?;
        let dtb_pa = res_manager
            .allocate_page()
            .map_err(|_| println!("  [ERR] Failed to allocate DTB backing page"))?;

        println!("  Allocated individual backing pages:");
        println!("    Payload PA: {:#018x}", payload_pa);
        println!("    Scratch PA: {:#018x}", scratch_pa);
        println!("    Stack PA:   {:#018x}", stack_pa);
        println!("    DTB PA:     {:#018x}", dtb_pa);
        println!("    Guard Page: unmapped (IPA {:#018x})", GUEST_GUARD_IPA);

        // 3. Copy test payload into payload backing page
        let payload_start = core::ptr::addr_of!(__payload_start) as usize;
        let payload_end = core::ptr::addr_of!(__payload_end) as usize;
        let payload_len = payload_end.saturating_sub(payload_start);
        assert!(payload_len > 0, "payload must not be empty");
        assert!(
            payload_len <= PAGE_SIZE as usize,
            "payload must fit in one page"
        );

        unsafe {
            core::ptr::copy_nonoverlapping(
                payload_start as *const u8,
                payload_pa as *mut u8,
                payload_len,
            );
        }

        // 4. Generate minimal Guest DTB describing exact backed memory regions
        let guest_mem_regions = [
            GuestMemoryRegion {
                base: GUEST_PAYLOAD_IPA,
                size: 2 * PAGE_SIZE, // covers payload page and scratch page
            },
            GuestMemoryRegion {
                base: GUEST_STACK_IPA,
                size: PAGE_SIZE, // guest stack page
            },
            GuestMemoryRegion {
                base: GUEST_DTB_IPA,
                size: PAGE_SIZE, // guest DTB page
            },
        ];

        let dtb_slice =
            unsafe { core::slice::from_raw_parts_mut(dtb_pa as *mut u8, PAGE_SIZE as usize) };
        let dtb_config = GuestFdtConfig {
            memory_regions: &guest_mem_regions,
            bootargs: None,
        };
        let dtb_size = build_guest_dtb(dtb_slice, &dtb_config)
            .map_err(|_| println!("  [ERR] Failed to generate guest DTB"))?;
        println!(
            "  Generated guest DTB at IPA {:#018x} ({} bytes)",
            GUEST_DTB_IPA, dtb_size
        );

        // 5. Clean Data Cache to PoC for payload and DTB, and invalidate Instruction Cache
        unsafe {
            clean_dcache_poc(
                payload_pa as usize,
                payload_pa as usize + PAGE_SIZE as usize,
            );
            clean_dcache_poc(
                scratch_pa as usize,
                scratch_pa as usize + PAGE_SIZE as usize,
            );
            clean_dcache_poc(stack_pa as usize, stack_pa as usize + PAGE_SIZE as usize);
            clean_dcache_poc(dtb_pa as usize, dtb_pa as usize + PAGE_SIZE as usize);
            invalidate_icache_all();
        }

        // 6. Build Stage-2 translation tables with distinct per-region permissions (4 KiB L3 leaves only).
        // Use the same implemented PA width for software descriptor validation
        // that stage2_register_values() encodes in VTCR_EL2.PS below.
        let pa_bits = pa_bits_from_parange(parange)
            .map_err(|_| println!("  [ERR] Unsupported physical address range"))?;
        let mut stage2_tables = Stage2TableSet::new(&mut res_manager, pa_bits)
            .map_err(|_| println!("  [ERR] Failed to create Stage2TableSet"))?;

        // Code page: ReadOnly, Executable
        stage2_tables
            .map(Stage2Mapping {
                ipa: GUEST_PAYLOAD_IPA,
                pa: payload_pa,
                size: PAGE_SIZE,
                mem_type: Stage2MemoryType::NormalWbWa,
                access: Stage2Access::ReadOnly,
                exec: Stage2Exec::Executable,
            })
            .map_err(|_| println!("  [ERR] Failed to map payload page"))?;

        // Scratch data page: ReadWrite, ExecuteNever
        stage2_tables
            .map(Stage2Mapping {
                ipa: GUEST_SCRATCH_IPA,
                pa: scratch_pa,
                size: PAGE_SIZE,
                mem_type: Stage2MemoryType::NormalWbWa,
                access: Stage2Access::ReadWrite,
                exec: Stage2Exec::ExecuteNever,
            })
            .map_err(|_| println!("  [ERR] Failed to map scratch page"))?;

        // Stack guard page at GUEST_GUARD_IPA (0x4000_2000) is intentionally left UNMAPPED!

        // Stack page: ReadWrite, ExecuteNever
        stage2_tables
            .map(Stage2Mapping {
                ipa: GUEST_STACK_IPA,
                pa: stack_pa,
                size: PAGE_SIZE,
                mem_type: Stage2MemoryType::NormalWbWa,
                access: Stage2Access::ReadWrite,
                exec: Stage2Exec::ExecuteNever,
            })
            .map_err(|_| println!("  [ERR] Failed to map stack page"))?;

        // DTB page: ReadOnly, ExecuteNever
        stage2_tables
            .map(Stage2Mapping {
                ipa: GUEST_DTB_IPA,
                pa: dtb_pa,
                size: PAGE_SIZE,
                mem_type: Stage2MemoryType::NormalWbWa,
                access: Stage2Access::ReadOnly,
                exec: Stage2Exec::ExecuteNever,
            })
            .map_err(|_| println!("  [ERR] Failed to map DTB page"))?;

        let stage2_root_pa = stage2_tables.root_pa();
        let stage2_regs = stage2_register_values(1, stage2_root_pa, parange)
            .map_err(|_| println!("  [ERR] Failed to build stage-2 registers"))?;

        println!(
            "  Stage-2 translation tables initialized: root_pa={:#018x} used_pages={}",
            stage2_root_pa,
            stage2_tables.used_pages()
        );

        // 7. Publish descriptors and activate Stage-2 translation
        unsafe {
            activate_stage2(&stage2_regs);
        }
        stage2_active = true;

        // 8. Initialize vCPU Context
        let mut context = VcpuContext::new(GUEST_PAYLOAD_IPA, GUEST_STACK_TOP_IPA);
        context.x[0] = GUEST_DTB_IPA;
        let mut exit = VcpuExit::default();

        println!("Phase 9: Entering guest EL1 execution loop...");

        // Checkpoint 1 (Guest RAM read/write test)
        println!(
            "  Starting guest execution at IPA {:#018x}...",
            context.elr_el2
        );
        let vector = unsafe { __vcpu_run(&mut context, &mut exit) };
        assert_eq!(vector, 8, "Expected Lower-EL AArch64 synchronous exit");
        let reason = exit.decode_reason(&context);
        println!(
            "  Guest exit 1: ESR_EL2={:#018x} reason={:?}",
            exit.esr_el2, reason
        );
        match reason {
            VcpuExitReason::Hvc { imm: 0, arg0: 1 } => {
                println!("  [OK] Guest Checkpoint 1: RAM read/write verification passed");
            }
            VcpuExitReason::Hvc { imm, arg0 } => {
                panic!(
                    "Guest failure exit at Checkpoint 1: HVC #{} with x0={:#x} x1={:#x}",
                    imm, arg0, context.x[1]
                );
            }
            other => panic!("Unexpected exit at Checkpoint 1: {:?}", other),
        }

        // Checkpoint 2 (System register verification)
        let vector = unsafe { __vcpu_run(&mut context, &mut exit) };
        assert_eq!(vector, 8);
        let reason = exit.decode_reason(&context);
        println!(
            "  Guest exit 2: ESR_EL2={:#018x} reason={:?}",
            exit.esr_el2, reason
        );
        match reason {
            VcpuExitReason::Hvc { imm: 0, arg0: 2 } => {
                println!(
                    "  [OK] Guest Checkpoint 2: System registers verified (CurrentEL=EL1, MPIDR_EL1={:#010x})",
                    context.x[1]
                );
            }
            VcpuExitReason::Hvc { imm, arg0 } => {
                panic!(
                    "Guest failure exit at Checkpoint 2: HVC #{} with x0={:#x} x1={:#x}",
                    imm, arg0, context.x[1]
                );
            }
            other => panic!("Unexpected exit at Checkpoint 2: {:?}", other),
        }

        // Checkpoint 3 (Deliberate Stage-2 Translation Fault on unmapped IPA 0x3000_0000)
        let vector = unsafe { __vcpu_run(&mut context, &mut exit) };
        assert_eq!(vector, 8);
        let reason = exit.decode_reason(&context);
        let fault_ipa = exit.fault_ipa();
        println!(
            "  Guest exit 3: ESR_EL2={:#018x} FAR_EL2={:#018x} HPFAR_EL2={:#018x} fault_ipa={:#018x}",
            exit.esr_el2, exit.far_el2, exit.hpfar_el2, fault_ipa
        );
        match reason {
            VcpuExitReason::Stage2DataAbort {
                ipa,
                is_write,
                dfsc,
            } => {
                assert_eq!(
                    ipa, 0x3000_0000,
                    "Fault IPA must match unmapped 0x3000_0000"
                );
                assert!(!is_write, "Test performed read from unmapped address");
                assert!(
                    (0x04..=0x07).contains(&dfsc),
                    "Expected translation fault DFSC, got {:#x}",
                    dfsc
                );
                println!(
                    "  [OK] Guest Checkpoint 3: Deliberate Stage-2 Data Abort successfully trapped and decoded at IPA {:#018x}",
                    ipa
                );
            }
            VcpuExitReason::Hvc { imm, arg0 } => {
                panic!(
                    "Guest reported failure before Stage-2 abort: HVC #{} with x0={:#x} x1={:#x}",
                    imm, arg0, context.x[1]
                );
            }
            other => panic!("Unexpected exit at Checkpoint 3: {:?}", other),
        }

        Ok(())
    })();

    // 9. Teardown only after activation. A setup error returned through `?`
    // occurs before activate_stage2(), so no inherited virtualization context
    // should be invalidated or overwritten on that path.
    if stage2_active {
        unsafe {
            deactivate_stage2();
        }
    }

    if setup_result.is_err() {
        if let Err(cleanup_err) = res_manager.rollback() {
            panic!(
                "Phase 9 setup failed, and rollback also failed: {:?}",
                cleanup_err
            );
        }
        panic!("Phase 9 guest execution failed during setup");
    }

    // 10. Release guest and Stage-2 table resources only after translation context is cleanly deactivated
    res_manager
        .rollback()
        .expect("rollback guest resources after teardown");

    let final_stats = mm::allocator_stats().expect("get allocator stats after teardown");
    assert_eq!(
        initial_stats.in_use_pages, final_stats.in_use_pages,
        "All guest and stage-2 table pages must be fully released upon teardown"
    );

    println!("============================================================");
    println!("Phase 9 Guest Preparation & Execution Verification: PASSED");
    println!("============================================================");
}
