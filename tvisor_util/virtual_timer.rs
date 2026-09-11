//! Per-vCPU architectural virtual-timer state and virtual-IRQ policy.

/// Architectural virtual timer interrupt PPI used by arm64 Linux.
pub const VIRTUAL_TIMER_PPI: u32 = 27;

const CNTV_CTL_ENABLE: u64 = 1 << 0;
const CNTV_CTL_IMASK: u64 = 1 << 1;

/// Hardware-visible timer state saved whenever a vCPU leaves EL1.
///
/// `pending_irq` is tvisor-owned state. A non-zero value requests HCR_EL2.VI
/// injection at the next guest entry; it does not expose a host interrupt.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VirtualTimerState {
    pub cntvoff_el2: u64,
    pub cntv_cval_el0: u64,
    pub cntv_ctl_el0: u64,
    pub pending_irq: u64,
}

const _: () = assert!(core::mem::size_of::<VirtualTimerState>() == 32);

impl VirtualTimerState {
    /// Updates the virtual timer's pending PPI using a virtual counter value.
    /// The caller supplies `CNTVCT_EL0`, which already reflects CNTVOFF_EL2.
    pub fn refresh_pending(&mut self, virtual_count: u64) {
        let enabled = self.cntv_ctl_el0 & CNTV_CTL_ENABLE != 0;
        let masked = self.cntv_ctl_el0 & CNTV_CTL_IMASK != 0;
        self.pending_irq = u64::from(enabled && !masked && virtual_count >= self.cntv_cval_el0);
    }

    pub const fn virtual_irq_pending(&self) -> bool {
        self.pending_irq != 0
    }

    pub fn acknowledge_virtual_irq(&mut self) {
        self.pending_irq = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_expires_only_when_enabled_and_unmasked() {
        let mut timer = VirtualTimerState {
            cntv_cval_el0: 100,
            cntv_ctl_el0: CNTV_CTL_ENABLE,
            ..VirtualTimerState::default()
        };
        timer.refresh_pending(99);
        assert!(!timer.virtual_irq_pending());
        timer.refresh_pending(100);
        assert!(timer.virtual_irq_pending());

        timer.cntv_ctl_el0 |= CNTV_CTL_IMASK;
        timer.refresh_pending(101);
        assert!(!timer.virtual_irq_pending());
    }

    #[test]
    fn acknowledge_clears_pending_irq() {
        let mut timer = VirtualTimerState {
            pending_irq: 1,
            ..VirtualTimerState::default()
        };
        timer.acknowledge_virtual_irq();
        assert!(!timer.virtual_irq_pending());
    }
}
