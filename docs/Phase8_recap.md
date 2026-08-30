# Phase 8 recap: physical-page allocation and boot-memory reclamation

## 1. Starting point

Phase 7 completes the irreversible U-Boot handoff. Tvisor now owns its EL2
stack, vector table, UART access, and stage-1 translation tables, and it has no
return path to U-Boot. Phase 8 turns the normalized physical-memory database
into allocatable 4 KiB pages and releases handoff-only storage at explicit
lifetime boundaries.

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
- reclaims U-Boot pages while retaining the mapped original DTB in place.

## 3. Allocator model

The first allocator uses 4 KiB pages and two bitmaps stored in tvisor-owned
`.bss`. It inventories all CPU-usable RAM at initialization and represents
three states without per-page ownership metadata:

- `Reserved`: the page belongs to RAM, but is permanently unavailable;
- `InUse`: the page is allocatable RAM currently occupied by U-Boot or tvisor;
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

Reclaimable handoff-only regions include:

- U-Boot relocated code, data, heap, stack, and translation tables;
- raw ELF staging bytes outside the live tvisor image; and
- other buffers described by `lmb=` or `bootmem=` arguments.

They become allocator candidates only after:

1. tvisor has crossed the no-return boundary;
2. its private stack, vectors, UART mapping, and translation tables are active;
3. all required platform and memory information has been copied into owned
   structures or explicitly retained;
4. no executing code or dereferenced pointer depends on the reclaimed U-Boot
   regions; and
5. page-rounded reservation boundaries have been handled conservatively.

The global FDT handle borrows the original DTB. Tvisor therefore retains its
page-rounded storage and maps it read-only/XN while other U-Boot handoff pages
are reclaimed. A later design may copy the DTB and release these pages.

## 5. EL2 mapping requirement

Phase 7 maps only the bootstrap objects needed to survive takeover. Phase 8
must also identity-map allocator-managed RAM as Normal, Inner Shareable,
read/write, and execute-never before test code dereferences allocated pages.
Large homogeneous ranges may use L1 or L2 block descriptors, while reservation
and attribute boundaries must remain exact.

The complete contiguous page-table store is allocated from pre-takeover
`Unused` RAM through the global allocator and remains `InUse`. Descriptor
stores are published before the table switch using the existing barrier
sequence.

The live DTB's page-rounded region is identity-mapped as Normal, read-only,
execute-never memory before switching tables. After the no-return boundary,
handoff pages transition from `InUse` to `Unused`, except for these retained
DTB pages. Tvisor then validates the DTB magic and total size through its own
mapping, so the original blob remains usable without copying.

## 6. Implementation structure

- Add a host-testable allocator module under `tvisor_util/` containing bitmap,
  region, allocation, free, statistics, and error logic.
- Extend `src/mm.rs` to initialize the allocator before table construction,
  allocate the table store from it, and map allocator-managed RAM.
- Copy only the delayed-reclamation metadata into a one-shot
  `ReclaimMemoryInfo` value in tvisor-owned `.bss`. Consume it after the
  private-EL2 transition to release U-Boot regions while retaining DTB pages;
  it is not part of `GlobalPageAllocator`.
- Pass only owned allocator state across the private-EL2 transition; do not
  retain references to the U-Boot stack.
- Run allocation/write/read/free validation after the Phase 7 post-switch
  checks.
- Print allocator totals, free-page counts, selected test pages, and explicit
  reclamation status.

## 7. Acceptance criteria

### Host tests

- allocation and free at every region boundary;
- deterministic allocation across unavailable holes;
- exhaustion and reuse after free;
- invalid, unaligned, reserved, and double-free rejection;
- bitmap-aperture overflow rejection;
- allocator ownership of the active table store; and
- statistics consistency after every operation.

### AArch64 build

- `cargo build --target aarch64-unknown-none` succeeds;
- no global heap is introduced;
- allocator bitmaps and control state are inside mapped tvisor-owned data; and
- every allocator region has a Normal RW/XN EL2 mapping.

### Raspberry Pi 4

- initialize the allocator before the page-table switch and allocate the table
  store from pre-takeover `Unused` RAM;
- allocate pages from eligible low and high RAM banks;
- write and read known patterns through their identity mappings;
- free and reallocate test pages deterministically;
- prove no returned page intersects permanent reservations or the table arena;
- demonstrate allocation from reclaimed U-Boot memory while retaining the
  original DTB reservation; and
- keep UART output and EL2 exception handling operational throughout.

Every hardware run begins from a fresh boot and ends with the board in a known
safe power state.
