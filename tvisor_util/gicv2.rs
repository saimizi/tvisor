//! GICv2 architecture definitions and DTB-discovered driver state.

use crate::system_info::PhysRegion;
use spin::Once;

/// Guest IPA of tvisor's GICv2 virtual CPU-interface region.
pub const VIRTUAL_GICV_IPA: u64 = 0x0801_0000;
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

    pub const fn gich_vmcr(&self) -> usize {
        self.hypervisor_interface_base() + GICH_VMCR
    }

    pub const fn gich_lr0(&self) -> usize {
        self.hypervisor_interface_base() + GICH_LR0
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

    pub unsafe fn restore_virtual_cpu(&self, state: &VirtualGicV2State) {
        unsafe {
            write32(self.gich_vmcr(), state.vmcr);
            write32(self.gich_lr0(), state.timer_lr);
            write32(self.gich_hcr(), GICH_HCR_ENABLE);
        }
    }

    pub unsafe fn save_virtual_cpu(&self, state: &mut VirtualGicV2State) {
        unsafe {
            state.vmcr = read32(self.gich_vmcr());
            state.timer_lr = read32(self.gich_lr0());
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
}

pub fn initialize(info: GicV2Info) -> &'static GicV2 {
    GLOBAL_GIC.call_once(|| GicV2::new(info))
}

pub fn global() -> Option<&'static GicV2> {
    GLOBAL_GIC.get()
}

pub const REQUIRED_GICD_SIZE: u64 = 0x104; // GICD_ISENABLER0 at 0x100
pub const REQUIRED_GICC_SIZE: u64 = 0x14; // GICC_EOIR at 0x10
pub const REQUIRED_GICH_SIZE: u64 = 0x104; // GICH_LR0 at 0x100
pub const REQUIRED_GICV_SIZE: u64 = 0x1004; // GICV_DIR at 0x1000, if needed

const GICD_CTLR: usize = 0x000;
const GICD_ISENABLER: usize = 0x100;
const GICC_CTLR: usize = 0x000;
const GICC_PMR: usize = 0x004;
const GICC_IAR: usize = 0x00c;
const GICC_EOIR: usize = 0x010;
const GICH_HCR: usize = 0x000;
const GICH_VMCR: usize = 0x008;
const GICH_LR0: usize = 0x100;

const GICD_CTLR_ENABLE_GRP1_NS: u32 = 1 << 0;
const GICC_CTLR_ENABLE_GRP1_NS: u32 = 1 << 0;
const GICC_CTLR_EOIMODE_NS: u32 = 1 << 9;
const GICH_HCR_ENABLE: u32 = 1;
const GICH_LR_HW: u32 = 1 << 31;
const GICH_LR_GROUP1: u32 = 1 << 30;
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

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VirtualGicV2State {
    pub vmcr: u32,
    pub timer_lr: u32,
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
    state.timer_lr = GICH_LR_HW
        | GICH_LR_GROUP1
        | GICH_LR_PENDING
        | lr_priority(0x80)
        | (ppi << GICH_LR_PHYSICAL_ID_SHIFT)
        | ppi;
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
            timer_lr: GICH_LR_HW | GICH_LR_GROUP1 | 27,
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
}
