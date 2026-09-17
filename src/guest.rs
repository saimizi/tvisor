//! Guest platform initialization, Stage-2 translation setup, and Linux launch.

use core::fmt;
use spin::Once;

use crate::vmctl::*;
use tvisor_util::aarch64_reg::VmpidrEl2;
use tvisor_util::el2_translation::TranslationError;
use tvisor_util::guest_platform::{GUEST_GICD, GUEST_PL011, VIRTUAL_PL011_IRQ};
use tvisor_util::println;
use tvisor_util::stage2_translation::Stage2RegisterValues;

use crate::mm;
use crate::vcpu::{__vcpu_run, Vcpu, VcpuExitReason};
use tvisor_util::gicv2;
use tvisor_util::mmio::MmioDispatcher;
use tvisor_util::system_info::PhysRegion;
use tvisor_util::virtual_timer::VIRTUAL_TIMER_PPI;

static LINUX_IMAGE_SOURCE: Once<PhysRegion> = Once::new();
static HOST_CONSOLE_IRQ: Once<u32> = Once::new();

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

/// Records the physical Mini UART SPI discovered from the host DTB.
pub fn set_host_console_irq(irq: u32) -> Result<(), ()> {
    if !(32..gicv2::SPURIOUS_IRQ).contains(&irq) {
        return Err(());
    }
    if let Some(existing) = HOST_CONSOLE_IRQ.get() {
        return if *existing == irq { Ok(()) } else { Err(()) };
    }
    HOST_CONSOLE_IRQ.call_once(|| irq);
    Ok(())
}

pub fn get_host_console_irq() -> u32 {
    *HOST_CONSOLE_IRQ.get().expect("HOST_CONSOLE_IRQ is not set")
}

#[inline]
pub unsafe fn dcache_line_size() -> usize {
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
pub unsafe fn clean_dcache_poc(start: usize, end: usize) {
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
pub unsafe fn invalidate_icache_all() {
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
pub unsafe fn activate_stage2(regs: &Stage2RegisterValues) {
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
pub fn run_vcpu_with_mmio(
    vcpu: &mut Vcpu,
    dispatcher: &mut MmioDispatcher,
    gic: &gicv2::GicV2,
) -> u64 {
    let host_console_irq = *HOST_CONSOLE_IRQ
        .get()
        .expect("host console IRQ must be configured before running a guest");
    loop {
        // An RX IRQ drains the host-owned Mini UART into this FIFO. Recheck
        // the virtual interrupt after every EL2 exit, because Linux can
        // enable PL011 RXIM in the same exit that emulates its IMSC write.
        if dispatcher.pl011_rx_irq_pending() && !vcpu.gic().uart_in_flight() {
            let _ = gicv2::queue_uart_spi(vcpu.gic_mut(), VIRTUAL_PL011_IRQ);
        }
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
                continue;
            }
            if irq == host_console_irq {
                // Drain before EOI: the Mini UART RX source is level-triggered
                // and would otherwise immediately reassert at the GIC.
                while let Some(byte) = tvisor_util::debug_util::read_byte() {
                    let _ = dispatcher.enqueue_pl011_rx(byte);
                }
                unsafe { gic.end_interrupt(irq) };
                unsafe { gic.deactivate_interrupt(irq) };
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

pub fn run_guest() -> Result<(), GuestRunError> {
    println!("Phase 10: Preparing Linux guest execution environment...");
    let initial_stats = mm::allocator_stats().expect("get allocator stats");
    let mut vm_ctl = VmCtl::new(1);
    let ret = vm_ctl.run();

    // Disable stage2 after all guest VMs are released.
    unsafe {
        deactivate_stage2();
    }

    vm_ctl.release_all();

    let final_stats = mm::allocator_stats().expect("get allocator stats after teardown");
    assert_eq!(
        initial_stats.in_use_pages, final_stats.in_use_pages,
        "All guest and stage-2 table pages must be fully released upon teardown"
    );

    ret
}
