# Phase 11.3 startup ABI and ownership

Task 3, 2026-10-31. The production definitions are in
[`tvisor_util/secondary_record.rs`](../tvisor_util/secondary_record.rs) and
[`secondary_mailbox.rs`](../tvisor_util/secondary_mailbox.rs). Bare-metal startup
and hosted tests use those same types. This document fixes the current data
layout, updated by Task 4 to serialize worker transitions in one atomic word.

## Record layout

`SecondaryRecord` is `repr(C, align(64))`, size 320 bytes (Task 7 extension). All scalar fields and
atomic words are 64 bits. Offsets below are bytes from the record base.

| Offset | Contents | Writer |
|---|---|---|
| 0 | Full affinity | Boot initializer, before publication |
| 8 | Stack top, exclusive bound; initial SP | Boot initializer |
| 16 | Stable per-CPU context address | Boot initializer |
| 24 | MAIR_EL2 | Boot initializer |
| 32 | TCR_EL2 | Boot initializer |
| 40 | Shared root physical address | Boot initializer |
| 48 | SCTLR_EL2 policy | Boot initializer |
| 56 | Nonzero startup generation | Boot initializer |
| 64 | Mailbox ready generation, atomic | Boot initializer, release publication |
| 72 | Worker state: 0 Ready, 1 Entered, 2 Arrived, 3 Parked, 4 Failed; atomic | Selected secondary |
| 80 | Terminal outcome: 0 Pending, 1 Success, 2 TimedOut, 3 SecondaryFailed, 4 PollLimit, 5 InvalidDeadline, 6 BootAborted; atomic | Boot CPU |
| 88 | Logical ID | Boot initializer |
| 96 | Stack bottom, inclusive bound | Boot initializer |
| 104–199 | Checkpoint: EL, MPIDR, SP, TPIDR, VBAR, MAIR, TCR, root, SCTLR, DAIF, HCR, CPTR; one atomic word each | Selected secondary |
| 200–263 | Additional local HSTR, VTTBR, CNTVOFF, CNTHCTL, CNTHP_CTL, CNTP_CTL, CNTV_CTL, SPSel; atomic words | Claimed secondary |
| 264–295 | Entry observations: SCTLR, MIDR, CPUECTLR, MPIDR; atomic words | Claimed secondary |
| 296 | Validation failure bitmask; atomic | Claimed secondary |
| 304–319 | Alignment padding; no protocol content | None |

Every assembly-consumed record field has a compile-time offset assertion.
The mailbox asserts its internal word offsets; the record asserts total size,
alignment and embedded-object offsets. `global_asm!` operands derive offsets
with `offset_of!` from the production type instead of duplicating numeric offsets.
The ready word is the first mailbox word, so the acquire address is
`record + offset_of!(SecondaryRecord, mailbox)`.

`RecordStorage` is a separate `repr(C, align(64))` wrapper: the record is its
first field at offset zero, followed by a private claim flag at offset 320.
Wrapper size is 384 bytes. These offsets/sizes are asserted too. The exported
`SECONDARY_RECORD` symbol names this wrapper, so its base is also the assembly
record base. The claim flag is outside the 320-byte record ABI.

## Publication and reference rules

1. The boot CPU prepares resources and passes ordinary values in `StartupRecord`.
   No address in that temporary input object is published. Address values must
   refer to retained resources; physical mapping/permission checks remain the
   release orchestrator's responsibility.
2. `RecordStorage::publish` rejects zero generation, out-of-capacity or boot
   logical ID, invalid affinity bits, invalid 64 KiB stack bounds/page alignment,
   misaligned/null context, and misaligned/null root. Invalid input does not
   consume the storage. The context contract is 64-byte alignment.
3. One atomic claim wins initialization. Later valid attempts fail with
   `AlreadyClaimed`, even after terminal timeout. There is no reset/reuse API.
4. The initializer writes ordinary fields through individual raw field pointers.
   It never constructs a mutable reference spanning concurrently readable atomics.
   Finally it release-publishes the ready generation in the embedded mailbox.
5. `get(expected_generation)` first acquire-loads the privately addressed ready
   word. It returns a whole-record immutable reference only on a nonzero match.
   The reference lifetime is tied to storage. Safe callers cannot access the
   unpublished mailbox or fabricate publication through the storage API.
6. After publication ordinary fields never change. Protocol/checkpoint fields
   use atomic interior mutation. A secondary crosses the assembly acquire
   boundary before ordinary shared Rust access. The backend must establish
   coherency and complete physical cache publication before release; a Rust
   release/acquire pair alone does not establish the hardware entry contract.

The bare-metal wrapper is static. Its stack allocations, per-CPU contexts and
tables must remain alive through success, failure, late arrival and guest
execution. It must not be moved, dropped or recycled while hardware may use it.
Hosted tests may use scoped storage because no hardware pointer escapes.
There is no supported transient-stack record publication to firmware.

## Per-CPU preparation is separate from release

`pcpu::CpuPreparation` publishes one boolean: the slot's identity, bounds and
resources have been initialized. It is not evidence that a CPU was released,
entered, parked or became Online. It replaces `CpuStartupState`; the unused
per-CPU generation/request/acknowledgement fields and speculative lifecycle enum
have been removed. All startup generations and handoff acknowledgements now
belong to the single record/mailbox protocol.

