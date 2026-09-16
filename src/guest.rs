//! Guest platform initialization, Stage-2 translation setup, and Linux launch.

use core::fmt;
use spin::Once;

use crate::vmctl::*;
use tvisor_util::aarch64_reg::{IdAa64Mmfr0El1, VmpidrEl2};
use tvisor_util::el2_translation::TranslationError;
use tvisor_util::guest_fdt::{
    GuestFdtConfig, GuestGicV2, GuestMemoryRegion, GuestPl011, build_guest_dtb,
};
use tvisor_util::guest_platform::{
    self, GUEST_GICD, GUEST_GICV, GUEST_PL011, GUEST_PL011_CLOCK_HZ, GUEST_RAM, VIRTUAL_PL011_IRQ,
};
use tvisor_util::linux_boot::LinuxBootLayout;
use tvisor_util::stage2_translation::{
    Stage2Access, Stage2Exec, Stage2MemoryType, Stage2RegisterValues, stage2_register_values,
};
use tvisor_util::{PAGE_SIZE, println};

use crate::mm;
use crate::vcpu::{__vcpu_run, Vcpu, VcpuExitReason};
use tvisor_util::gicv2;
use tvisor_util::mmio::MmioDispatcher;
use tvisor_util::system_info::PhysRegion;
use tvisor_util::virtual_timer::VIRTUAL_TIMER_PPI;

static LINUX_IMAGE_SOURCE: Once<PhysRegion> = Once::new();

/// Why Linux execution returned to EL2 instead of continuing its normal run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestRunError {
    Translation(TranslationError),
    Exited {
        vector: u64,
        esr_el2: u64,
        reason: VcpuExitReason,
    },
}

impl From<TranslationError> for GuestRunError {
    fn from(error: TranslationError) -> Self {
        Self::Translation(error)
    }
}

impl fmt::Display for GuestRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Translation(error) => write!(f, "guest translation setup failed: {error}"),
            Self::Exited {
                vector,
                esr_el2,
                reason,
            } => write!(
                f,
                "Linux guest exited at EL2 vector {vector} (ESR_EL2={esr_el2:#018x}): {reason:?}"
            ),
        }
    }
}

/// Records the U-Boot-loaded Linux Image source before tvisor replaces the
/// inherited EL2 translation regime.
pub fn set_linux_image_source(source: PhysRegion) -> Result<(), ()> {
    if let Some(existing) = LINUX_IMAGE_SOURCE.get() {
        return if *existing == source { Ok(()) } else { Err(()) };
    }
    LINUX_IMAGE_SOURCE.call_once(|| source);
    Ok(())
}

pub fn linux_image_source() -> Option<PhysRegion> {
    LINUX_IMAGE_SOURCE.get().copied()
}

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

