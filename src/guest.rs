//! Guest platform initialization, Stage-2 translation setup, and Phase 9 test runner.

use core::arch::global_asm;

use crate::vmctl::*;
use tvisor_util::aarch64_reg::{IdAa64Mmfr0El1, VmpidrEl2};
use tvisor_util::el2_translation::TranslationError;
use tvisor_util::guest_fdt::{GuestFdtConfig, GuestMemoryRegion, build_guest_dtb};
use tvisor_util::stage2_translation::{
    Stage2Access, Stage2Exec, Stage2MemoryType, Stage2RegisterValues, stage2_register_values,
};
use tvisor_util::{PAGE_SIZE, println};

use crate::mm;
use crate::vcpu::{__vcpu_run, VcpuContext, VcpuExit, VcpuExitReason};

pub const GUEST_PAYLOAD_IPA: u64 = 0x4000_0000;
pub const GUEST_SCRATCH_IPA: u64 = 0x4000_1000;
pub const GUEST_GUARD_IPA: u64 = 0x4000_2000;
pub const GUEST_STACK_IPA: u64 = 0x4000_3000;
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

fn run_guest_inner(vm_ctl: &mut VmCtl, stage2_active: &mut bool) -> Result<(), TranslationError> {
    println!("Phase 9: Preparing guest execution environment...");

    let mut alloc_ipa_pa = |usage: VmMemUsage,
                            ipa: IpaAddr,
                            size: usize|
     -> Result<(u64, u64, usize), TranslationError> {
        let pa = vm_ctl
            .vm_mem_alloc(usage, Some(ipa), size)
            .map_err(|_| TranslationError::Unexpected)?
            .ok_or(TranslationError::Unexpected)?;

        Ok((pa.value(), ipa.value(), size))
    };

    // 2. Allocate individual 4 KiB physical backing pages for guest regions
    let (payload_pa, payload_ipa, payload_size) = alloc_ipa_pa(
        VmMemUsage::Image,
        IpaAddr::new(GUEST_PAYLOAD_IPA),
        PAGE_SIZE,
    )?;

    let (scratch_pa, scratch_ipa, scratch_size) = alloc_ipa_pa(
        VmMemUsage::Scratch,
        IpaAddr::new(GUEST_SCRATCH_IPA),
        PAGE_SIZE,
    )?;

    let (stack_pa, stack_ipa, stack_size) =
        alloc_ipa_pa(VmMemUsage::Stack, IpaAddr::new(GUEST_STACK_IPA), PAGE_SIZE)?;

    let (dtb_pa, dtb_ipa, dtb_size) =
        alloc_ipa_pa(VmMemUsage::Dtb, IpaAddr::new(GUEST_DTB_IPA), PAGE_SIZE)?;

    vm_ctl
        .vm_mem_alloc(
            VmMemUsage::Guard,
            Some(IpaAddr::new(GUEST_GUARD_IPA)),
            PAGE_SIZE,
        )
        .map_err(|_| TranslationError::Unexpected)?;

    println!("{}", vm_ctl);

    // 3. Copy test payload into payload backing page
    let (payload_start, payload_end) = {
        (
            core::ptr::addr_of!(__payload_start) as usize,
            core::ptr::addr_of!(__payload_end) as usize,
        )
    };
    let payload_len = payload_end.saturating_sub(payload_start);
    assert!(payload_len > 0, "payload must not be empty");
    assert!(payload_len <= PAGE_SIZE, "payload must fit in one page");

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
            base: payload_ipa,
            size: payload_size as u64,
        },
        GuestMemoryRegion {
            base: scratch_ipa,
            size: scratch_size as u64,
        },
        GuestMemoryRegion {
            base: stack_ipa,
            size: stack_size as u64,
        },
        GuestMemoryRegion {
            base: dtb_ipa,
            size: dtb_size as u64,
        },
    ];

    let dtb_slice = unsafe { core::slice::from_raw_parts_mut(dtb_pa as *mut u8, dtb_size) };

    let dtb_config = GuestFdtConfig {
        memory_regions: &guest_mem_regions,
        bootargs: None,
    };
    let dtb_real_size =
        build_guest_dtb(dtb_slice, &dtb_config).map_err(|_| TranslationError::Unexpected)?;

    println!(
        "  Generated guest DTB at IPA {} ({} bytes)",
        dtb_ipa, dtb_real_size
    );

    // 5. Clean Data Cache to PoC for payload and DTB, and invalidate Instruction Cache
    // Note: for EL2 stage1, PA=VA
    unsafe {
        clean_dcache_poc(payload_pa as usize, payload_pa as usize + payload_size);
        clean_dcache_poc(scratch_pa as usize, scratch_pa as usize + scratch_size);
        clean_dcache_poc(stack_pa as usize, stack_pa as usize + stack_size);
        clean_dcache_poc(dtb_pa as usize, dtb_pa as usize + dtb_real_size);
        invalidate_icache_all();
    }

    // 6. Build Stage-2 translation tables with distinct per-region permissions (4 KiB L3 leaves only).
    // Use the same implemented PA width for software descriptor validation
    // that stage2_register_values() encodes in VTCR_EL2.PS below.
    vm_ctl
        .vm_mem_alloc(VmMemUsage::PageTable, None, PAGE_SIZE)
        .map_err(|_| TranslationError::Unexpected)?;

    // Code page: ReadOnly, Executable
    vm_ctl.map(
        VmMemUsage::Image,
        Stage2MemoryType::NormalWbWa,
        Stage2Access::ReadOnly,
        Stage2Exec::Executable,
    )?;

    // Scratch data page: ReadWrite, ExecuteNever
    vm_ctl.map(
        VmMemUsage::Scratch,
        Stage2MemoryType::NormalWbWa,
        Stage2Access::ReadWrite,
        Stage2Exec::ExecuteNever,
    )?;

    // Stack guard page at GUEST_GUARD_IPA (0x4000_2000) is intentionally left UNMAPPED!
    // Stack page: ReadWrite, ExecuteNever
    vm_ctl.map(
        VmMemUsage::Stack,
        Stage2MemoryType::NormalWbWa,
        Stage2Access::ReadWrite,
        Stage2Exec::ExecuteNever,
    )?;

    // DTB page: ReadOnly, ExecuteNever
    vm_ctl.map(
        VmMemUsage::Dtb,
        Stage2MemoryType::NormalWbWa,
        Stage2Access::ReadOnly,
        Stage2Exec::ExecuteNever,
    )?;

    let pa_range = IdAa64Mmfr0El1::dump().unwrap().pa_range();
    let stage2_root_pa = vm_ctl.page_table_root().unwrap().value();
    let stage2_regs = stage2_register_values(vm_ctl.vm_id(), stage2_root_pa, pa_range)?;

    let used_pages = vm_ctl
        .entry_pa(VmMemUsage::PageTable)
        .map(|v| v.size() / PAGE_SIZE as u64)
        .sum::<u64>();

    println!(
        "  Stage-2 translation tables initialized: root_pa={:#018x} used_pages={}",
        stage2_root_pa, used_pages,
    );

    // 7. Publish descriptors and activate Stage-2 translation
    unsafe {
        activate_stage2(&stage2_regs);
    }
    *stage2_active = true;

    // 8. Initialize vCPU Context
    // Stack grows from high to low, so initial stack pointer is set to the stack_ipa + stack_size
    let mut context = VcpuContext::new(payload_ipa, stack_ipa + stack_size as u64);
    context.x[0] = dtb_ipa;
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
}

pub fn run_guest() -> Result<(), TranslationError> {
    println!("Phase 9: Preparing guest execution environment...");
    let initial_stats = mm::allocator_stats().expect("get allocator stats");

    let mut vm_ctl = VmCtl::new(1);
    let mut stage2_active = false;

    {
        struct Stage2DeactivationGuard<'a> {
            active: &'a mut bool,
        }

        impl Drop for Stage2DeactivationGuard<'_> {
            fn drop(&mut self) {
                if *self.active {
                    unsafe {
                        deactivate_stage2();
                    }
                    *self.active = false;
                }
            }
        }

        let guard = Stage2DeactivationGuard {
            active: &mut stage2_active,
        };

        run_guest_inner(&mut vm_ctl, &mut *guard.active)?;
    }

    vm_ctl.release_all();

    let final_stats = mm::allocator_stats().expect("get allocator stats after teardown");
    assert_eq!(
        initial_stats.in_use_pages, final_stats.in_use_pages,
        "All guest and stage-2 table pages must be fully released upon teardown"
    );

    println!("============================================================");
    println!("Phase 9 Guest Preparation & Execution Verification: PASSED");
    println!("============================================================");

    Ok(())
}
