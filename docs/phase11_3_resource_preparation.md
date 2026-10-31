# Phase 11.3 boot-side resource preparation

Task 8, 2026-10-31. Production resource validation and the table mutation gate
are in [`startup_resources.rs`](../tvisor_util/startup_resources.rs). The draft
orchestrator in [`src/secondary.rs`](../src/secondary.rs) now uses them; boot
activation remains Task 10.

## Target and prepared resources

The lowest enabled nonzero logical ID is selected from the owned topology.
Structural validation rejects unsupported startup methods, unaligned release
cells and any release cell shared with another described CPU, including a
disabled CPU. A failed candidate is not silently replaced by a different backend
or CPU. These existing production selectors and their tests remain in use.

The selected slot must have published preparation, matching logical ID, present
flag, null active-vCPU pointer and zero saved world-switch SP. Its full affinity
must agree with the topology. CPU 0 checks active MAIR/TCR/root/SCTLR against the
constructed startup policy before preparing release; it does not use arbitrary
live registers as startup policy input.

CPU 0 snapshots prepared stack bounds before taking the table lock. Inconsistent
bounds, guard underflow and capacity errors return an error. All prepared stack
guards, including CPU 0 and unselected secondaries, are revalidated in the final
critical section. The selected stack must retain its allocated 64 KiB usable
range and guard. The existing Phase 11.1 allocator/preparation path owns these
allocations; no new allocation or reclamation is introduced by this task.

## Release-page ownership and mapping

The release cell must appear in the captured startup-memory contract. The full
containing 4 KiB page must be inside described RAM, wholly reserved, outside
usable RAM and outside MMIO according to the published finalized memory map.
A partially reserved page is explicitly unsupported by this backend: it is
rejected rather than exposing memory with unestablished ownership as writable.
An aligned cell at physical zero remains valid.

`map_firmware_release_cell` creates or accepts only a host identity RW/NX Normal
mapping and completes descriptor publication/local TLB maintenance. The final
validator checks the complete page again. Normal descriptors use the table
builder's existing Inner Shareable encoding. There is no stage-2 table operation
or guest mapping in this path.

Before freezing, `verify_unreleased_cell` checks captured-cell membership,
identity read translation and a zero cell value, using assembly rather than a
Rust reference at PA zero. The irreversible release helper still must perform
its own immediate zero recheck (Task 9).

## Resource checks and freeze

`StartupResources` names the complete text, vector, rodata, writable-image and
preallocated table-arena ranges; the actual record wrapper and selected context;
the selected stack/guard; entry/root addresses; and the release page. All region
arithmetic uses checked `PhysRegion` construction.

The production validator checks:

- Page alignment and separation of image sections, table arena and release page.
- An aligned entry inside text and a vector region large enough for the table.
- Full record/context containment in the writable image, exact object sizes,
  64-byte alignment and non-overlap.
- Correct selected stack geometry and no overlap of its allocation/guard with
  the above sections or release page.
- The policy root equals the actual `TableSet` root at the start of the table
  arena, with room for the complete fixed-capacity arena.
- Every covered mapping, including an object's last byte across a page boundary,
  is identity Normal memory with exact required writable/executable permissions.
- Text/vectors are RX; rodata is R/NX; writable objects, tables, stacks and release
  page are RW/NX. Stack guards are unmapped.

The old separate atomic frozen flag has been replaced by `BootTables<TableSet>`
inside the existing IRQ-save table lock. All normal writers obtain the table
through `edit`, which rejects access after freezing. Validation and freezing run
under one acquisition of that same lock. Immutable views remain available;
there is no thaw API. Failed validation does not freeze the table set, and a
second freeze is rejected. Initial table installation remains once-only through
its existing initializer claim.

This is prevention of mapping changes, not a cross-core TLB shootdown protocol.
Cache publication and final barriers remain Task 9. The final lock is released
before cache cleaning, firmware release, waiting or logging. There is no nested
allocator, UART or heap lock inside validation.

## Failure, lifetime and remaining integration

Preparation errors prevent the firmware write. The draft publishes its one-shot
record before the final resource check; if that check fails, the record remains
claimed and allocations remain retained. No retry, rollback or failed-record
reuse is added. Task 10 must make every failed attempt terminal in the actual
boot path. The resource validator establishes layout/mapping prerequisites; it
does not establish firmware entry/coherency or code/data cache publication.

The `EntryContract` API still contains caller-supplied writable/published fields
that the old draft does not consume through its full release validator. Task 9
must reconcile those with the actual preparation/publication results instead of
hard-coding them. Task 8 does not grant release approval or activate boot startup.

## Local verification

155 hosted tests pass. Five new tests use the actual table builder, walker,
resource validator and mutation gate to cover:

1. Valid resources, successful freeze, mutation rejection and duplicate freeze.
2. Incorrect permissions, Device memory and non-identity mappings.
3. Incorrect root/layout, overlapping or out-of-bounds objects, mapped guards,
   and failed-validation preservation of the editable state.
4. A record crossing into an unmapped second page.
5. Full-page firmware ownership, PA zero, unaligned cells, partial reservations,
   unreserved RAM and overflowing page bounds.

Debug/release builds and linked-entry checks pass, as do formatting/whitespace
checks. Startup remains inactive, producing unused-path warnings. No hardware
was operated. Final publication/boot integration and hardware acceptance remain
Tasks 9–14.

## Task 9 follow-up

The [publication pipeline](phase11_3_publication.md) now consumes a FrozenStartup
handle from the actual resource-validation/freeze operation. The unused
writable/published contract booleans mentioned in the Task 8 snapshot above are
removed, and the production backend/target validator is used before cleaning.
Boot activation and overall terminal-result integration remain Task 10.
