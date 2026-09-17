//! GICv2 architecture definitions and DTB-discovered driver state.

use crate::system_info::PhysRegion;
use spin::Once;

pub const SPURIOUS_IRQ: u32 = 1023;

/// Physical GICv2 regions, in the DTB `reg` order: GICD, GICC, GICH, GICV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GicV2Info {
    distributor: PhysRegion,
    cpu_interface: PhysRegion,
    hypervisor_interface: PhysRegion,
    virtual_cpu_interface: PhysRegion,
}

impl GicV2Info {
    pub fn new(
        distributor: PhysRegion,
        cpu_interface: PhysRegion,
        hypervisor_interface: PhysRegion,
        virtual_cpu_interface: PhysRegion,
    ) -> Option<Self> {
        if distributor.size() < REQUIRED_GICD_SIZE
            || cpu_interface.size() < REQUIRED_GICC_SIZE
            || hypervisor_interface.size() < REQUIRED_GICH_SIZE
            || virtual_cpu_interface.size() < REQUIRED_GICV_SIZE
        {
            None
        } else {
            Some(Self {
                distributor,
                cpu_interface,
                hypervisor_interface,
                virtual_cpu_interface,
            })
        }
    }

    pub const fn distributor(self) -> PhysRegion {
        self.distributor
    }

    pub const fn cpu_interface(self) -> PhysRegion {
        self.cpu_interface
    }

    pub const fn hypervisor_interface(self) -> PhysRegion {
        self.hypervisor_interface
    }

    pub const fn virtual_cpu_interface(self) -> PhysRegion {
        self.virtual_cpu_interface
    }
}

/// Instance-oriented access to the host GICv2 virtualization interfaces.
#[derive(Debug, Clone, Copy)]
pub struct GicV2 {
    info: GicV2Info,
}

static GLOBAL_GIC: Once<GicV2> = Once::new();

impl GicV2 {
    pub const fn new(info: GicV2Info) -> Self {
        Self { info }
    }

    pub const fn info(&self) -> GicV2Info {
        self.info
    }

    const fn distributor_base(&self) -> usize {
        self.info.distributor.start().value() as usize
    }

    const fn cpu_interface_base(&self) -> usize {
        self.info.cpu_interface.start().value() as usize
    }

    const fn hypervisor_interface_base(&self) -> usize {
        self.info.hypervisor_interface.start().value() as usize
    }

    pub const fn gicd_ctlr(&self) -> usize {
        self.distributor_base() + GICD_CTLR
    }

    pub const fn gicc_pmr(&self) -> usize {
        self.cpu_interface_base() + GICC_PMR
    }

    pub const fn gicc_ctlr(&self) -> usize {
        self.cpu_interface_base() + GICC_CTLR
    }

    pub const fn gicd_isenabler(&self) -> usize {
        self.distributor_base() + GICD_ISENABLER
    }

    const fn gicd_igroupr(&self) -> usize {
        self.distributor_base() + GICD_IGROUPR
    }

    const fn gicd_ipriorityr(&self) -> usize {
        self.distributor_base() + GICD_IPRIORITYR
    }

    const fn gicd_itargetsr(&self) -> usize {
        self.distributor_base() + GICD_ITARGETSR
    }

    const fn gicd_icfgr(&self) -> usize {
        self.distributor_base() + GICD_ICFGR
    }

    pub const fn gich_vmcr(&self) -> usize {
        self.hypervisor_interface_base() + GICH_VMCR
    }

    pub const fn gich_lr0(&self) -> usize {
        self.hypervisor_interface_base() + GICH_LR0
    }

    pub const fn gich_lr1(&self) -> usize {
        self.hypervisor_interface_base() + GICH_LR1
    }

    pub const fn gich_hcr(&self) -> usize {
        self.hypervisor_interface_base() + GICH_HCR
    }

    pub const fn gicc_iar(&self) -> usize {
        self.cpu_interface_base() + GICC_IAR
    }

    pub const fn gicc_eoir(&self) -> usize {
        self.cpu_interface_base() + GICC_EOIR
    }

    pub const fn gicc_dir(&self) -> usize {
        self.cpu_interface_base() + GICC_DIR
    }

