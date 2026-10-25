# Phase 11.1: per-CPU storage and guarded EL2 stacks

Status: **implemented and quick regression passed**, 2026-10-25. Work starts from `70918f5` on
`feature/phase11_smp_support`. No secondary CPU is released in this milestone.

## Implementation

`src/pcpu.rs` owns four lifetime-stable, cache-line-aligned CPU slots. Each slot
separates owner-only mutable world-switch context from atomic startup metadata.
Identity, stack bounds and presence are initialized once from DT logical IDs,
then release-published as `Prepared`. The boot CPU's logical ID is zero regardless
of its physical MPIDR affinity. Disabled/absent CPUs are not published Prepared.

The context is `repr(C, align(64))`, 64 bytes, with compile-time checks preserving
`host_sp` at offset 0 and `active_vcpu` at offset 8. Assembly uses those two fields;
no reference to the former global `__pcpu_state` remains. The context itself is
not declared Sync. Its static container documents the one-time initialization,
immutable metadata publication and exclusive physical-CPU ownership that justify
static sharing. No Rust references to mutable context survive a world switch.

The startup metadata reserves lifecycle, generation, request and acknowledgement
storage for later milestones. Only `Discovered -> Prepared` is implemented in
11.1. The generation starts at one and request/acknowledgement fields remain zero;
release, arrival and management-message protocols are not implemented yet.

Early entry masks DAIF before global initialization. The private-stack takeover
receives the prepared boot context and stack top, sets SP and `TPIDR_EL2`, then
enters Rust. All four world-switch/exit assembly lookup sites use `MRS TPIDR_EL2`.
Boot checkpoints and every guest entry/return assert CPU0's pointer, full affinity,
stack bounds, saved host SP bounds/alignment, and null inactive vCPU association.
Workers remain in firmware; preparing their storage does not initialize their
physical EL2 registers or install their TPIDR values.

## Stacks, guards and table capacity

The linker reserves a 272 KiB arena: four repetitions of one 4 KiB guard followed
by one 64 KiB stack. Alignment/extent/non-overlap assertions and the shared Rust
layout calculation must agree. Every stack is identity-mapped RW/NX Normal;
guards are omitted. Software walks verify every stack page, stack-end byte and
guard page both before takeover and after mapping usable allocator RAM.

The pilot ELF has runtime bounds `[0x04000000, 0x04140000)` and stack arena
`[0x040ec000, 0x04130000)`. The full table arena remains 64 KiB (16 pages):
hardware checkpoints report 9 used pages before and after usable-RAM mapping.
A host test forces the four-stack arena across a 2 MiB boundary and verifies
four required table pages; another proves inadequate storage fails explicitly.
The actual boot also rechecks the complete mappings instead of relying on that
stack-only bound as a guarantee for the entire address space.

Fresh U-Boot `bdinfo` confirmed the runtime and ELF source remain below the live
DTB at `0x2eff1f00`, guest staging at `0x30100000`, and U-Boot runtime reservation
`[0x36b2b000, 0x38000000)`. The entire linker-owned image, including all stacks
and guards, remains reserved from allocation.

## Validation

- 107 native ARM64 library tests passed, including layout/overflow/alignment,
  guard preservation, table-capacity failure and initial context/startup state.
- Bare-metal build, formatting and whitespace checks passed.
- Disassembly confirms SP and TPIDR installation before private Rust entry, four
  TPIDR-based world-switch lookups, and removal of the global context symbol.
- Pilot lab-rpi4 boot reached Linux BusyBox with all four prepared-slot records,
  CPU0 TPIDR installed, worker vCPU associations null, and all four guards absent.
- Full quick regression **PASS**: four boots, 18 completed guest commands,
  10 independent UART/IRQ checks, and a 600.453-second soak with 2,136 wakeups,
  8,119 complete 32 MiB memory passes, maximum lateness 7.585 ms, and zero
  wakeups over 500 ms late. The smoke and final reboot probes passed too.
- Every fresh boot log confirms four prepared contexts, null inactive vCPU
  associations, boot CPU TPIDR installation, four absent guards, and 9/16 table
  pages used. No fatal markers or failures were reported.
- Final runner result includes `power_off_verified`; independent relay readback
  also confirmed lab-rpi4 off. No saved U-Boot environment or SD files changed.
- Results: `guest/build/phase111-quick-20261025/`. Source diffs, an archive/hashes
  of new source files, frozen images, configs, disassembly and UART evidence are
  kept with the run. This generated directory is ignored by Git.
- Tested tvisor ELF SHA-256:
  `6027bf33ef5e81290999eff0888dcb9d9f55fb77da3e41904d918ca129d6eff3`.
  Normal guest Image SHA-256 remains
  `3a72c904e211f56e5f05d601b41495dfa4ec9029b3d7b1ee3556a72f9bb33b15`.
  Current compiled source hashes and ELF were checked against the frozen bundle.
  This is a quick pass, not a new overnight or physical-SMP execution result.

This milestone validates the boot-CPU implementation. It does not establish
allocator/UART concurrency safety (11.2), execute secondary entry (11.3), add a
scheduler, or expand the Linux guest beyond one vCPU.
