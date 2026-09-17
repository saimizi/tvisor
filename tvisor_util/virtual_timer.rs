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
}

const _: () = assert!(core::mem::size_of::<VirtualTimerState>() == 24);