    /// Enables a banked PPI from the GICv2 Non-secure register view.
    /// The platform handoff must classify the timer PPI as Non-secure Group 1.
    pub unsafe fn enable_timer_ppi(&self, ppi: u32) {
        debug_assert!(ppi < 32);
        unsafe {
            write32(
                self.gicd_ctlr(),
                read32(self.gicd_ctlr()) | GICD_CTLR_ENABLE_GRP1_NS,
            );
            write32(self.gicc_pmr(), 0xff);
            write32(
                self.gicc_ctlr(),
                read32(self.gicc_ctlr()) | GICC_CTLR_ENABLE_GRP1_NS | GICC_CTLR_EOIMODE_NS,
            );
            write32(self.gicd_isenabler(), 1 << ppi);
        }
    }

    /// Enables a Non-secure Group 1 shared peripheral interrupt on CPU0.
    /// This is used for host-owned EL2 devices; it is never exposed directly
    /// to a guest.
    pub unsafe fn enable_spi(&self, spi: u32) {
        debug_assert!((32..SPURIOUS_IRQ).contains(&spi));
        let word_offset = usize::try_from((spi / 32) * 4).unwrap();
        let bit = 1_u32 << (spi % 32);
        let config_word_offset = usize::try_from((spi / 16) * 4).unwrap();
        let edge_bit = 1_u32 << ((spi % 16) * 2 + 1);
        unsafe {
            write32(
                self.gicd_ctlr(),
                read32(self.gicd_ctlr()) | GICD_CTLR_ENABLE_GRP1_NS,
            );
            write32(self.gicc_pmr(), 0xff);
            write32(
                self.gicc_ctlr(),
                read32(self.gicc_ctlr()) | GICC_CTLR_ENABLE_GRP1_NS | GICC_CTLR_EOIMODE_NS,
            );
            write32(
                self.gicd_igroupr() + word_offset,
                read32(self.gicd_igroupr() + word_offset) | bit,
            );
            write8(self.gicd_ipriorityr() + spi as usize, 0x80);
            write8(self.gicd_itargetsr() + spi as usize, 0x01);
            // The DTB describes the Mini UART as level-high. Clear the
            // trigger-type bit explicitly instead of relying on U-Boot's
            // inherited distributor configuration.
            write32(
                self.gicd_icfgr() + config_word_offset,
                read32(self.gicd_icfgr() + config_word_offset) & !edge_bit,
            );
            write32(self.gicd_isenabler() + word_offset, bit);
        }
    }

    pub unsafe fn restore_virtual_cpu(&self, state: &VirtualGicV2State) {
        unsafe {
            write32(self.gich_vmcr(), state.vmcr);
            write32(self.gich_lr0(), state.timer_lr);
            write32(self.gich_lr1(), state.uart_lr);
            write32(self.gich_hcr(), GICH_HCR_ENABLE);
        }
    }

    pub unsafe fn save_virtual_cpu(&self, state: &mut VirtualGicV2State) {
        unsafe {
            state.vmcr = read32(self.gich_vmcr());
            state.timer_lr = read32(self.gich_lr0());
            state.uart_lr = read32(self.gich_lr1());
            write32(self.gich_hcr(), 0);
        }
    }

    pub unsafe fn acknowledge(&self) -> u32 {
        unsafe { read32(self.gicc_iar()) & GICH_LR_INTID_MASK }
    }

    /// Drops priority only; guest GICV_EOIR deactivates the hardware LR.
    pub unsafe fn end_interrupt(&self, id: u32) {
        unsafe { write32(self.gicc_eoir(), id) }
    }

    /// Completes a host-owned interrupt after `end_interrupt`. EOImodeNS is
    /// enabled, so EOIR only drops priority; physical sources which are not
    /// represented by a hardware List Register must be deactivated here.
    pub unsafe fn deactivate_interrupt(&self, id: u32) {
        unsafe { write32(self.gicc_dir(), id) }
    }
}

pub fn initialize(info: GicV2Info) -> &'static GicV2 {
    GLOBAL_GIC.call_once(|| GicV2::new(info))
}

pub fn global() -> Option<&'static GicV2> {
    GLOBAL_GIC.get()
}

// Covers GICD_ITARGETSR31, needed to route any valid SPI to CPU0.
pub const REQUIRED_GICD_SIZE: u64 = 0xc80;
pub const REQUIRED_GICC_SIZE: u64 = 0x1004; // GICC_DIR at 0x1000
pub const REQUIRED_GICH_SIZE: u64 = 0x104; // GICH_LR0 at 0x100
pub const REQUIRED_GICV_SIZE: u64 = 0x1004; // GICV_DIR at 0x1000, if needed

