//! Minimal BCM2711 GIC-400 CPU/distributor access for EL2 timer IRQ routing.

pub const GICD_BASE: usize = 0xff84_1000;
pub const GICC_BASE: usize = 0xff84_2000;
/// One 4 KiB page for the distributor and one for the CPU interface.
pub const GIC_MMIO_SIZE: usize = 0x2000;
pub const SPURIOUS_IRQ: u32 = 1023;
const GICD_CTLR: usize = 0x000;
const GICD_ISENABLER: usize = 0x100;
const GICC_CTLR: usize = 0x000;
const GICC_PMR: usize = 0x004;
const GICC_IAR: usize = 0x00c;
const GICC_EOIR: usize = 0x010;
#[inline]
unsafe fn read32(address: usize) -> u32 {
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

#[inline]
unsafe fn write32(address: usize, value: u32) {
    unsafe { core::ptr::write_volatile(address as *mut u32, value) }
}

/// Enable a banked PPI and make Group 0 interrupts visible to the EL2 CPU
/// interface.  The caller must have mapped the GIC MMIO region as Device.
pub unsafe fn enable_timer_ppi(ppi: u32) {
    debug_assert!(ppi < 32);
    unsafe {
        write32(GICD_BASE + GICD_CTLR, 1);
        write32(GICC_BASE + GICC_PMR, 0xff);
        write32(GICC_BASE + GICC_CTLR, 1);
        write32(GICD_BASE + GICD_ISENABLER, 1 << ppi);
    }
}

/// Acknowledge the highest-priority pending physical IRQ at the CPU interface.
pub unsafe fn acknowledge() -> u32 {
    unsafe { read32(GICC_BASE + GICC_IAR) & 0x3ff }
}

/// Signal completion of a physical IRQ previously returned by [`acknowledge`].
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
    fn identifies_timer() {
        assert!(is_timer_ppi(27, 27));
        assert!(!is_timer_ppi(SPURIOUS_IRQ, 27));
    }
}