/// Runs a vCPU until a non-emulated exit. Trapped virtual-device stage-2
/// aborts are completed and resumed here, keeping host devices host-owned.
fn run_vcpu_with_mmio(vcpu: &mut Vcpu, dispatcher: &mut MmioDispatcher, gic: &gicv2::GicV2) -> u64 {
    loop {
        // The guest accesses GICV directly; save/restore GICH state around
        // every world switch so its List Register and CPU-interface policy
        // remain owned by this vCPU rather than the host.
        unsafe { gic.restore_virtual_cpu(vcpu.gic()) };
        let vector = unsafe { __vcpu_run(vcpu) };
        unsafe { gic.save_virtual_cpu(vcpu.gic_mut()) };
        if vector == 9 {
            let irq = unsafe { gic.acknowledge() };
            if gicv2::is_timer_ppi(irq, VIRTUAL_TIMER_PPI) {
                vcpu.timer_mut().mark_pending_from_irq();
                gicv2::queue_timer_ppi(vcpu.gic_mut(), irq)
                    .expect("physical timer PPI arrived while its virtual LR remained active");
                vcpu.timer_mut().clear_pending_after_list_register();
                // EOImodeNS is set: this only drops priority. The physical
                // PPI remains active until the guest GICV_EOIR completes LR0.
                unsafe { gic.end_interrupt(irq) };
                println!("  EL2 queued virtual timer PPI {} in GICH LR0", irq);
                continue;
            }
            if irq != gicv2::SPURIOUS_IRQ {
                unsafe { gic.end_interrupt(irq) };
            }
            println!("Unexpected physical IRQ {} while guest ran", irq);
            return vector;
        }
        if vector != 8 {
            return vector;
        }

        let reason = vcpu.exit().decode_reason(vcpu.context());
        let VcpuExitReason::Stage2DataAbort { ipa, .. } = reason else {
            return vector;
        };
        let is_trapped_mmio = (GUEST_GICD.start()
            ..GUEST_GICD.end().expect("validated guest platform"))
            .contains(&ipa)
            || (GUEST_PL011.start()..GUEST_PL011.end().expect("validated guest platform"))
                .contains(&ipa);
        if !is_trapped_mmio {
            return vector;
        }

        let exit = *vcpu.exit();
        let transmit = vcpu
            .context_mut()
            .emulate_stage2_mmio(&exit, dispatcher)
            .unwrap_or_else(|error| panic!("Virtual MMIO emulation failed: {error}"));
        if let Some(byte) = transmit {
            tvisor_util::debug_util::write_byte(byte)
                .unwrap_or_else(|_| panic!("Virtual PL011 transmit could not reach host console"));
        }
    }
}

