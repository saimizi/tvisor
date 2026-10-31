# Phase 11.3 design: one secondary CPU enters tvisor

Status, 2026-10-31: implementation and scoped acceptance complete; ready for user
review. See the [completion and evidence handoff](phase11_3_status.md).
This document refines
milestone 11.3 of [the SMP plan](tvisor_smp.md#113-secondary-entry-and-tvisor-owned-holding-loops).
It specifies the implemented milestone contract and its completion boundary.

Task 10 now [connects the startup path to boot](phase11_3_boot_integration.md).
Supported-board builds attempt one secondary release before Linux. Physical
arrival, deliberate failure and the full quick regression have passed on lab-rpi4.

## 1. Goal and completion boundary

CPU 0 brings exactly one enabled secondary from firmware into tvisor's private
EL2 execution environment. The secondary validates its identity and local state,
acknowledges the requested startup generation, and parks in a tvisor-owned loop
with interrupts masked. CPU 0 then boots the existing one-vCPU Linux guest.

Completion requires a real lab-rpi4 arrival and parking checkpoint, a bounded
failure test, and guest regression with that secondary parked. Host tests and a
linked trampoline alone do not satisfy this milestone.

Other eligible CPUs remain in firmware. Management SGIs and IRQ unmasking belong
to 11.4; all-core startup and allocator contention on hardware belong to 11.5;
mapping changes after secondary release and TLB shootdowns belong to 11.6.
There is no scheduler, guest SMP, or vCPU migration in this milestone.

## 2. Backend and entry contract

See the [Task 2 backend contract](phase11_3_backend_contract.md) for the
supported lab-rpi4 profile, evidence and firmware trust boundary.

The initial backend targets the DT-described spin-table interface on lab-rpi4.
Select an enabled logical CPU other than CPU 0 by its full MPIDR affinity; do
not derive logical IDs from the low MPIDR bits. For deterministic testing, choose
the lowest eligible nonzero logical ID. If none exists, report unavailable; this
is not a successful 11.3 hardware test.

The backend must establish these prerequisites before writing a release cell:

| Requirement | Evidence or validation |
|---|---|
| Target and firmware memory ownership | Owned topology, aligned DT release cell, preserved DT reservations, finalized memory map |
| Unique release cell | No other described CPU, including a disabled CPU, uses that cell |
| Physical entry contract | Documented backend contract for AArch64 EL2, MMU off, compatible endianness and cache state |
| Coherency domain | Documented platform/firmware contract establishing coherent shared-memory access |
| Writable release cell | Validated host EL2 mapping, initial zero value, correct memory attributes |
| Startup resources ready | Validated private stack, context, frozen tables, immutable startup record, code/data publication |

The [spin-table boot interface](rpi4_secondary_startup.md) establishes how firmware
observes a release address. A DT property alone does not prove every execution
or coherency prerequisite. The Cortex-A72 identity and SMPEN checks in the
trampoline are defensive checks; SMPEN alone is not proof of system-level
coherency. Never set an `EntryContract` boolean to true merely because code needs
it to proceed. Record the justification for each prerequisite in bring-up
evidence. Existing firmware disassembly may inform that investigation without
becoming a runtime binary allowlist.

The trampoline rejects non-EL2, MMU-on, data-cache-on and big-endian entry. Unsupported
entry must park before stack use or ordinary Rust access. If lab-rpi4 requires
another entry regime, specify and review an adapter rather than relaxing the
checks without a maintenance argument. Reads of implementation-defined registers
must be permitted by the backend contract and preceded by CPU identification.

PSCI is not a fallback for a failed spin-table attempt. It needs its own
EL2-callable conduit, CPU_ON and entry contract before use. Shared release-cell
activation is deferred: a one-record trampoline cannot safely route multiple
released affinities to separate stacks.

## 3. Startup data and ownership

The [Task 3 ABI and ownership specification](phase11_3_startup_abi.md) records
the integrated production layout and publication API.

Use one authoritative startup protocol. The production `SecondaryRecord` embeds
`SecondaryMailbox`; tests exercise that assembly-visible storage and its
publication lifecycle. Preparation metadata carries no duplicate handoff words.

The boot CPU owns preparation and release. After publication, the record's
ordinary fields are immutable; the secondary owns its private stack and mutable
per-CPU context. Keep only protocol/diagnostic words atomic. Returning an immutable
record reference is permitted only after its publication, and must not alias a
mutable whole-record reference used during preparation.

Required immutable contents:

- Logical ID and full target affinity.
- Stack bottom/top and stable per-CPU context address.
- MAIR_EL2, TCR_EL2, shared root PA and explicit SCTLR_EL2 policy.
- A nonzero one-shot startup generation.

Required handoff/evidence contents:

- Ready generation, release-published by CPU 0.
- Entry/arrival/park status and acknowledged generation.
- Local checkpoint: CurrentEL, MPIDR, SP, TPIDR, VBAR, translation/control state
  and relevant cache/coherency observations.
- Bounded early failure status where safe startup-memory access is possible.

All assembly-visible offsets, size and alignment require compile-time assertions.
Keep startup storage in reserved static or allocated memory with a lifetime that
extends through success, failure, timeout and guest execution. Ensure physical
and virtual addresses agree across the MMU transition. No transient Rust stack
address may be published. Guard pages remain unmapped in EL2 stage 1.

## 4. Boot CPU sequence

See the [Task 8 preparation specification](phase11_3_resource_preparation.md)
for the resource validator and table-freeze gate, and the
[Task 9 publication specification](phase11_3_publication.md) for consuming
readiness handles, cache coverage and irreversible release ordering.

1. Finish once-only initialization: topology and memory-map publication, physical
   allocator, dynamic secondary stacks, heap, exception vectors and console.
2. Select one eligible target. Validate its entry/backend contract and unique
   release cell before any irreversible firmware write.
3. Establish a host-only RW/NX Normal mapping for the complete release-cell page.
   Validate identity translation and reservation ownership. Reading/writing a
   cell at physical zero must not create a null Rust reference.
4. Finish all EL2 mappings and validate code, record, context, stack and tables.
   Complete local descriptor publication/TLB maintenance, then freeze mappings
   before secondary release. A mutex alone does not provide a shootdown protocol.
5. Populate the startup record with the selected CPU's prepared resources and
   the shared register policy. Use a single nonzero generation for this boot;
   no reset, hotplug retry, or reuse of a failed record is supported.
6. Release-publish the ready generation. Clean the code, record, context,
   required stack data and translation tables to the coherency point required by
   the backend. Account for cache-line rounding and any shared cache lines.
   Complete the required barriers before publishing the physical entry address.
7. Recheck the cell is zero. Store the aligned 64-bit entry in little-endian
   form, clean its cache line to PoC, complete the release barrier and issue SEV.
   This is the irreversible release point.
8. Wait for matching validated arrival and park status using a physical-counter
   deadline. Validate the counter frequency and conversion; handle counter
   wraparound. Use an additional finite poll bound if needed. An iteration count
   alone is not a measured startup timeout.
9. Print the acquired checkpoint from CPU 0, then enter the one-vCPU guest only
   after success. Failure is explicit and stops the startup/guest boot path.

All hardware waits occur outside allocator, page-table and UART locks. Keep
firmware reservations outside the allocator. Mapping a host release-cell page
must not introduce a guest stage-2 mapping; deferred issue #29 remains separate.

## 5. Secondary sequence and Rust boundary

See the [Task 6 entry review](phase11_3_entry_review.md) for the implemented
stackless path, mapping assumptions and linked-binary checks.

```text
Physical entry, DAIF masked
    |
    v
Validate CurrentEL, CPU identity and permitted entry regime
    |
    v
Read only published startup memory; match full MPIDR affinity
    |
    v
Select private SP_EL2, TPIDR_EL2 and exception vectors
    |
    v
Configure EL2 controls, FP/SIMD policy and disabled local timers
    |
    v
Install shared MAIR/TCR/root/SCTLR and complete local maintenance
    |
    v
Acquire-observe matching ready generation in coherent memory
    |
    v
ORDINARY SHARED RUST ACCESS IS NOW PERMITTED
    |
    v
secondary_main: validate local state and publish checkpoint
    |
    v
Publish Parked for the matching generation; enter WFE loop
```

Before the boundary, the assembly may access only its explicitly published
startup code, bounded record, selected private context/stack and translation
tables. It must not clear BSS, parse U-Boot arguments, call Rust helpers, use
allocators or locks, format messages, or access MMIO. Unknown affinity or entry
state parks without using any unvalidated pointer. Before safe memory access,
a timeout may be the boot CPU's only failure evidence.

The [Task 5 shared EL2 policy](phase11_3_el2_policy.md) specifies the common
host baseline and translation builder.

The entry must share the primary CPU's register policy through named constants
or a common configuration builder, rather than copying undocumented live state.
Explicitly address HCR (VM/E2H and trap policy), CPTR/FP-SIMD, HSTR, VTTBR, timer
access/offset/control registers and vectors. Disable unused local timers before
parking. Architectural RES0/RES1 requirements and instruction ordering must be
reviewed against the supported architecture and CPU documentation.

In `secondary_main`, validate EL, full affinity, logical ID/context identity,
SP bounds/alignment, TPIDR, VBAR, MAIR/TCR/root/SCTLR, DAIF and required control
state before acknowledging. The finalized memory map is readable through its
published immutable API. VM/vCPU and boot-owned emulation objects remain
inaccessible. Record failure without allocation; keep interrupts masked.

An arrival acknowledgement means local validation succeeded. Parked is a
separate state published immediately before the permanent holding loop. Do not
publish Online: that requires local GIC and management-handler readiness in
11.4. WFE can return on unrelated events; loop without allocating or logging.

## 6. Failure and lifetime rules

| Failure point | Required behavior |
|---|---|
| Preflight or allocation/mapping validation | Report from CPU 0; do not write the firmware cell |
| Unsupported early entry or wrong affinity | Bounded assembly rejection into a holding loop; no ordinary shared access |
| Rust local-state validation | Publish a failure record/status, then park with interrupts masked |
| Deadline expires | Record timeout and last safe evidence; retain record, stack, context and tables |
| Late arrival after timeout | Do not treat it as success, retry, clear the cell, or recycle any memory |
| Generation mismatch or duplicate acknowledgement | Reject the transition; retain first valid evidence |

CPU 0 owns the overall terminal result. A late secondary must not overwrite a
terminal timeout with a successful outcome. Existing fatal records preserve
first-failure evidence, but a global stop protocol is not implemented by this
milestone. Hardware reset/power cycle is the recovery boundary after release
failure; no CPU_OFF or retry protocol is implied.

## 7. Validation and acceptance

The [Task 11 local acceptance review](phase11_3_local_acceptance.md) records
local gate results. The [Task 12 hardware arrival review](phase11_3_hardware_arrival.md)
records the passing one-secondary cold boot. The [Task 13 failure acceptance](phase11_3_failure_acceptance.md)
records a passing deliberate timeout with late arrival. The [Task 14 quick regression](phase11_3_quick_regression.md)
records the complete passing guest regression with CPU1 parked. The
[Task 15 handoff](phase11_3_status.md) closes the original audit requirements
and indexes the final review bundle.

Host tests must cover unique target selection, disabled/shared-cell cases,
unsupported backends, each missing contract, ABI layout, record validation,
release/acquire data visibility, stale/zero/duplicate generations, bounded waits,
timeout/late-arrival behavior and terminal outcome preservation. Tests should use
the integrated production protocol, not only the draft helper types.

Build and disassembly review must demonstrate a stackless pre-stack path, no
Rust call before the boundary, correct full-affinity matching, explicit register
policy, ABI-aligned private stack and the intended cache/TLB/barrier/acquire
instructions. Account for literal pools and all mappings on both sides of MMU
enablement. Build warnings about unused startup paths indicate incomplete wiring.

On lab-rpi4:

1. Preserve baseline image hashes and inspect current U-Boot layout/DT topology.
2. Cold-boot one target into tvisor; collect its local checkpoint and matching
   arrival/park generation from CPU 0. Verify other CPUs were not released.
3. Exercise a deliberate bounded failure using a documented test mechanism,
   without corrupting active mappings or manufacturing a coherency violation.
   Verify timeout/failure is terminal and resources are retained until reset.
4. Cold-boot the success path again and run the full quick regression with one
   secondary parked: guest boot, console RX/TX, timer progress and memory stress.
5. Retain logs, source provenance, image hashes, thresholds and cleanup evidence;
   independently verify final power off. Never save U-Boot environment or write
   the SD card during these RAM-only tests.

A PASS requires all these gates. It proves one physical secondary can coexist
with the one-vCPU guest in the validated board profile; it does not prove SGI
routing, all-core concurrency or cross-core TLB updates.

## 8. Current implementation and remaining work

The [Task 1 implementation audit](phase11_3_audit.md) records the current
working-tree checklist and verification results as of 2026-10-31. The draft
inventory below predates that audit.

The working tree contains provisional helpers for release preflight, mailbox
publication and bounded polling; a linked stackless entry; startup-record
storage/publication; a host release-cell mapping helper; local arrival checks;
and the low-level cell write/cache-clean/SEV operation. The latest reported
native test count is 127. No secondary has been released or hardware-validated.

Before enabling release, the implementation still needs:

- An evidence-backed backend contract, with unresolved conditions kept explicit.
- One integrated mailbox/record ABI and lifecycle, replacing duplicate draft
  protocol storage and adding the missing checkpoint/terminal states.
- Boot orchestration, complete cache publication, table freeze and physical-time
  deadline, with no irreversible action before all preflight checks succeed.
- Complete local validation and arrival/park/error reporting.
- The hardware success, bounded failure and parked-secondary regression gates.

Relevant files: `src/secondary.rs`, `src/smp.rs`, `src/pcpu.rs`, `src/mm.rs`,
`src/boot.rs`, `tvisor_util/secondary_entry.rs`,
`tvisor_util/secondary_mailbox.rs` and `tvisor_util/pcpu.rs`.