The selected secondary owns mutable context and stack after release. CPU 0 may
consume immutable published identity and atomic evidence; it must not borrow
or mutate a secondary's whole context while it executes. VM/vCPU ownership is
unchanged. Task 7 still owes complete local-state checks and detailed failure diagnostics.

## Validation

- 134 hosted library tests pass on `aarch64-unknown-linux-gnu`.
- Four new production-record tests cover invalid input without consuming the
  claim, one-shot retention after timeout, concurrent readers seeing all ordinary
  fields after publication, and competing initializers with exactly one winner.
- Existing mailbox and preparation tests pass against the production types.
- Bare-metal build and formatting pass; inactive startup still produces warnings.
- Linked entry inspection confirms offsets 56 (generation), 64 (ready acquire),
  the other record loads, LDAR and the call to `secondary_main` retain their ABI.
  This scoped inspection is not the full Task 6/11 architecture/binary review.
- No hardware release or change to the boot activation gate is part of Task 3.

## Task 4 protocol transitions

The 24-byte embedded mailbox replaces separate arrived/parked generation words
and the redundant checkpoint failure flag. Each transition validates the supplied
nonzero generation against the one-shot ready generation. Generation is never
packed/truncated into a state word, so full-width generations remain valid.

| Operation | Required worker state | Result | Publication |
|---|---|---|---|
| `publish(g)` | Unpublished, ready=0 | Ready for generation g | Release-store immutable record via CAS |
| `enter(g)` | Ready | Entered | Acquire/release CAS; claim checkpoint writer |
| `acknowledge(g)` | Entered, local validation succeeded | Arrived | Acquire/release CAS publishes checkpoint |
| `park(g)` | Arrived | Parked | Acquire/release CAS; immediately precedes WFE loop |
| `fail(g)` | Entered, local validation failed | Failed | Acquire/release CAS publishes failure checkpoint |

Entered means that the secondary crossed the coherent Rust boundary. It is not
an acknowledgement of raw physical entry. Before safe shared-memory access,
unsupported entry still relies on boot timeout evidence; assembly rejection
instrumentation belongs to Task 6. Failed here means Rust local validation
failure; detailed validation reasons and additional observations belong to Task 7.

A single atomic worker state prevents skipped, duplicate and conflicting
transitions. Failed and Parked have no outgoing worker transition. A second
entrant is rejected before writing checkpoint fields, preserving the first
entrant's evidence. Acquire-observing Arrived/Parked or Failed makes the preceding
relaxed checkpoint stores visible. The production waiter prints a checkpoint
only after acquiring Parked or Failed, not while Entered may still be writing.

The boot CPU calls `wait_and_finish`, which both waits and compare-exchanges the
terminal outcome. Invalid generation leaves all state untouched. A zero or
ambiguous half-counter-range duration becomes InvalidDeadline; reaching the
measured deadline becomes TimedOut; exhausting the finite poll budget becomes
PollLimit. A valid Failed observation becomes SecondaryFailed. A Parked
observation is bracketed by physical-counter reads and can become Success only
if both are before the deadline. Deadline expiry takes priority when already
observed; a late failure can remain diagnostic evidence after a TimedOut result.

`finish` rejects Pending, unvalidated success, and unsupported SecondaryFailed
claims. Only the first terminal CAS wins; repeated waits/decisions are rejected.
The Rust API relies on the documented boot/worker ownership, rather than
attempting to identify the executing CPU inside the hosted protocol library.

Worker evidence may advance after boot timeout (including late entry, arrival,
park or failure). It never changes the terminal result, permits a retry, clears
the ready generation or authorizes reclamation. No Online state is present.
The boot caller must halt the guest path on every unsuccessful result; that
call-site integration remains Task 10. The complete hardware deadline origin
and conversion policy also remain part of boot integration.

Task 4 validation: **142 host tests pass**. Eleven mailbox tests cover invalid
and duplicate transitions, stale/zero generations, failure, terminal-result
preservation, concurrent terminal decisions, deadline wraparound and boundary
observations, invalid durations, finite polling, full-width generations and a worker/deadline race. Two new production-record
tests check park/failure checkpoint visibility through the embedded protocol.
These replace the five older mailbox tests; Task 3's four record tests remain.
Bare-metal build and formatting pass. No hardware release was performed.

## Task 6 entry observations

At the Task 6 snapshot, `EntryCheckpoint` occupied offset 200, size 32, within
a 256-byte record and 320-byte wrapper. Task 7 expands these as shown in the
current layout table above. Assembly preserves original SCTLR/MIDR/CPUECTLR/
MPIDR in x21–x24 and passes them as x1–x4 to `secondary_main(record, ...)`.
Rust claims Entered before storing them, so rejected duplicate entrants cannot
overwrite the first checkpoint. Arrived/Failed publication also publishes these
observations. Task 7 will validate/report the full checkpoint.

## Task 7 local validation

The [local validation specification](phase11_3_local_validation.md) defines
additional readbacks, 28 named failure bits, acquire-visible diagnostic reporting
and the extended record layout. Physical release, mappings and boot orchestration
remain separate gates.

## Task 10 terminal boot integration

The [boot integration specification](phase11_3_boot_integration.md) adds
BootAborted outcome 6 without changing mailbox size or offsets. A separate
one-shot boot-attempt state owns final guest permission, including errors before
a record is published. Physical deadline origin is now captured before the
release store; the hardware wait uses wrap-safe low-56-bit elapsed ticks.
