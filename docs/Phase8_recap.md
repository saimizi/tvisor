# Phase 8 recap: post-takeover physical-page allocation

## 1. Starting point

Phase 7 completes the irreversible U-Boot handoff. Tvisor now owns its EL2
stack, vector table, UART access, and stage-1 translation tables, and it has no
return path to U-Boot. Phase 8 turns the normalized physical-memory database
into allocatable 4 KiB pages. Initial tables reside in tvisor's linker-owned
runtime footprint, so allocator initialization is deferred until this point.

Phase 8 does not add a Rust global heap, create guest stage-2 mappings, or boot
a guest. Those are later milestones.

## 2. Goal

Implement a deterministic physical-page allocator that:

- allocates only page-aligned RAM from the validated post-takeover `USABLE`
  view;
- supports multiple discontiguous RAM banks;
- never returns permanent reservations or active tvisor-owned pages;
- detects invalid and double frees according to a documented policy;
- makes allocator-managed RAM accessible through the current EL2 identity
  map; and
- retains the mapped original DTB in place.

## 3. Allocator model

The first allocator uses 4 KiB pages and two bitmaps stored in tvisor-owned
`.bss`. It inventories all CPU-usable RAM at initialization and represents
three states without per-page ownership metadata:

- `Reserved`: the page belongs to RAM, but is permanently unavailable;
- `InUse`: the page is allocator-managed RAM currently owned by tvisor;
- `Unused`: the page is immediately available for allocation.

Both bitmaps cover one flat 4 GiB physical aperture. The managed bitmap
distinguishes allocator-controlled RAM from unavailable addresses. The latter
include reserved RAM, MMIO, firmware carve-outs, and unpopulated space; their
semantic classification remains in `MemoryMap`. The in-use bitmap
distinguishes `InUse` from `Unused` allocator-controlled RAM.

The Raspberry Pi 4 physical layout used by tvisor lies below 4 GiB, requiring
at most 1,048,576 page bits per bitmap, or 256 KiB of total bitmap storage.
The implementation must reject a usable page whose number is outside the
bitmap's configured physical aperture instead of truncating it.

Allocation is deterministic first-fit across the flat bitmap. Unavailable
pages naturally stop a contiguous allocation at every reserved or non-RAM
hole. Free
requires a page-aligned address that belongs to an allocator region and whose
allocation bit is set. Freeing an unallocated page reports a double-free
error; freeing a reserved, unaligned, or out-of-range address reports an
invalid-page error.

## 4. Ownership and reservations

Permanent exclusions include:

- the linked tvisor image, including allocator bitmap metadata;
- private stack and unmapped guard page;
- active EL2 page tables and the complete bootstrap table arena;
- firmware carve-outs and permanent DTB reservations;
- MMIO and device-owned ranges; and
- any future runtime object explicitly retained by tvisor.

U-Boot runtime allocations are not represented in the permanent memory map.
Before takeover, tvisor writes only its fixed, linker-defined runtime interval.
After takeover, former U-Boot RAM is included in `USABLE` without a separate
reclamation pass. The deployment procedure must validate that the complete
tvisor interval `[__image_start, __image_end)` did not overlap U-Boot or
firmware state before entry.

The global FDT handle borrows the original DTB. Tvisor therefore retains its
page-rounded storage and maps it read-only/XN. A later design may copy the DTB
and release these pages.

## 5. EL2 mapping requirement

Phase 7 maps only the bootstrap objects needed to survive takeover. Phase 8
must also identity-map allocator-managed RAM as Normal, Inner Shareable,
read/write, and execute-never before test code dereferences allocated pages.
Large homogeneous ranges may use L1 or L2 block descriptors, while reservation
and attribute boundaries must remain exact.

The complete 64 KiB page-table store is a dedicated, 4 KiB-aligned linker
`NOLOAD` section inside tvisor's runtime footprint. `mm::prepare` explicitly
zeros it and constructs tables there without initializing the allocator.
Descriptor stores are published before the table switch using the existing
barrier sequence.

The live DTB's page-rounded region is identity-mapped as Normal, read-only,
execute-never memory before switching tables. After the no-return boundary,
the post-switch allocator excludes these retained pages. Tvisor validates the
DTB magic and total size through its own mapping, so the original blob remains
usable without copying.

## 6. Implementation structure

- Add a host-testable allocator module under `tvisor_util/` containing bitmap,
  region, allocation, free, statistics, and error logic.
- Extend `scripts/rpi.ld` with the linker-owned bootstrap-table arena.
- Make `src/mm.rs::prepare` construct tables in that arena and move the final
  `MemoryMap` into one-shot tvisor-owned `.bss` storage.
- Initialize the allocator in the post-switch path directly from `USABLE`;
  do not retain references to the U-Boot stack.
- Run allocation/write/read/free validation after the Phase 7 post-switch
  checks.
- Print allocator totals, free-page counts, and selected test pages.

## 7. Acceptance criteria

### Host tests

- allocation and free at every region boundary;
- deterministic allocation across unavailable holes;
- exhaustion and reuse after free;
- invalid, unaligned, reserved, and double-free rejection;
- bitmap-aperture overflow rejection;
- permanent exclusion of the linker-owned table store; and
- statistics consistency after every operation.

### AArch64 build

- `cargo build --target aarch64-unknown-none` succeeds;
- no global heap is introduced;
- allocator bitmaps and control state are inside mapped tvisor-owned data; and
- every allocator region has a Normal RW/XN EL2 mapping.

### Raspberry Pi 4

- construct the page tables without initializing the allocator;
- initialize the allocator only after the private page-table switch;
- allocate pages from eligible low and high RAM banks;
- write and read known patterns through their identity mappings;
- free and reallocate test pages deterministically;
- prove no returned page intersects permanent reservations or the table arena;
- retain the original DTB reservation; and
- keep UART output and EL2 exception handling operational throughout.

Every hardware run begins from a fresh boot and ends with the board in a known
safe power state.
