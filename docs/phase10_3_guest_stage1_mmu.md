# Phase 10.3: Linux guest stage-1 MMU bring-up

Linux owns its EL1 VA-to-IPA translation regime. `VcpuContext` therefore saves
and restores `SCTLR_EL1`, `TTBR0_EL1`, `TTBR1_EL1`, `TCR_EL1`, `MAIR_EL1`, and
`VBAR_EL1` around every lower-EL exit. `GuestEl1TranslationState` exposes that
subset as one explicit snapshot, while `guest_stage1_mmu_enabled()` detects
`SCTLR_EL1.M` for the Phase 10.3 checkpoint.

The context also now preserves `TPIDR_EL1` and `TPIDRRO_EL0`, so Linux's
current-task/thread-pointer state cannot be corrupted by a stage-2 fault or a
trapped-MMIO exit.

The translation relationship remains:

```text
Linux VA -> guest EL1 stage 1 -> guest IPA -> EL2 stage 2 -> host PA
```

EL2 never copies Linux's RX/RW/XN policy into its own stage-1 tables. Linux
owns permissions within the VM; stage 2 owns the VM's access to host physical
resources. A future real-Linux run records the first exit after
`SCTLR_EL1.M = 1` as the dedicated Phase 10.3 hardware checkpoint.
