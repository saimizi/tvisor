//! BCM2711 GIC-400 physical and virtualization-interface access.
//!
//! A physical timer PPI is acknowledged by EL2, represented by a hardware
//! List Register, and completed by the guest through the GIC virtual CPU
//! interface. This keeps the physical level interrupt active until the guest
//! has actually completed its corresponding virtual interrupt.

pub const GICD_BASE: usize = 0xff84_1000;
pub const GICC_BASE: usize = 0xff84_2000;
pub const GICH_BASE: usize = 0xff84_4000;
pub const GICV_BASE: usize = 0xff84_6000;
/// Guest IPA of the GICv2 virtual CPU interface page.
pub const VIRTUAL_GICV_IPA: u64 = 0x0801_0000;
/// Covers GICD, GICC, GICH, and the GIC virtual CPU-interface page.
pub const GIC_MMIO_SIZE: usize = 0x6000;
pub const SPURIOUS_IRQ: u32 = 1023;

const GICD_CTLR: usize = 0x000;
const GICD_IGROUPR: usize = 0x080;
const GICD_ISENABLER: usize = 0x100;
const GICC_CTLR: usize = 0x000;
const GICC_PMR: usize = 0x004;
const GICC_IAR: usize = 0x00c;
const GICC_EOIR: usize = 0x010;
const GICH_HCR: usize = 0x000;
const GICH_VMCR: usize = 0x008;
const GICH_LR0: usize = 0x100;

const GICD_CTLR_ENABLE_GRP1: u32 = 1 << 1;
const GICC_CTLR_ENABLE_GRP1: u32 = 1 << 1;
/// Split priority-drop from deactivation for Group 1 physical interrupts.
const GICC_CTLR_EOIMODE_NS: u32 = 1 << 9;
const GICH_HCR_ENABLE: u32 = 1;

const GICH_LR_HW: u32 = 1 << 31;
const GICH_LR_GROUP1: u32 = 1 << 30;
const GICH_LR_PENDING: u32 = 0b01 << 28;
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

/// Per-vCPU state held in GIC virtualization registers while the vCPU runs.
/// Phase 10 is single-vCPU and reserves List Register zero for the timer.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VirtualGicV2State {
    pub vmcr: u32,
    pub timer_lr: u32,
}

const _: () = assert!(core::mem::size_of::<VirtualGicV2State>() == 8);

impl VirtualGicV2State {
    pub const fn timer_in_flight(&self) -> bool {
        self.timer_lr != 0
    }
}

/// Enable a banked PPI as Group 1 and split physical EOI from deactivation.
/// A guest GICV_EOIR later deactivates the matching hardware List Register.
pub unsafe fn enable_timer_ppi(ppi: u32) {
    debug_assert!(ppi < 32);
    unsafe {
        write32(
            GICD_BASE + GICD_CTLR,
            read32(GICD_BASE + GICD_CTLR) | GICD_CTLR_ENABLE_GRP1,
        );
        write32(
            GICD_BASE + GICD_IGROUPR,
            read32(GICD_BASE + GICD_IGROUPR) | (1 << ppi),
        );
        write32(GICC_BASE + GICC_PMR, 0xff);
        write32(
            GICC_BASE + GICC_CTLR,
            read32(GICC_BASE + GICC_CTLR) | GICC_CTLR_ENABLE_GRP1 | GICC_CTLR_EOIMODE_NS,
        );
        write32(GICD_BASE + GICD_ISENABLER, 1 << ppi);
    }
}

/// Queue one physical PPI to LR0. It must already be acknowledged at GICC.
pub fn queue_timer_ppi(state: &mut VirtualGicV2State, ppi: u32) -> Result<(), ()> {
    if state.timer_in_flight() || ppi > GICH_LR_INTID_MASK {
        return Err(());
    }
    state.timer_lr = GICH_LR_HW
        | GICH_LR_GROUP1
        | GICH_LR_PENDING
        | (0x80 << GICH_LR_PRIORITY_SHIFT)
        | (ppi << GICH_LR_PHYSICAL_ID_SHIFT)
        | ppi;
    Ok(())
}

/// Load the saved virtual CPU interface before guest entry.
pub unsafe fn restore_virtual_cpu(state: &VirtualGicV2State) {
    unsafe {
        write32(GICH_BASE + GICH_VMCR, state.vmcr);
        write32(GICH_BASE + GICH_LR0, state.timer_lr);
        write32(GICH_BASE + GICH_HCR, GICH_HCR_ENABLE);
    }
}

/// Save the virtual CPU interface after every guest exit. A guest GICV_EOIR
/// clears the hardware List Register and deactivates its paired physical PPI.
pub unsafe fn save_virtual_cpu(state: &mut VirtualGicV2State) {
    unsafe {
        state.vmcr = read32(GICH_BASE + GICH_VMCR);
        state.timer_lr = read32(GICH_BASE + GICH_LR0);
        write32(GICH_BASE + GICH_HCR, 0);
    }
}

pub unsafe fn acknowledge() -> u32 {
    unsafe { read32(GICC_BASE + GICC_IAR) & GICH_LR_INTID_MASK }
}

/// Drops priority only; `EOImodeNS=1` leaves deactivation to guest GICV_EOIR.
pub unsafe fn end_interrupt(id: u32) {
    unsafe { write32(GICC_BASE + GICC_EOIR, id) }
}

pub const fn is_timer_ppi(id: u32, ppi: u32) -> bool {
    id == ppi
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_lr_preserves_physical_and_virtual_ppi() {
        let mut state = VirtualGicV2State::default();
        queue_timer_ppi(&mut state, 27).unwrap();
        assert!(state.timer_in_flight());
        assert_eq!(state.timer_lr & GICH_LR_INTID_MASK, 27);
        assert_eq!(
            (state.timer_lr >> GICH_LR_PHYSICAL_ID_SHIFT) & GICH_LR_INTID_MASK,
            27
        );
        assert_ne!(state.timer_lr & GICH_LR_HW, 0);
    }

    #[test]
    fn timer_lr_cannot_be_duplicated() {
        let mut state = VirtualGicV2State::default();
        queue_timer_ppi(&mut state, 27).unwrap();
        assert_eq!(queue_timer_ppi(&mut state, 27), Err(()));
        assert!(!is_timer_ppi(SPURIOUS_IRQ, 27));
    }
}
