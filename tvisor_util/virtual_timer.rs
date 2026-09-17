//! Per-vCPU architectural virtual-timer state and virtual-IRQ policy.

/// Architectural virtual timer interrupt PPI used by arm64 Linux.
pub const VIRTUAL_TIMER_PPI: u32 = 27;

/// Hardware-visible timer state saved whenever a vCPU leaves EL1.
///
/// `pending_irq` records an expiration until EL2 queues its hardware-backed
/// GICv2 List Register.
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

    pub fn clear_pending_after_list_register(&mut self) {
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
        assert_eq!(timer.pending_irq, 1);
        timer.clear_pending_after_list_register();
        assert_eq!(timer.pending_irq, 0);
    }
}
