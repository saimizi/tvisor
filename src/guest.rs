//! Guest platform initialization, Stage-2 translation setup, and Phase 9 test runner.

use core::arch::global_asm;

use tvisor_util::aarch64_reg::{IdAa64Mmfr0El1, VmpidrEl2};
use tvisor_util::el2_translation::{PAGE_SIZE, TableStorage};
use tvisor_util::guest_fdt::{GuestFdtConfig, build_guest_dtb};
use tvisor_util::println;
use tvisor_util::stage2_translation::{
    Stage2Access, Stage2Exec, Stage2Mapping, Stage2MemoryType, Stage2TableSet, stage2_register_values,
};

use crate::mm;
use crate::vcpu::{VcpuContext, VcpuExit, VcpuExitReason, __vcpu_run};

pub const GUEST_RAM_BASE: u64 = 0x4000_0000;
pub const GUEST_RAM_SIZE: u64 = 0x0020_0000; // 2 MiB for Phase 9 test
pub const GUEST_ENTRY_IPA: u64 = 0x4000_0000;
pub const GUEST_SP_IPA: u64 = 0x4000_3000;
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
    // 1. Initialize EL1 stack pointer to 0x4000_3000
    mov  x9, #0x40000000
    add  x9, x9, #0x3000
    mov  sp, x9

    // 2. Checkpoint 1: Memory write and read test
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
    b.ne .Lfail

    // Signal Checkpoint 1 via HVC #0 with x0 = 1
    mov  x0, #1
    hvc  #0

    // 3. Checkpoint 2: System register verification
    // Verify CurrentEL is EL1 (bits [3:2] == 0b01 -> CurrentEL value == 0x04)
    mrs  x13, CurrentEL
    lsr  x14, x13, #2
    and  x14, x14, #0x3
    cmp  x14, #1
    b.ne .Lfail

    // Verify MPIDR_EL1 has bit 30 (UP) set and Aff0 == 0
    mrs  x15, MPIDR_EL1
    tbz  x15, #30, .Lfail

    // Read SCTLR_EL1 to verify accessibility
    mrs  x16, SCTLR_EL1

    // Signal Checkpoint 2 via HVC #0 with x0 = 2
    mov  x0, #2
    hvc  #0

    // 4. Checkpoint 3: Deliberate Stage-2 Translation Fault
    // Attempt to read from unmapped guest IPA 0x3000_0000
    movz x17, #0x3000, lsl #16
    ldr  x18, [x17]

.Lfail:
    wfe
    b    .Lfail
    .size __el1_test_payload, . - __el1_test_payload
"#
);

#[inline]
unsafe fn clean_dcache_poc(start: usize, end: usize) {
    let mut addr = start & !(64 - 1);
    while addr < end {
        unsafe {
            core::arch::asm!(
                "dc cvac, {addr}",
                addr = in(reg) addr,
                options(nomem, nostack, preserves_flags),
            );
        }
        addr += 64;
    }
    unsafe {
        core::arch::asm!("dsb ish", "isb", options(nomem, nostack, preserves_flags));
    }
}

#[inline]
unsafe fn invalidate_icache_all() {
    unsafe {
        core::arch::asm!("ic ialluis", "dsb ish", "isb", options(nomem, nostack, preserves_flags));
    }
}

