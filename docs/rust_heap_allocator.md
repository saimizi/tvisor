# Rust heap allocator design

## 1. Scope

Tvisor provides one fixed Rust heap after the no-return takeover. The first
implementation enables the `alloc` crate while keeping the memory and locking
model deliberately small:

- `linked_list_allocator::Heap` supplies first-fit byte allocation and
  adjacent-hole coalescing;
- `TvisorHeap` owns initialization, synchronization, statistics, and the
  `GlobalAlloc` implementation;
- the global physical-page allocator supplies one contiguous 1 MiB arena;
- the active EL2 identity map makes the same range contiguous in VA; and
- the arena neither grows nor returns pages during this phase.

Guest RAM, page tables, DMA buffers, and other page-oriented resources remain
clients of the physical-page allocator. They must not be placed in the Rust
heap.

## 2. Memory path

```text
DTB-derived usable RAM
        |
        v
Global physical-page allocator
  allocate_contiguous(256 pages)
        |
        v
1 MiB PA range marked InUse
        |
        | EL2 identity map: VA = PA
        v
TvisorHeap
        |
        v
linked_list_allocator::Heap
        |
        v
Box / Vec / String / other alloc types
```

The heap allocator operates only on virtual pointers. Contiguous PA is needed
in this phase solely because tvisor uses identity mappings and does not yet
maintain a dedicated dynamic heap VA range.

## 3. Initialization and lifetime

Initialization occurs after all of the following:

1. tvisor has installed its private stack and vectors;
2. tvisor's EL2 stage-1 tables are active;
3. the no-return takeover boundary has been crossed;
4. U-Boot RAM has been reclaimed; and
5. the Phase 8 physical-page allocator validation has completed.

The physical allocator then reserves `INITIAL_HEAP_PAGES = 256` contiguous
pages. Those pages remain `InUse` permanently. `TvisorHeap::initialize()`
publishes the arena only after the underlying free-list metadata is ready.
Before that publication, allocation attempts return null.

The Rust toolchain's default no-`std` allocation-failure path reaches tvisor's
panic handler. Recoverable subsystems should still prefer fallible operations
such as `Vec::try_reserve()` and return an explicit out-of-memory error.

## 4. Synchronization policy

`TvisorHeap` serializes allocator metadata with `spin::Mutex`. This protects
future concurrent physical CPUs, but the mutex does not mask local interrupts.
Heap allocation is therefore forbidden in exception and interrupt handlers:
an interrupt that re-enters the allocator while interrupted code owns the heap
lock would deadlock.

Panic, UART, exception, physical-page allocator, and lock implementations must
remain allocation-free. SMP work must document lock ordering before the heap
is allowed to request more pages dynamically.

## 5. Fragmentation policy

The linked-list allocator uses first fit. It splits suitable free holes and
merges physically adjacent holes when allocations are released. It cannot
compact live objects and therefore cannot eliminate external fragmentation.

The initial policy limits that risk:

- use the heap for small and medium control metadata;
- keep page-sized and large buffers in the physical-page allocator;
- reserve container capacity when the expected size is known;
- group temporary allocations by lifetime where practical; and
- expose used, free, live-allocation, and failed-allocation counters.

Before adding dynamic heap growth, realistic VM and device workloads must be
measured. A multi-arena allocator or a dedicated heap VA range can replace the
backend without changing users because `TvisorHeap` is the public boundary.

## 6. Verification

Host tests verify:

- allocation before initialization fails safely;
- requested alignment is honored;
- allocation/deallocation restores accounting; and
- adjacent freed allocations coalesce into a larger usable hole.

The AArch64 hardware checkpoint initializes the heap after takeover, creates a
`Box<u64>` and a reserved-capacity `Vec<u64>`, verifies their addresses lie in
the heap arena, validates their contents, drops both objects, and confirms
that live allocations and used bytes return to zero.
