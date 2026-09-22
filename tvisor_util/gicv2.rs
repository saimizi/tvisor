//! GICv2 architecture definitions and DTB-discovered driver state.

use alloc::format;
use core::error::Error;
use core::fmt::Display;

use crate::system_info::PhysRegion;
use crate::virtual_timer::VIRTUAL_TIMER_PPI;
use alloc::string::ToString;
use spin::Once;

pub const SPURIOUS_IRQ: u32 = 1023;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GicV2Error {
    InvalidIRQ(u32),
    InvalidSPI(u32),
    Unexpected,
}

impl Error for GicV2Error {}

impl Display for GicV2Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            GicV2Error::InvalidIRQ(irq) => format!("InvalidIRQ({})", irq),
            GicV2Error::InvalidSPI(irq) => format!("InvalidSPI({})", irq),
            GicV2Error::Unexpected => "Unexpected".to_string(),
        };

        write!(f, "{}", msg)
    }
}

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

    pub fn gicd_ctlr_bitflag_set(&self, bitflag: u32) {
        let gicd_ctlr = self.gicd_ctlr();
        unsafe {
            write32(gicd_ctlr, read32(gicd_ctlr) | bitflag);
        }
    }

    pub fn gicd_ctlr_set(&self, value: u32) {
        let gicd_ctlr = self.gicd_ctlr();
        unsafe {
            write32(gicd_ctlr, value);
        }
    }

    pub const fn gicc_pmr(&self) -> usize {
        self.cpu_interface_base() + GICC_PMR
    }

    pub fn gicc_pmr_set(&self, value: u32) {
        let gicc_pmr = self.gicc_pmr();
        unsafe {
            write32(gicc_pmr, value);
        }
    }

    pub const fn gicc_ctlr(&self) -> usize {
        self.cpu_interface_base() + GICC_CTLR
    }

    pub fn gicc_ctlr_bitflag_set(&self, bitflag: u32) {
        let gicc_ctlr = self.gicc_ctlr();
        unsafe {
            write32(gicc_ctlr, read32(gicc_ctlr) | bitflag);
        }
    }

    pub const fn gicd_isenabler(&self) -> usize {
        self.distributor_base() + GICD_ISENABLER
    }

    pub fn gicd_isenabler_n(&self, irq: u32) -> Result<usize, GicV2Error> {
        if irq < SPURIOUS_IRQ {
            let word_offset =
                usize::try_from((irq / 32) * 4).map_err(|_| GicV2Error::InvalidIRQ(irq))?;
            Ok(self.gicd_isenabler() + word_offset as usize)
        } else {
            Err(GicV2Error::InvalidIRQ(irq))
        }
    }

    // GICD_ISENABLER is a write-one-set register
    // write 0 doesn't take effect, so no need to do read-modify-write.
    pub fn gicd_isenabler_n_set(&self, irq: u32) -> Result<(), GicV2Error> {
        if irq < SPURIOUS_IRQ {
            let isenabler = self.gicd_isenabler_n(irq)?;
            let bit = 1_u32 << (irq % 32);
            unsafe {
                write32(isenabler, bit);
            }

            Ok(())
        } else {
            Err(GicV2Error::InvalidIRQ(irq))
        }
    }

    pub fn gicd_isenabler_n_get(&self, irq: u32) -> Result<bool, GicV2Error> {
        if irq < SPURIOUS_IRQ {
            let isenabler = self.gicd_isenabler_n(irq)?;
            let bit = 1_u32 << (irq % 32);
            unsafe { Ok(read32(isenabler) & bit == bit) }
        } else {
            Err(GicV2Error::InvalidIRQ(irq))
        }
    }

    pub const fn gicd_icenabler(&self) -> usize {
        self.distributor_base() + GICD_ICENABLER
    }

    pub fn gicd_icenabler_n(&self, irq: u32) -> Result<usize, GicV2Error> {
        if irq < SPURIOUS_IRQ {
            let word_offset =
                usize::try_from((irq / 32) * 4).map_err(|_| GicV2Error::InvalidIRQ(irq))?;
            Ok(self.gicd_icenabler() + word_offset as usize)
        } else {
            Err(GicV2Error::InvalidIRQ(irq))
        }
    }

    // GICD_ICENABLER is a write-one-clear register
    // write 0 doesn't take effect, so no need to do read-modify-write.
    pub fn gicd_icenabler_n_set(&self, irq: u32) -> Result<(), GicV2Error> {
        if irq < SPURIOUS_IRQ {
            let icenabler = self.gicd_icenabler_n(irq)?;
            let bit = 1_u32 << (irq % 32);
            unsafe {
                write32(icenabler, bit);
            }

            Ok(())
        } else {
            Err(GicV2Error::InvalidIRQ(irq))
        }
    }

    pub fn gicd_icenabler_n_get(&self, irq: u32) -> Result<bool, GicV2Error> {
        if irq < SPURIOUS_IRQ {
            let icenabler = self.gicd_icenabler_n(irq)?;
            let bit = 1_u32 << (irq % 32);
            unsafe { Ok(read32(icenabler) & bit == bit) }
        } else {
            Err(GicV2Error::InvalidIRQ(irq))
        }
    }

    pub fn gicd_enable_irq(&self, irq: u32) -> Result<(), GicV2Error> {
        self.gicd_isenabler_n_set(irq)
    }

    pub fn gicd_disable_irq(&self, irq: u32) -> Result<(), GicV2Error> {
        self.gicd_icenabler_n_set(irq)
    }

    const fn gicd_igroupr(&self) -> usize {
        self.distributor_base() + GICD_IGROUPR
    }

    pub fn gicd_igroupr_n(&self, irq: u32) -> Result<usize, GicV2Error> {
        if irq < SPURIOUS_IRQ {
            let word_offset =
                usize::try_from((irq / 32) * 4).map_err(|_| GicV2Error::InvalidIRQ(irq))?;
            Ok(self.gicd_igroupr() + word_offset as usize)
        } else {
            Err(GicV2Error::InvalidIRQ(irq))
        }
    }

    pub fn gicd_igroupr_n_get(&self, irq: u32) -> Result<bool, GicV2Error> {
        if irq < SPURIOUS_IRQ {
            let igroupr = self.gicd_igroupr_n(irq)?;
            let bit = 1_u32 << (irq % 32);
            let value = unsafe { read32(igroupr) & bit };
            Ok(value == bit)
        } else {
            Err(GicV2Error::InvalidIRQ(irq))
        }
    }

    pub fn gicd_igroupr_n_set(&self, irq: u32, on: bool) -> Result<(), GicV2Error> {
        if irq < SPURIOUS_IRQ {
            let igroupr = self.gicd_igroupr_n(irq)?;
            let bit = 1_u32 << (irq % 32);
            unsafe {
                if on {
                    write32(igroupr, read32(igroupr) | bit);
                } else {
                    write32(igroupr, read32(igroupr) & !bit);
                }

                Ok(())
            }
        } else {
            Err(GicV2Error::InvalidIRQ(irq))
        }
    }

    pub fn gicd_ipriorityr(&self) -> usize {
        self.distributor_base() + GICD_IPRIORITYR
    }

    pub fn gicd_ipriorityr_set(&self, irq: u32, value: u8) {
        unsafe {
            write8(self.gicd_ipriorityr() + irq as usize, value);
        }
    }

    pub fn gicd_itargetsr(&self) -> usize {
        self.distributor_base() + GICD_ITARGETSR
    }

    pub fn gicd_itargetsr_set(&self, irq: u32, value: u8) {
        unsafe {
            write8(self.gicd_itargetsr() + irq as usize, value);
        }
    }

    pub fn gicd_icfgr(&self) -> usize {
        self.distributor_base() + GICD_ICFGR
    }

    pub fn gicd_icfgr_n(&self, irq: u32) -> Result<usize, GicV2Error> {
        if irq < SPURIOUS_IRQ {
            let word_offset =
                usize::try_from((irq / 16) * 4).map_err(|_| GicV2Error::InvalidIRQ(irq))?;
            Ok(self.gicd_icfgr() + word_offset)
        } else {
            Err(GicV2Error::InvalidIRQ(irq))
        }
    }

    pub fn gicd_icfgr_n_get(&self, irq: u32) -> Result<bool, GicV2Error> {
        if irq < SPURIOUS_IRQ {
            let ifcfgr = self.gicd_icfgr_n(irq)?;
            let bit = 1_u32 << ((irq % 16) * 2 + 1);
            let value = unsafe { read32(ifcfgr) & bit };
            Ok(value == bit)
        } else {
            Err(GicV2Error::InvalidIRQ(irq))
        }
    }

    pub fn gicd_icfgr_n_set(&self, irq: u32, on: bool) -> Result<(), GicV2Error> {
        if irq < SPURIOUS_IRQ {
            let ifcfgr = self.gicd_icfgr_n(irq)?;
            let bit = 1_u32 << ((irq % 16) * 2 + 1);
            unsafe {
                if on {
                    write32(ifcfgr, read32(ifcfgr) | bit);
                } else {
                    write32(ifcfgr, read32(ifcfgr) & !bit);
                }
            }

            Ok(())
        } else {
            Err(GicV2Error::InvalidIRQ(irq))
        }
    }

    pub const fn gich_vmcr(&self) -> usize {
        self.hypervisor_interface_base() + GICH_VMCR
    }

    pub fn gich_vmcr_set(&self, value: u32) {
        let gich_vmcr = self.gich_vmcr();
        unsafe { write32(gich_vmcr, value) }
    }

    pub fn gich_vmcr_get(&self) -> u32 {
        let gich_vmcr = self.gich_vmcr();
        unsafe { read32(gich_vmcr) }
    }

    pub const fn gich_lr0(&self) -> usize {
        self.hypervisor_interface_base() + GICH_LR0
    }

    pub fn gich_lr0_set(&self, value: u32) {
        let gich_lr0 = self.gich_lr0();
        unsafe {
            write32(gich_lr0, value);
        }
    }

    pub fn gich_lr0_get(&self) -> u32 {
        let gich_lr0 = self.gich_lr0();
        unsafe { read32(gich_lr0) }
    }

    pub const fn gich_lr1(&self) -> usize {
        self.hypervisor_interface_base() + GICH_LR1
    }

    pub fn gich_lr1_set(&self, value: u32) {
        let gich_lr1 = self.gich_lr1();
        unsafe {
            write32(gich_lr1, value);
        }
    }

    pub fn gich_lr1_get(&self) -> u32 {
        let gich_lr1 = self.gich_lr1();
        unsafe { read32(gich_lr1) }
    }

    pub const fn gich_hcr(&self) -> usize {
        self.hypervisor_interface_base() + GICH_HCR
    }

    pub fn gich_hcr_set(&self, value: u32) {
        let gich_hcr = self.gich_hcr();
        unsafe {
            write32(gich_hcr, value);
        }
    }

    pub fn gicc_iar(&self) -> usize {
        self.cpu_interface_base() + GICC_IAR
    }

    pub fn gicc_iar_get(&self) -> u32 {
        let gicc_iar = self.gicc_iar();
        unsafe { read32(gicc_iar) }
    }

    pub const fn gicc_eoir(&self) -> usize {
        self.cpu_interface_base() + GICC_EOIR
    }

    pub fn gicc_eoir_set(&self, value: u32) {
        let gicc_eoir = self.gicc_eoir();
        unsafe {
            write32(gicc_eoir, value);
        }
    }

    pub const fn gicc_dir(&self) -> usize {
        self.cpu_interface_base() + GICC_DIR
    }

    pub fn gicc_dir_set(&self, value: u32) {
        let gicc_dir = self.gicc_dir();
        unsafe {
            write32(gicc_dir, value);
        }
    }

    /// Enables a banked virtual timer PPI (27) from the GICv2 Non-secure register view.
    pub unsafe fn enable_virtual_timer_ppi(&self) -> Result<(), GicV2Error> {
        // Explicitly classify PPI 27 as Non-Secure interrupt (Group 1)
        self.gicd_igroupr_n_set(VIRTUAL_TIMER_PPI, true)?;

        // Enable Group 1 forwarding
        self.gicd_ctlr_bitflag_set(GICD_CTLR_ENABLE_GRP1_NS);

        // Allow all normal priorities through the CPU interface
        self.gicc_pmr_set(0xff);

        self.gicc_ctlr_bitflag_set(GICC_CTLR_ENABLE_GRP1_NS | GICC_CTLR_EOIMODE_NS);
        self.gicd_enable_irq(VIRTUAL_TIMER_PPI)?;

        Ok(())
    }

    /// Enables a Non-secure Group 1 shared peripheral interrupt on CPU0.
    /// This is used for host-owned EL2 devices; it is never exposed directly
    /// to a guest.
    pub unsafe fn enable_spi(&self, spi: u32) -> Result<(), GicV2Error> {
        if !(32..SPURIOUS_IRQ).contains(&spi) {
            return Err(GicV2Error::InvalidSPI(spi));
        }

        self.gicd_ctlr_bitflag_set(GICD_CTLR_ENABLE_GRP1_NS);
        self.gicc_pmr_set(0xff);
        self.gicc_ctlr_bitflag_set(GICC_CTLR_ENABLE_GRP1_NS | GICC_CTLR_EOIMODE_NS);
        self.gicd_igroupr_n_set(spi, true)?;
        self.gicd_ipriorityr_set(spi, 0x80);
        self.gicd_itargetsr_set(spi, 0x01);
        // The DTB describes the Mini UART as level-high. Clear the
        // trigger-type bit explicitly instead of relying on U-Boot's
        // inherited distributor configuration.
        self.gicd_icfgr_n_set(spi, false)?;
        self.gicd_enable_irq(spi)?;

        Ok(())
    }

    pub unsafe fn restore_virtual_cpu(&self, state: &VirtualGicV2State) {
        self.gich_vmcr_set(state.vmcr);
        self.gich_lr0_set(state.timer_lr);
        self.gich_lr1_set(state.uart_lr);
        self.gich_hcr_set(GICH_HCR_ENABLE);
    }

    pub unsafe fn save_virtual_cpu(&self, state: &mut VirtualGicV2State) {
        state.vmcr = self.gich_vmcr_get();
        state.timer_lr = self.gich_lr0_get();
        state.uart_lr = self.gich_lr1_get();
        self.gich_hcr_set(0);
    }

    pub unsafe fn acknowledge(&self) -> u32 {
        self.gicc_iar_get() & GICH_LR_INTID_MASK
    }

    /// Drops priority only; guest GICV_EOIR deactivates the hardware LR.
    pub unsafe fn end_interrupt(&self, id: u32) {
        self.gicc_eoir_set(id);
    }

    /// Completes a host-owned interrupt after `end_interrupt`. EOImodeNS is
    /// enabled, so EOIR only drops priority; physical sources which are not
    /// represented by a hardware List Register must be deactivated here.
    pub unsafe fn deactivate_interrupt(&self, id: u32) {
        self.gicc_dir_set(id);
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
const GICD_ICENABLER: usize = 0x180;
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
