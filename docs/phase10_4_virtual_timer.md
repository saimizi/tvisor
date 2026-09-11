# Phase 10.4: virtual timer and interrupt foundation

Each VM-owned `Vcpu` contains `VirtualTimerState`, preserving
`CNTVOFF_EL2`, `CNTV_CVAL_EL0`, and `CNTV_CTL_EL0` over lower-EL exits. Before
each guest entry, tvisor samples `CNTVCT_EL0`; an enabled, unmasked, expired
timer marks virtual-timer PPI 27 pending. The world switch then maps that
pending state to `HCR_EL2.VI`, which delivers a virtual IRQ when guest
`PSTATE.DAIF.I` permits it.

The current policy is deliberately simple:

- `CNTVOFF_EL2` is per-vCPU and initially zero, so the guest virtual counter
  starts aligned with the host counter;
- guest virtual timer compare/control state is preserved per vCPU;
- physical counter/timer access remains controlled by the existing
  `CNTHCTL_EL2` policy; and
- a pending virtual IRQ is injected through `HCR_EL2.VI`, not by assigning a
  host device interrupt directly to the guest.

This is the virtual-timer and injection foundation, not the final Phase 10.4
hardware checkpoint. Linux still needs a selected virtual GIC interface, EL2
physical-IRQ routing, and a timer-IRQ wakeup path while the guest runs without
exits. Those pieces are required before claiming scheduler/timekeeping progress.
