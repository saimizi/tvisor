# Phase 11.3 cache publication and release

Task 9, 2026-10-31. The production pipeline is in
[`startup_publication.rs`](../tvisor_util/startup_publication.rs), with hardware
callbacks in [`src/smp.rs`](../src/smp.rs) and the five small retained assembly
primitives in [`secondary_publication.S`](../src/secondary_publication.S).
Boot still does not invoke secondary startup; that is Task 10.

## Readiness comes from completed operations

The former `EntryContract.release_cell_writable` and
`.startup_memory_published` booleans have been removed. The remaining two fields
describe only the documented platform entry/coherency contract. They remain
backend evidence obligations, not facts inferred by cache cleaning.

The resource and publication stages now return consuming handles:

1. `BootTables::freeze_startup` validates the real resources and all supplied
   stack guards, freezes the table set under its existing lock, then returns a
   `FrozenStartup` containing immutable resource metadata. No public constructor,
   Clone or thaw exists for that handle.
2. `PublicationPlan::new` consumes it, binds the release cell and cache geometry,
   and checks all rounded ranges before maintenance begins.
3. `publish` checks the backend/target with the shared release validator, binds
   the actual production record to the plan, cleans all publication ranges,
   and completes DSB SY. Only then can it return `PublishedStartup`.
4. `PublishedStartup::release` consumes the only release handle, rechecks the cell
   and performs the write/clean/barrier/event sequence. Success or failure consumes
   the handle; no retry operation is provided.

Record checks require the matching logical ID/full affinity, Ready worker state,
Pending outcome, nonzero matching ready generation, actual record address,
context/root/stack correspondence and the named translation policy. The hardware
callback's record-address calculation is the actual identity VA/PA address;
hosted tests use an explicit modeled physical address while exercising the same
production record and protocol.

The frozen handle certifies table validation at freeze time. It cannot by itself
keep physical allocations alive or establish firmware ownership/coherency.
The hardware backend constructor remains unsafe: the caller must retain the
validated resources and use the active checked identity regime and reserved
release page. `start_one` supplies those conditions through the Task 8 preparation
path. Backend selection and final boot ownership remain Task 10.

## Cache geometry and coverage

CPU 0 derives the minimum data-cache line size from CTR_EL0.DminLine. Accepted
sizes are powers of two from 8 bytes through the 4 KiB mapping granule; unsupported
geometry is rejected rather than allowing rounded maintenance into an unvalidated
neighboring mapping. This includes the supported A72's normal geometry.

The iterator rounds start down and end up with checked arithmetic. It covers all
intersecting lines, including partial/shared lines, and rejects rounding overflow.
Every planned parent interval is page-aligned, so accepted line sizes keep all
rounded addresses inside already validated mappings. An 8-byte-aligned release
cell fits within one accepted line; PA zero requires no Rust reference.

| Cleaned interval | Dependencies covered |
|---|---|
| Complete `.text` | Trampoline, Rust secondary code and linked publication primitives |
| Complete `.vectors` | Private EL2 vector instructions |
| Complete `.rodata` | Constants used by the secondary |
| Complete writable image | Record/wrapper and ready word, prepared contexts, published topology/map and their static metadata |
| Complete fixed table arena | Root and all preallocated translation-table pages |
| Selected usable stack | Initialized stack data; never the unmapped guard |

The record/context are cleaned through their whole writable parent section,
including neighboring shared cache lines. The primary stack and heap data are
not secondary dependencies in this milestone; secondaries do not use them.
No ELF-header page, unmapped guard, arbitrary RAM scan or release-page code is
included in the startup-data clean plan. The release cell is cleaned separately
only after its store.

Each line uses DC CVAC, followed by a completion DSB SY for the complete startup
publication. This matches the supported firmware/cache contract's PoC obligation.
The [AArch64 boot interface](https://docs.kernel.org/arch/arm64/booting.html)
requires code publication to PoC, a single 64-bit little-endian spin-table value,
and event-based wakeup. The secondary's local I-cache/TLB maintenance remains
part of the reviewed entry path. Cache cleaning does not repair an unsupported
firmware entry regime, dirty secondary cache history or an external cache that
fails the documented platform contract.

## Irreversible release ordering

The exact production order is:

```text
record release-publication and validated table freeze
    → all planned DC CVAC operations
    → DSB SY
    → read release cell and require zero
    → one aligned 64-bit little-endian store of entry PA
    → DC CVAC of that cell's cache line
    → DSB SY
    → SEV
```

The hardware uses assembly for both the read and write, including PA zero.
The Rust adapter converts between the native integer and little-endian value;
Task 8 already checks the active little-endian host policy. No publication or
release operation occurs under the table, allocator or UART lock. The final
release handle does not map memory, allocate or log.

A nonzero cell returns before the store. A maintenance/completion failure cannot
produce a release handle. A failure after the store is potentially already a
physical release, even if SEV was not reached: resources must remain retained
and reset is required. The pipeline never clears the cell, rolls back, retries,
performs CPU_OFF or switches to PSCI. Task 10 must record failures in the overall
boot result and halt the guest path. The actual hardware primitives return no
recoverable cache/store error; a synchronous fault follows the existing fatal
exception path.

## Validation

160 host tests pass. Five new production-pipeline tests cover rounded/shared
lines, invalid geometry and overflow; exact publication/release ordering and
range coverage including PA zero; backend/target/record/state mismatches with no
maintenance or write; failures during cleaning and the publication barrier;
and nonzero-cell plus read/store/clean/barrier/event failures with no retries.
They use the real table builder, frozen-resource operation, startup record and
consuming pipeline, with event-recording hardware callbacks.

Debug/release builds pass. The linked-entry checker now also checks each retained
publication primitive has exactly its intended instructions and RET, lies in RX
text and has no stack access. Both debug/release checks pass. The consuming Rust
pipeline's ordering is covered by the host callback traces; the final integrated
binary will be rechecked in Task 11. Formatting and whitespace checks pass.
Inactive startup paths still produce warnings. No hardware was operated and no
secondary release gate was enabled for Task 9.

## Task 10 integration update

The release-store primitive now samples CNTPCT immediately before STR and returns
that timestamp for the validated deadline. The checker verifies this extended
sequence. [Boot integration](phase11_3_boot_integration.md) now calls the consuming
pipeline and gates guest entry on successful matching parking. Earlier Task 9
references to inactive startup describe that task's snapshot.
