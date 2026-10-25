# Phase 11.0 implementation and board evidence

Status: **DT boot-interface and startup-memory validation implemented**,
2026-10-25. Secondary activation remains disabled. Firmware instruction matching
and the embedded armstub reference were removed following PR #28 design review.
Firmware updates preserving the DT-described interface and required reservations
do not require updating an embedded firmware binary.

See the [secondary startup interface](rpi4_secondary_startup.md) for runtime
checks, protocol assumptions, diagnostic observations and later entry-state gates.

## Implemented

- Allocation-free owned CPU descriptions, full MPIDR affinity matching,
  deterministic logical IDs with boot CPU zero, disabled-CPU recording,
  exact cell decoding and malformed/duplicate/capacity rejection.
- Explicit PSCI binding/conduit descriptions and version-dependent CPU_ON
  capability planning. This board advertises no PSCI interface; no speculative
  firmware calls or backend fallback are performed.
- DT-selected spin-table cells: validate alignment, RAM/reservation coverage,
  inherited read mapping and initial zero values. Shared cells and relocated
  addresses are supported; no exact CPU-affinity or firmware-byte pattern is
  required by startup-memory validation.
- Before allocation, verify that every DT reservation-block range remains
  reserved and outside usable RAM, including holding-code reservations separate
  from the release cells. Preserve existing `/reserved-memory` handling.
- No CPU_ON, release-cell writes, secondary trampoline, firmware instruction
  reads, Cortex-specific SMPEN gate, or allocator reclamation policy change.

## Board observations

lab-rpi4 advertised four enabled affinities 0..3, boot MPIDR `0x80000000`,
spin-table cells `0xd8`/`0xe0`/`0xe8`/`0xf0`, and a reserved first 4 KiB.
Firmware identification/disassembly from the earlier investigation remains
optional diagnostic evidence. The runtime trusts the documented DT boot
interface and does not claim to measure parked CPUs' registers.

## Validation

- **103 native ARM64 library tests passed**, including DT-selected/shared cells,
  missing/partial/overflowing reservations, RAM boundaries, nonzero/unreadable
  cells, separate holding-code protection, allocator exclusion and PSCI with no
  invented release-cell reads.
- Bare-metal build, formatting and whitespace checks passed.
- lab-rpi4 positive test passed: DT cell checks, pre-allocation reservation
  checks, Linux 6.18.2 guest boot, and UART command completion with status zero.
- Negative hardware test passed: removing the reservation from a RAM-only DTB
  copy produced `MissingHoldingReservation` before allocator initialization.
- Evidence is in `docs/evidence/pr28_spin_table_interface/`; no quick/overnight
  regression claim is made. No SD files or saved environment were changed.
- Normal guest Image reused unchanged, SHA-256:
  `3a72c904e211f56e5f05d601b41495dfa4ec9029b3d7b1ee3556a72f9bb33b15`.
- lab-rpi4 powered off after validation; relay state read back as off.
- Revised tvisor ELF SHA-256:
  `f091aac5c5059f4b15cc4f3d63a57162d687b9c516e2785fbff33cf25d25505e`.

Issue [#29](https://github.com/saimizi/tvisor/issues/29) remains deferred.
[11.1 per-CPU state and guarded stacks](phase11_1_status.md) is now implemented
and has passed the quick regression on the boot CPU. Actual secondary EL2,
MMU/cache/coherency validation and the release-publication implementation remain
11.3 gates; DT interface validation alone does not satisfy them.