fn run_linux_guest_inner(
    vm_ctl: &mut VmCtl,
    stage2_active: &mut bool,
) -> Result<(), GuestRunError> {
    let source = linux_image_source().ok_or(TranslationError::Unexpected)?;
    let image = unsafe {
        core::slice::from_raw_parts(source.start().value() as *const u8, source.size() as usize)
    };
    let layout = LinuxBootLayout::default_for_image(image, None)
        .map_err(|_| TranslationError::Unexpected)?;
    let (image_ipa, image_extent) = layout.image();
    let (dtb_ipa, dtb_capacity) = layout.dtb();

    println!(
        "Phase 10: loading Linux Image: source={} bytes={} entry={:#018x} extent={} DTB={:#018x}",
        source,
        image.len(),
        image_ipa,
        image_extent,
        dtb_ipa,
    );
    let gic = gicv2::global().expect("GICv2 must be discovered before guest preparation");
    unsafe { gic.enable_timer_ppi(VIRTUAL_TIMER_PPI) };
    guest_platform::validate().map_err(|_| TranslationError::Unexpected)?;
    let gicv_mapping =
        guest_platform::map_device_into_window(GUEST_GICV, gic.info().virtual_cpu_interface())
            .map_err(|_| TranslationError::Unexpected)?;

    let guest_ram_pa = vm_ctl
        .vm_mem_alloc(
            VmMemUsage::GuestRam,
            Some(IpaAddr::new(GUEST_RAM.start())),
            GUEST_RAM.size() as usize,
        )
        .map_err(|_| TranslationError::Unexpected)?
        .ok_or(TranslationError::Unexpected)?
        .value();
    let guest_pa_for = |ipa: u64| -> Result<u64, TranslationError> {
        guest_ram_pa
            .checked_add(
                ipa.checked_sub(GUEST_RAM.start())
                    .ok_or(TranslationError::Unexpected)?,
            )
            .ok_or(TranslationError::AddressOverflow)
    };
    let image_pa = guest_pa_for(image_ipa)?;
    let dtb_pa = guest_pa_for(dtb_ipa)?;

    // The U-Boot source contains only initialized bytes. The Image header's
    // extent includes the zero-initialized tail expected by the kernel.
    unsafe {
        core::ptr::copy_nonoverlapping(image.as_ptr(), image_pa as *mut u8, image.len());
        core::ptr::write_bytes(
            (image_pa + image.len() as u64) as *mut u8,
            0,
            image_extent as usize - image.len(),
        );
    }

    let guest_mem_regions = [GuestMemoryRegion {
        base: GUEST_RAM.start(),
        size: GUEST_RAM.size(),
    }];
    let dtb_slice =
        unsafe { core::slice::from_raw_parts_mut(dtb_pa as *mut u8, dtb_capacity as usize) };
    let dtb_real_size = build_guest_dtb(
        dtb_slice,
        &GuestFdtConfig {
            memory_regions: &guest_mem_regions,
            bootargs: Some("console=ttyAMA0,115200 earlycon=pl011,mmio32,0x09000000 loglevel=8"),
            pl011: Some(GuestPl011 {
                base: GUEST_PL011.start(),
                size: GUEST_PL011.size(),
                clock_hz: GUEST_PL011_CLOCK_HZ,
                interrupt: VIRTUAL_PL011_IRQ,
            }),
            gicv2: Some(GuestGicV2 {
                distributor_base: GUEST_GICD.start(),
                distributor_size: GUEST_GICD.size(),
                cpu_interface_base: gicv_mapping.device_ipa,
                cpu_interface_size: gicv_mapping.device.size(),
            }),
        },
    )
    .map_err(|_| TranslationError::Unexpected)?;

    unsafe {
        clean_dcache_poc(image_pa as usize, (image_pa + image_extent) as usize);
        clean_dcache_poc(dtb_pa as usize, (dtb_pa + dtb_real_size as u64) as usize);
        invalidate_icache_all();
    }

    vm_ctl
        .vm_mem_alloc(VmMemUsage::PageTable, None, PAGE_SIZE)
        .map_err(|_| TranslationError::Unexpected)?;
    vm_ctl.map(
        VmMemUsage::GuestRam,
        Stage2MemoryType::NormalWbWa,
        Stage2Access::ReadWrite,
        Stage2Exec::Executable,
    )?;
    vm_ctl.map_external_device(
        IpaAddr::new(GUEST_GICV.start()),
        gicv_mapping.mapped_pa,
        gicv_mapping.mapping_size,
    )?;

    let pa_range = IdAa64Mmfr0El1::dump().unwrap().pa_range();
    let stage2_root_pa = vm_ctl.page_table_root().unwrap().value();
    let stage2_regs = stage2_register_values(vm_ctl.vm_id(), stage2_root_pa, pa_range)?;
    unsafe { activate_stage2(&stage2_regs) };
    *stage2_active = true;

    let registers = layout.initial_registers();
    let vcpu_id = vm_ctl.add_vcpu(Vcpu::new_linux(registers.pc, registers.x0));
    let vcpu = vm_ctl
        .vcpu_mut(vcpu_id)
        .expect("new Linux vCPU must be present");
    println!(
        "Phase 10: entering Linux at EL1: PC={:#018x} x0={:#018x}",
        vcpu.context().elr_el2,
        vcpu.context().x[0]
    );

    let mut mmio_dispatcher = MmioDispatcher::default();
    let vector = run_vcpu_with_mmio(vcpu, &mut mmio_dispatcher, gic);
    Err(GuestRunError::Exited {
        vector,
        esr_el2: vcpu.exit().esr_el2,
        reason: vcpu.exit().decode_reason(vcpu.context()),
    })
}

pub fn run_guest() -> Result<(), GuestRunError> {
    println!("Phase 10: Preparing Linux guest execution environment...");
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

        run_linux_guest_inner(&mut vm_ctl, &mut *guard.active)?;
    }

    vm_ctl.release_all();

    let final_stats = mm::allocator_stats().expect("get allocator stats after teardown");
    assert_eq!(
        initial_stats.in_use_pages, final_stats.in_use_pages,
        "All guest and stage-2 table pages must be fully released upon teardown"
    );

    println!("============================================================");
    println!("Phase 10 Linux guest returned cleanly");
    println!("============================================================");

    Ok(())
}
