# Phase 11.3 secondary local validation

Task 7, 2026-10-31. Production validation and checkpoint publication are in
[`tvisor_util/secondary_validation.rs`](../tvisor_util/secondary_validation.rs).
The hardware register collector and CPU 0 reporter are in
[`src/secondary.rs`](../src/secondary.rs). Hosted tests execute that production
validator and use the real startup record and mailbox.

## Secondary execution and ownership

The secondary first wins `mailbox.enter(record.generation)`. A rejected entrant
immediately holds in WFE without touching diagnostics. The winning entrant
collects EL, full MPIDR, SP, TPIDR, VBAR, MAIR/TCR/root/SCTLR, DAIF, HCR/CPTR/HSTR,
VTTBR, CNTVOFF/CNTHCTL, CNTHP/CNTP/CNTV timer controls and SPSel. Entry SCTLR,
MIDR, CPUECTLR and MPIDR are the values assembly preserved before enabling MMU.

Before dereferencing the per-CPU context, `validate_secondary_context` checks
that the logical ID selects a non-boot static slot, that it was prepared, and
that TPIDR matches that slot's actual address. The owner-only context validator
then checks present/logical ID/full affinity, identical stack bounds, zero saved
world-switch SP and a null active-vCPU pointer. It never dereferences a vCPU or
accesses VM/emulation objects. The same context identity predicate is host-tested.

`boot::memory_map()` is acquired through its published immutable API. Absence is
a validation failure. There are no allocator, UART, page-table or heap operations
on the secondary path. Register collection and validation neither unmask DAIF
nor modify controls. Both success and failure enter the permanent WFE loop.

## Validation and diagnostics

All checks run; a failure mask retains simultaneous defects instead of returning
only the first mismatch. Bit positions correspond to the following stable names:

| Bits | Failure names |
|---|---|
| 0–6 | Generation, ExceptionLevel, Affinity, Stack, ContextPointer, ContextIdentity, Vectors |
| 7–13 | Mair, Tcr, Root, Sctlr, InterruptMasks, Hcr, Cptr |
| 14–20 | Hstr, Vttbr, CounterOffset, CounterAccess, HypTimer, PhysicalTimer, VirtualTimer |
| 21–27 | StackSelection, MemoryMap, EntryRegime, CpuImplementation, MaintenanceParticipation, EntryAffinity, LogicalId |

SP must be 16-byte aligned and inside `[bottom, top)`; top is excluded for the
actual Rust checkpoint. VBAR must match the linker vector address and be 2 KiB
aligned. Identity checks use the complete MPIDR affinity mask, not low CPU bits.
Logical ID must be nonzero and below capacity.

Translation checks compare readbacks with both the immutable startup record and
the shared host policy. TCR is reconstructed with `HostTranslation` and must
match the full policy value, including physical size; root must match exactly.
MAIR/SCTLR cannot pass merely because an incorrect readback matches an incorrect
record. HCR/CPTR/HSTR/VTTBR/counter controls match their named policy values.
All four DAIF mask bits must remain set and SPSel must select SP_EL2.

Timer checks compare ENABLE and IMASK with the disabled policy while retaining
the full control readbacks for reporting. Read-only ISTATUS is not required to
be zero when disabled; Arm documents that value as unknown in that state. See
the timer control descriptions in the official
[Arm system-register reference](https://documentation-service.arm.com/static/6166bf63e4f35d248467c9c0).

Saved entry SCTLR must have M/C/EE clear; MIDR must identify Arm Cortex-A72;
CPUECTLR must have SMPEN set; saved MPIDR must match the selected affinity.
The full MIDR, including variant/revision, is retained for bring-up/errata review.
These observations corroborate the backend contract; they do not independently
prove cache history, system coherency or the correctness of live mappings.

## Publication, failure and reporting

The entrant writes the complete local and entry checkpoints and failure mask
with relaxed atomic stores. Zero failures permits the existing release/acquire
Arrived transition, then Parked. Any failure publishes Failed without arrival.
CPU 0 acquire-observes Parked or Failed before reading the diagnostic fields.
It prints all collected state, the numeric mask and every named failed check.
The secondary does not format or log messages.

Diagnostics are immutable after worker terminal state. The first entrant owns
them; duplicate entry is rejected before writes. A secondary that arrives after
boot timeout may still publish diagnostic evidence, but cannot replace the
boot-owned terminal result or authorize resource reuse. CPU 0 reports worker
state without treating an in-progress Entered checkpoint as complete.

## ABI change

The fixed assembly header, mailbox and logical-ID/stack-bottom offsets are
unchanged. The local checkpoint grows from 96 to 160 bytes at offset 104.
Entry observations move to offset 264 (32 bytes); the failure mask is at 296.
Record size is now 320, aligned to 64. The claim flag is at 320 in a 384-byte
storage wrapper. Compile-time assertions enforce the layout. The linked checker
now uses the actual storage symbol size rather than assuming the older size.
See the [current ABI table](phase11_3_startup_abi.md#record-layout).

## Verification and boundaries

150 host tests pass. Six new tests cover the complete valid validator path,
individual local/entry failures, combined failures, stack boundaries and timer
ISTATUS handling, release/acquire failure visibility after a boot timeout,
incorrect-record policy agreement, and the actual context identity predicate.
Existing production protocol and storage tests remain passing.

Debug/release builds and linked-entry checks pass; formatting and whitespace
checks pass. Startup remains inactive and still produces unused-path warnings.
No hardware was operated. Tasks 8/9/10 still owe resource/publication/boot
integration, and actual register observations, CPU revision/errata assessment and
parked-secondary guest regression remain hardware acceptance gates.
