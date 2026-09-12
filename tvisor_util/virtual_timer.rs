//! Per-vCPU architectural virtual-timer state and virtual-IRQ policy.

/// Architectural virtual timer interrupt PPI used by arm64 Linux.
pub const VIRTUAL_TIMER_PPI: u32 = 27;

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
    /// Records an expiration reported by the architectural timer IRQ.
    pub fn mark_pending_from_irq(&mut self) {
        self.pending_irq = 1;
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
    fn timer_irq_marks_pending() {
        let mut timer = VirtualTimerState::default();
        timer.mark_pending_from_irq();
        assert!(timer.virtual_irq_pending());
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
