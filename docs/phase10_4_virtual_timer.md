# Phase 10.4: virtual timer and interrupt foundation

Each VM-owned `Vcpu` contains `VirtualTimerState`, preserving
`CNTVOFF_EL2`, `CNTV_CVAL_EL0`, and `CNTV_CTL_EL0` over lower-EL exits. Before
guest entry, tvisor restores that state. When the architectural virtual timer
expires, its physical PPI 27 is routed to EL2 by `HCR_EL2.IMO`. The EL2 IRQ
vector acknowledges and completes the physical GIC interrupt, then marks the
vCPU's virtual-timer PPI 27 pending. The next world switch maps that pending
state to `HCR_EL2.VI`, which delivers a virtual IRQ when guest `PSTATE.DAIF.I`
permits it.

The current policy is deliberately simple:

- `CNTVOFF_EL2` is per-vCPU and initially zero, so the guest virtual counter
  starts aligned with the host counter;
- guest virtual timer compare/control state is preserved per vCPU;
- EL2 maps the BCM2711 GIC distributor and CPU-interface pages as Device MMIO,
  enables PPI 27, and uses the IRQ vector rather than polling `CNTVCT_EL0` at
  guest-entry boundaries;
- physical counter/timer access remains controlled by the existing
  `CNTHCTL_EL2` policy; and
- a pending virtual IRQ is injected through `HCR_EL2.VI`, not by assigning a
  host device interrupt directly to the guest.

This is the virtual-timer and injection foundation, not the final Phase 10.4
hardware checkpoint. The IRQ-driven EL2 timer path is implemented, but its
hardware test is intentionally deferred. Linux still needs a selected
guest-visible virtual GIC interface to acknowledge, prioritize, and deassert
injected interrupts before claiming scheduler/timekeeping progress.