const GICD_CTLR: usize = 0x000;
const GICD_IGROUPR: usize = 0x080;
const GICD_ISENABLER: usize = 0x100;
const GICD_IPRIORITYR: usize = 0x400;
const GICD_ITARGETSR: usize = 0x800;
const GICD_ICFGR: usize = 0xc00;
const GICC_CTLR: usize = 0x000;
const GICC_PMR: usize = 0x004;
const GICC_IAR: usize = 0x00c;
const GICC_EOIR: usize = 0x010;
const GICC_DIR: usize = 0x1000;
const GICH_HCR: usize = 0x000;
const GICH_VMCR: usize = 0x008;
const GICH_LR0: usize = 0x100;
const GICH_LR1: usize = 0x104;

const GICD_CTLR_ENABLE_GRP1_NS: u32 = 1 << 0;
const GICC_CTLR_ENABLE_GRP1_NS: u32 = 1 << 0;
const GICC_CTLR_EOIMODE_NS: u32 = 1 << 9;
const GICH_HCR_ENABLE: u32 = 1;
const GICH_LR_HW: u32 = 1 << 31;
const GICH_LR_PENDING: u32 = 0b01 << 28;
const GICH_LR_STATE_MASK: u32 = 0b11 << 28;
const GICH_LR_PRIORITY_SHIFT: u32 = 23;
const GICH_LR_PHYSICAL_ID_SHIFT: u32 = 10;
const GICH_LR_INTID_MASK: u32 = 0x3ff;

#[inline]
unsafe fn read32(address: usize) -> u32 {
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

#[inline]
unsafe fn write32(address: usize, value: u32) {
    unsafe { core::ptr::write_volatile(address as *mut u32, value) }
}

#[inline]
unsafe fn write8(address: usize, value: u8) {
    unsafe { core::ptr::write_volatile(address as *mut u8, value) }
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VirtualGicV2State {
    pub vmcr: u32,
    pub timer_lr: u32,
    pub uart_lr: u32,
}

impl VirtualGicV2State {
    pub const fn uart_in_flight(&self) -> bool {
        self.uart_lr & GICH_LR_STATE_MASK != 0
    }
}

impl VirtualGicV2State {
    pub const fn timer_in_flight(&self) -> bool {
        self.timer_lr & GICH_LR_STATE_MASK != 0
    }
}

pub fn queue_timer_ppi(state: &mut VirtualGicV2State, ppi: u32) -> Result<(), ()> {
    if state.timer_in_flight() || ppi > GICH_LR_INTID_MASK {
        return Err(());
    }
    state.timer_lr =
        GICH_LR_HW | GICH_LR_PENDING | lr_priority(0x80) | (ppi << GICH_LR_PHYSICAL_ID_SHIFT) | ppi;
    Ok(())
}

/// Queues a software-originated SPI in LR1. The guest's non-secure GICv2
/// CPU-interface initialization enables virtual Group 0 (VMCR.VENG0), so the
/// LR group bit must remain clear. Unlike the timer LR, it has no physical
/// interrupt to deactivate when the guest EOIs it.
pub fn queue_uart_spi(state: &mut VirtualGicV2State, spi: u32) -> Result<(), ()> {
    if state.uart_in_flight() || spi > GICH_LR_INTID_MASK {
        return Err(());
    }
    state.uart_lr = GICH_LR_PENDING | lr_priority(0x80) | spi;
    Ok(())
}

const fn lr_priority(gic_priority: u8) -> u32 {
    (((gic_priority as u32) >> 3) & 0x1f) << GICH_LR_PRIORITY_SHIFT
}

pub const fn is_timer_ppi(id: u32, ppi: u32) -> bool {
    id == ppi
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_lr_is_reusable_after_invalid_state() {
        let mut state = VirtualGicV2State {
            timer_lr: GICH_LR_HW | 27,
            ..VirtualGicV2State::default()
        };
        assert!(!state.timer_in_flight());
        queue_timer_ppi(&mut state, 27).unwrap();
        assert!(state.timer_in_flight());
        assert_eq!(state.timer_lr & GICH_LR_INTID_MASK, 27);
        assert_eq!(
            state.timer_lr & (0x1f << GICH_LR_PRIORITY_SHIFT),
            0x10 << 23
        );
    }

    #[test]
    fn uart_spi_uses_a_software_list_register() {
        let mut state = VirtualGicV2State::default();
        queue_uart_spi(&mut state, 33).unwrap();
        assert!(state.uart_in_flight());
        assert_eq!(state.uart_lr & GICH_LR_INTID_MASK, 33);
        assert_eq!(state.uart_lr & GICH_LR_HW, 0);
    }
}