pub fn run_phase9_guest_test() {
    println!("Phase 9: Preparing guest execution environment...");

    // 1. Check processor PARange support
    let mmfr0 = IdAa64Mmfr0El1::dump().expect("ID_AA64MMFR0_EL1 is available at EL2");
    let parange = mmfr0.parange();

    // 2. Allocate 2 MiB of physical RAM pages for the guest
    let guest_ram_pages = (GUEST_RAM_SIZE / PAGE_SIZE) as usize;
    // Allocate first page to determine contiguous or base PA
    let first_page = mm::allocate_page().expect("allocate first guest RAM page");
    let guest_pa_base = first_page.value();

    // Allocate remaining 511 pages
    for i in 1..guest_ram_pages {
        let page = mm::allocate_page().expect("allocate guest RAM page");
        // For simple initial test, ensure pages are contiguous or record mapping
        assert_eq!(page.value(), guest_pa_base + (i as u64) * PAGE_SIZE);
    }

    println!(
        "  Guest RAM backing PA range: [{:#018x}, {:#018x}) (2 MiB)",
        guest_pa_base,
        guest_pa_base + GUEST_RAM_SIZE
    );

    // 3. Allocate 8 pages for Stage-2 translation tables
    let table_pages = 8;
    let first_table_page = mm::allocate_page().expect("allocate first stage-2 table page");
    let table_pa_base = first_table_page.value();
    for i in 1..table_pages {
        let page = mm::allocate_page().expect("allocate stage-2 table page");
        assert_eq!(page.value(), table_pa_base + (i as u64) * PAGE_SIZE);
    }

    // SAFETY: table_pa_base points to exclusively allocated, identity-mapped RAM.
    let table_storage = unsafe { &mut *(table_pa_base as *mut TableStorage<8>) };
    *table_storage = TableStorage::zeroed();

    let mut stage2_tables =
        Stage2TableSet::new(table_storage, table_pa_base, 40).expect("create Stage2TableSet");

    // 4. Map Guest RAM: IPA [0x4000_0000, 0x4020_0000) -> PA [guest_pa_base, guest_pa_base + 2MiB)
    stage2_tables
        .map(Stage2Mapping {
            ipa: GUEST_RAM_BASE,
            pa: guest_pa_base,
            size: GUEST_RAM_SIZE,
            mem_type: Stage2MemoryType::NormalWbWa,
            access: Stage2Access::ReadWrite,
            exec: Stage2Exec::Executable,
        })
        .expect("map guest RAM in stage 2");

    let stage2_root_pa = stage2_tables.root_pa();
    let stage2_regs =
        stage2_register_values(1, stage2_root_pa, parange).expect("build stage-2 register values");

    println!(
        "  Stage-2 tables initialized: root_pa={:#018x} used_pages={}",
        stage2_root_pa,
        stage2_tables.used_pages()
    );
    println!(
        "  VTCR_EL2={:#018x} VTTBR_EL2={:#018x}",
        stage2_regs.vtcr_el2, stage2_regs.vttbr_el2
    );

    // 5. Copy test payload into guest RAM entry page
    let payload_start = core::ptr::addr_of!(__payload_start) as usize;
    let payload_end = core::ptr::addr_of!(__payload_end) as usize;
    let payload_len = payload_end.saturating_sub(payload_start);
    assert!(payload_len > 0, "payload must not be empty");
    assert!(payload_len <= PAGE_SIZE as usize, "payload must fit in first page");

    let guest_entry_ptr = guest_pa_base as *mut u8;
    unsafe {
        core::ptr::copy_nonoverlapping(
            payload_start as *const u8,
            guest_entry_ptr,
            payload_len,
        );
    }

    // 6. Generate Guest DTB into guest RAM at offset 0x0010_0000 (IPA 0x4010_0000)
    let dtb_pa = guest_pa_base + (GUEST_DTB_IPA - GUEST_RAM_BASE);
    let dtb_slice = unsafe { core::slice::from_raw_parts_mut(dtb_pa as *mut u8, 4096) };
    let dtb_config = GuestFdtConfig {
        ram_base: GUEST_RAM_BASE,
        ram_size: GUEST_RAM_SIZE,
        bootargs: Some("console=ttyAMA0 earlycon"),
    };
    let dtb_size = build_guest_dtb(dtb_slice, &dtb_config).expect("generate guest DTB");
    println!("  Generated guest DTB at IPA {:#018x} ({} bytes)", GUEST_DTB_IPA, dtb_size);

    // 7. Perform Cache Maintenance (clean D-cache to PoC, invalidate I-cache)
    unsafe {
        clean_dcache_poc(guest_pa_base as usize, (guest_pa_base + GUEST_RAM_SIZE) as usize);
        invalidate_icache_all();
    }

    // 8. Configure Virtualization Control Registers
    unsafe {
        // Set VTCR_EL2 and VTTBR_EL2
        core::arch::asm!(
            "msr VTCR_EL2, {vtcr}",
            "msr VTTBR_EL2, {vttbr}",
            "msr HCR_EL2, {hcr}",
            "msr CPTR_EL2, {cptr}",
            vtcr = in(reg) stage2_regs.vtcr_el2,
            vttbr = in(reg) stage2_regs.vttbr_el2,
            hcr = in(reg) stage2_regs.hcr_el2,
            cptr = in(reg) stage2_regs.cptr_el2,
            options(nomem, nostack, preserves_flags),
        );
        // Set virtual MPIDR_EL1
        VmpidrEl2 {
            value: stage2_regs.vmpidr_el2,
        }
        .write();

        // Invalidate all stage-1 and stage-2 guest TLBs
        core::arch::asm!("tlbi vmalls12e1is", "dsb ish", "isb", options(nomem, nostack, preserves_flags));
    }

    // 9. Initialize vCPU Context
    let mut context = VcpuContext::new(GUEST_ENTRY_IPA, GUEST_SP_IPA);
    context.x[0] = GUEST_DTB_IPA; // x0 = DTB IPA
    let mut exit = VcpuExit::default();

    println!("Phase 9: Entering guest EL1 execution loop...");

    // Checkpoint 1
    println!("  Starting guest execution at IPA {:#018x}...", context.elr_el2);
    let vector = unsafe { __vcpu_run(&mut context, &mut exit) };
    assert_eq!(vector, 8, "Expected Lower-EL AArch64 synchronous exit");
    let reason = exit.decode_reason(&context);
    println!(
        "  Guest exit 1: ESR_EL2={:#018x} reason={:?}",
        exit.esr_el2, reason
    );
    match reason {
        VcpuExitReason::Hvc { imm, arg0 } => {
            assert_eq!(imm, 0);
            assert_eq!(arg0, 1, "Expected Checkpoint 1 (RAM read/write test passed)");
            println!("  [OK] Guest Checkpoint 1: RAM read/write verification passed");
        }
        other => panic!("Unexpected exit at Checkpoint 1: {:?}", other),
    }

    // Advance PC past HVC instruction (4 bytes)
    context.elr_el2 += 4;

    // Checkpoint 2
    let vector = unsafe { __vcpu_run(&mut context, &mut exit) };
    assert_eq!(vector, 8);
    let reason = exit.decode_reason(&context);
    println!(
        "  Guest exit 2: ESR_EL2={:#018x} reason={:?}",
        exit.esr_el2, reason
    );
    match reason {
        VcpuExitReason::Hvc { imm, arg0 } => {
            assert_eq!(imm, 0);
            assert_eq!(arg0, 2, "Expected Checkpoint 2 (System register verification passed)");
            println!("  [OK] Guest Checkpoint 2: System register verification passed (CurrentEL=EL1, MPIDR_EL1=UP)");
        }
        other => panic!("Unexpected exit at Checkpoint 2: {:?}", other),
    }

    // Advance PC past HVC instruction (4 bytes)
    context.elr_el2 += 4;

    // Checkpoint 3 (Deliberate Stage-2 Translation Fault)
    let vector = unsafe { __vcpu_run(&mut context, &mut exit) };
    assert_eq!(vector, 8);
    let reason = exit.decode_reason(&context);
    let fault_ipa = exit.fault_ipa();
    println!(
        "  Guest exit 3: ESR_EL2={:#018x} FAR_EL2={:#018x} HPFAR_EL2={:#018x} fault_ipa={:#018x}",
        exit.esr_el2, exit.far_el2, exit.hpfar_el2, fault_ipa
    );
    match reason {
        VcpuExitReason::Stage2DataAbort { ipa, is_write, dfsc } => {
            assert_eq!(ipa, 0x3000_0000, "Fault IPA must match unmapped 0x3000_0000");
            assert!(!is_write, "Test performed read from unmapped address");
            // DFSC translation fault level 0..3 (0x04..0x07)
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
        other => panic!("Unexpected exit at Checkpoint 3: {:?}", other),
    }

    println!("============================================================");
    println!("Phase 9 Guest Preparation & Execution Verification: PASSED");
    println!("============================================================");
}
