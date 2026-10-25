# Phase 11.0 implementation and board evidence

Status: discovery implementation validated on lab-rpi4, 2026-10-25.
The full startup-contract gate is not yet closed; secondary activation remains
unimplemented and disabled. Base revision: `2e78ca2` on
`feature/phase11_smp_support`.

## Implemented

- Allocation-free, owned physical CPU descriptions in
  `tvisor_util/cpu_topology.rs`, bounded to four CPU nodes for this board profile.
- Exact one-/two-cell CPU affinity decoding, reserved-bit checks, duplicate and
  capacity rejection, boot-CPU matching against MPIDR affinity, deterministic
  logical IDs with boot CPU zero, and disabled-CPU recording without logical IDs.
- CPU startup-method discovery without inheriting the legacy `/cpus` parent
  method over an explicit per-CPU method. Missing/unsupported methods remain
  explicit descriptions and grant no permission to activate a CPU.
- PSCI binding/conduit discovery, including modern and legacy descriptions.
  An advertised interface does not prove EL2 callability. Pure version-dependent
  capability planning distinguishes PSCI 0.2's CPU_ON contract from PSCI 1.0+
  PSCI_FEATURES queries using the AArch64 CPU_ON function ID.
- Boot-time diagnostics before allocator initialization. No SMC, HVC, CPU_ON,
  release-cell writes, secondary entry code or allocator-policy changes.

## Observed board configuration

| Item | Observation |
|---|---|
| U-Boot | 2024.01, build timestamp Jan 08 2024 15:37:48 UTC |
| Live handoff DTB | `0x2eff1f00`, size `0xe0b7` |
| Boot MPIDR | `0x80000000`, affinity `0`, assigned logical CPU0 |
| CPU affinities | `0`, `1`, `2`, `3`, all enabled |
| Per-CPU enable method | `spin-table` on all four nodes |
| Release cells | `0xd8`, `0xe0`, `0xe8`, `0xf0`, all initially zero |
| PSCI | No `/psci` node or discovered PSCI interface; no conduit/version/features calls |
| DT memory reservation | `[0x0, 0x1000)` protects the observed low-memory code and cells |

Captured memory disassembly shows the secondary path reading MPIDR, selecting
its release cell, executing WFE at `0x7c`, loading the cell at `0x80`, and
returning to WFE while it is zero. A nonzero entry branches through x4 at
`0xa4`, with x0-x3 zeroed on the secondary path. This loop has no stack accesses
or dependencies on relocated U-Boot's stack/tables in the captured instructions.
The preceding code programs SCTLR_EL2 and returns from EL3 with SPSR_EL3 `0x3c9`
(EL2h with DAIF masked); SCTLR_EL2 literal `0x30c50830` has M/C/I clear.
These are deductions from captured instructions, not live secondary register
measurements or proof of the exact firmware build provenance/coherency state.

The normal boot log confirms the first-page reservation is imported by existing
memory discovery before allocator initialization. No new guessed reservation
or reclamation policy was introduced.

## Validation

- Native ARM64 library tests: **95 passed**, including 11 new CPU discovery and
  PSCI capability-planning tests (ordering, disabled/absent boot CPU, capacity,
  duplicate/reserved affinities, malformed cells/registers/release addresses,
  malformed methods, missing CPU tree, SMC/HVC, legacy/modern PSCI and unsupported
  version/method handling).
- Bare-metal build, `cargo fmt --check`, and `git diff --check`: passed.
- Two RAM-only hardware boots: discovery report and normal one-vCPU Linux 6.18.2
  guest reached BusyBox. On the first boot, UART echo/uname/ending marker all
  completed with status zero. This is boot/UART validation, not a quick or
  overnight regression run.
- The normal guest Image was reused unchanged, SHA-256
  `3a72c904e211f56e5f05d601b41495dfa4ec9029b3d7b1ee3556a72f9bb33b15`.
- Final tvisor ELF SHA-256:
  `be9866d47ee82d6dd85f5213fdf3ec1f734b5e6efe10d68920d069f6c265c2d0`.
- No U-Boot environment was saved and no SD-card files were written.
- lab-rpi4 was powered off after validation; relay state was read back as off.

Raw evidence: [U-Boot inspection](evidence/phase11_0_uboot_2026-10-25.txt),
[initial UART capture](evidence/phase11_0_boot_2026-10-25.txt),
[guest command completion](evidence/phase11_0_guest_2026-10-25.txt),
[low-memory disassembly](evidence/phase11_0_lowmem_disassembly.txt), and
[final build boot](evidence/phase11_0_final_boot_2026-10-25.txt).
The initial full UART capture includes the extended low-memory dump and DT
reservation/header inspection. Expected bootelf/prompt-detection timeouts were
checked against fresh UART output; they were not counted as guest failures.

## Remaining startup-contract gate

Before any secondary release, establish exact firmware/U-Boot build provenance,
verify the complete holding-memory footprint and coherency/entry contract, and
review the publication/cache-maintenance sequence for this backend. Retain the
first-page reservation permanently for the run. The source currently neither
claims that these checks passed nor provides an activation API.

On a PSCI-advertising platform, a reviewed EL2-callable conduit adapter and
non-activating PSCI_VERSION/PSCI_FEATURES probes are still required. Only parsing
and pure capability planning are implemented here; spin-table evidence on this
board does not validate the PSCI execution path. Actual secondary registers
will be checked at the guarded 11.3 entry, as specified by the design.
