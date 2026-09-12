# Phase 10.4: virtual timer and interrupt foundation

Each VM-owned `Vcpu` contains `VirtualTimerState`, preserving
`CNTVOFF_EL2`, `CNTV_CVAL_EL0`, and `CNTV_CTL_EL0` over lower-EL exits. Before
guest entry, tvisor restores that state. When the architectural virtual timer
expires, its physical PPI 27 is routed to EL2 by `HCR_EL2.IMO`. The EL2 IRQ
vector acknowledges PPI 27 and places it into GICH List Register 0 as a
hardware-backed virtual PPI 27. The guest accesses the mapped GICV virtual CPU
interface directly: its interrupt acknowledge and EOI operations update the
List Register. The guest `GICV_EOIR` deactivates the paired physical PPI, so a
level-triggered timer source cannot re-enter EL2 before Linux handles it.

The current policy is deliberately simple:

- `CNTVOFF_EL2` is per-vCPU and initially zero, so the guest virtual counter
  starts aligned with the host counter;
- guest virtual timer compare/control state is preserved per vCPU;
- EL2 maps the BCM2711 GIC distributor, physical CPU interface, GICH control
  interface, and GICV page as Device MMIO;
- the platform handoff must classify physical PPI 27 as Non-secure Group 1;
  tvisor uses the GICv2 Non-secure control-register view and does not access
  Secure-only `GICD_IGROUPR`; with `GICC_CTLR.EOImodeNS=1`, EL2's physical
  EOI drops priority only while the guest's virtual EOI completes deactivation;
- GICH VMCR and LR0 are saved and restored with the vCPU, and GICV is mapped
  at guest IPA `0x0801_0000`; and
- physical counter/timer access remains controlled by the existing
  `CNTHCTL_EL2` policy; and
- expiration detection is exclusively hardware-driven; tvisor does not poll
  `CNTVCT_EL0` before guest entry.

This is the virtual-timer and injection foundation, not the final Phase 10.4
hardware checkpoint. The IRQ-driven EL2 timer path is implemented, but its
hardware test is intentionally deferred. A complete virtual distributor and
guest DTB interrupt-controller description are still required before Linux can
boot using this interface.
