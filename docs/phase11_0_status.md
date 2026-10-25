# Phase 11.0 implementation and board evidence

Status: **complete for the verified lab-rpi4 cold-boot profile**, 2026-10-25.
Secondary activation remains disabled. The entry contract is established from
identified firmware bytes; actual secondary entry-register validation remains
part of 11.3, not an unperformed 11.0 activation test.

See the [verified secondary startup contract](rpi4_secondary_startup.md) for
firmware provenance, full memory dependencies, entry/coherency requirements,
release/publication protocol, validation and portability limits.

## Implemented

- Allocation-free owned CPU descriptions, full MPIDR affinity matching,
  deterministic logical IDs with the boot CPU zero, disabled-CPU recording,
  exact cell decoding and malformed/duplicate/capacity rejection.
- Explicit PSCI binding and SMC/HVC description parsing, plus version-dependent
  CPU_ON capability planning. This board advertises no PSCI interface, so
  firmware queries are not applicable. There is no guessed conduit or fallback.
- A verified BCM2711 Cortex-A72 spin-table profile, checked against the actual
  low-memory stub at boot before trusting its memory footprint or entry state.
- A permanent holding-page reservation invariant checked against the live DTB
  and finalized memory map before allocator initialization. Unknown firmware or
  missing protection halts boot.
- No CPU_ON, release-cell writes, secondary trampoline, allocator reclamation
  policy change, SD-card writes or saved-environment changes.

## Board findings

| Item | Verified result |
|---|---|
| Boot CPU | MPIDR `0x80000000`, affinity zero, logical CPU0 |
| Physical topology | Four enabled CPUs, affinities 0..3 |
| Backend | Per-CPU `spin-table` |
| Release cells | `0xd8`, `0xe0`, `0xe8`, `0xf0`, initially zero |
| Holding dependencies | First 4 KiB, already reserved by the live DTB |
| Secondary entry contract | Non-secure AArch64 EL2h; DAIF masked; MMU/D-cache/I-cache off; x0-x3 zero |
| Maintenance coherency | Firmware sets Cortex-A72 SMPEN before park; boot CPU readback `0x40` |
| PSCI | Not advertised; no probes called |

The installed firmware was compared byte-for-byte with the official pinned
`start4.elf`; the entire embedded armstub was reproduced from pinned source and
all immutable live bytes match. U-Boot's exact downstream `.config` was not
recovered; its assumed defaults are not used to justify the secondary path.
See the contract for why the observed firmware holding loop has no relocated
U-Boot stack/table dependencies.

## Validation

- **102 native ARM64 library tests passed**.
- Bare-metal build, `cargo fmt --check`, and `git diff --check` passed.
- Positive hardware validation: profile recognition, holding-page reservation
  before allocation, Linux 6.18.2 guest boot and UART command completion.
- Negative hardware validation: remove the reservation from a RAM DTB copy;
  tvisor reports `MissingHoldingReservation` before allocator initialization.
- Normal guest Image reused unchanged, SHA-256:
  `3a72c904e211f56e5f05d601b41495dfa4ec9029b3d7b1ee3556a72f9bb33b15`.
- Tested final tvisor ELF SHA-256:
  `6bf1a64e3d646b27079bfdb347a2a8e44d9ebc323e87b56b16e4684c78b7cf12`.
- Targeted validation only; no new quick/overnight regression claim.
- lab-rpi4 powered off after validation; relay state read back as off.

Issue [#29](https://github.com/saimizi/tvisor/issues/29), general guest stage-2
protection for the firmware low page, remains deferred by agreement. It is
separate from the verified allocator reservation and present guest mappings.

Next: 11.1 per-CPU state and guarded stacks on the boot CPU, with no secondary
release until the 11.3 assembly entry and readiness boundary are implemented.
