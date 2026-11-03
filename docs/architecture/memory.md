# Memory ownership and translation

Host physical addresses (PA), host EL2 virtual addresses (VA) and guest
intermediate physical addresses (IPA) name different spaces. Host mappings are
currently identity mappings, but a guest IPA does not identify its backing PA.
Linux separately owns its EL1 VA-to-IPA translation. Device Tree bus addresses
must first be translated to CPU physical addresses; see [platform discovery](platform.md).

## Physical memory classification

[MemoryMap](../../tvisor_util/memory_map.rs) is built in static storage, with
sorted/merged RAM, permanent reservations and MMIO ranges. `finalize()` rejects
RAM/MMIO conflicts and derives usable RAM by subtracting reservations. A
successful boot-only publication exposes an immutable map to readers. Failed
construction exposes no partial map. Coherent entry precedes worker access.

Permanent reservations include the complete tvisor runtime image, live host
DTB pages, staged Linux Image pages, FDT reservation entries, supported
`/reserved-memory` nodes, the BCM2711 firmware carve-out and secondary holding
memory. `no-map` and `reusable` reservations are both kept unavailable; conflicting
attributes and unsupported `no-map-fixup` are rejected. A dynamic reservation
with `alloc-ranges` conservatively reserves its possible allocation windows.
Unbounded placement is rejected.

The full firmware release-cell page must be described RAM, wholly reserved,
outside usable RAM and outside MMIO before mapping it writable. Holding code
and other live firmware data need reservations too; reserving only the cells
is insufficient. An aligned physical cell at zero is supported through assembly
access. Partial page ownership is rejected.

Before takeover there is no general physical allocation. The initial tables,
boot stack and static metadata are inside the deployment-validated linker
footprint. After the no-return EL2 switch, U-Boot's former stack, tables and
runtime allocations may be reused if they are otherwise usable RAM. No LMB or
bootloader-allocation list is passed to tvisor; [deployment checks](../setup/raspberry-pi4.md)
must protect the original load and transition.

## Physical allocator and heap

[PageAllocator](../../tvisor_util/page_allocator.rs) covers a 4 GiB aperture
with two bitmaps: managed pages and in-use pages. Unmanaged addresses include
reserved RAM, MMIO and holes; they cannot be allocated or freed. Managed pages
are InUse or Unused. Bitmap set counts provide consistent allocator statistics.
RAM above the aperture is not supported by this allocator.

[mm.rs](../../src/mm.rs) serializes initialization, bitmap views, allocation,
freeing and statistics through one IRQ-save lock. Returned pages have exclusive
ownership; freeing validates alignment, ownership and double-free conditions.
Initial secondary stack allocation rolls back only before mapping/publication.

The [Rust heap](../../tvisor_util/heap_allocator.rs) wraps `linked_list_allocator`
with a spin mutex. [Heap setup](../../src/heap.rs) obtains 256 contiguous physical
pages (1 MiB) after usable RAM is identity-mapped. That arena remains allocated
for tvisor's lifetime. The heap does not discover physical RAM or grow itself;
ordinary allocation/deallocation and coalescing occur within the arena.
Statistics track arena size, usage, live allocations and failed requests.

Heap locking does not mask IRQs. Allocation, deallocation and heap statistics
are forbidden in exception/IRQ handlers. Typed exit errors avoid heap allocation
in failure handling. VM allocations and stage-2 table pages are separately
tracked by [VM management](virtualization.md#vm-and-vcpu-ownership); mapping a
host-owned device does not transfer it to the VM allocator.

## Host EL2 mappings

[The linker](../../scripts/rpi.ld) reserves sixteen 4 KiB pages (64 KiB) for
runtime EL2 stage-1 tables. The NOLOAD arena is explicitly zeroed before use and
never returned to the allocator. Table exhaustion is a boot error.

The host uses a 39-bit VA space, 4 KiB granules and an L1 root. The
[table builder](../../tvisor_util/el2_stage1_translation.rs) chooses aligned
1 GiB L1 blocks, 2 MiB L2 blocks or 4 KiB L3 pages without crossing permission
boundaries. It rejects truncation, overflow, unsupported PA widths and conflicts.
Normal memory is Inner Shareable; AF is set. EL2 XN is bit 54, not EL1 PXN.

| Region | Attributes |
|---|---|
| Text and exception vectors | Normal, read-only, executable |
| Read-only data, live DTB, staged Image source | Normal, read-only, non-executable |
| Writable image, table arena, usable stacks and ordinary usable RAM | Normal, read/write, non-executable |
| UART and GIC interfaces | Device-nGnRE, read/write, non-executable |
| Stack guards and unmapped holes | Invalid descriptors |
| Validated firmware release page | Normal, read/write, non-executable |

MAIR has Normal WB/WA at index 0 and Device-nGnRE at index 1 (`0x4ff`).
TCR uses T0SZ=25, Inner Shareable WB/WA walks and a validated PARange encoding;
TTBR0 names the owned aligned root with CnP clear. `HostTranslation` supports
PA encodings through 48 bits, independently of the allocator's smaller aperture.
SCTLR is the explicit host policy (`0x30cd183d`), enabling M/C/I, stack alignment
checking and WXN with little-endian data accesses.

Initial setup rejects inherited HCR VM/E2H enablement and unsupported 4 KiB
granules. Every mapped PA must fit the implemented width and the host's 39-bit
identity VA range. An architectural PA encoding is not evidence that the CPU
implements that width; policy construction uses validated ID-register metadata.

CPU0's translation switch stays in identity-mapped executable code with DAIF
masked, private SP/TPIDR/VBAR installed and all transition arguments in registers.
It completes publication, temporarily clears M while preserving C/I, installs
MAIR/TCR/root, invalidates local EL2 translations with barriers, installs final
SCTLR and branches to the private continuation. There is no stack access or
literal-pool load in the critical interval and no return to the inherited regime.
Secondary entry has its own [firmware preconditions](cpu.md#stackless-secondary-entry).

Before releasing CPUs, resource validation checks actual table walks, complete
object extents, identity permissions, root ownership, section separation and
all disjoint guarded stacks. `BootTables::validate_and_freeze` validates and seals
ordinary table editing under the same lock. `edit()` then rejects writers; read
views remain available. Failed validation does not freeze, and there is no thaw.

## Synchronization and failure records

[`IrqSpinLock`](../../tvisor_util/sync.rs) masks local IRQ before acquisition,
releases the lock before restoring the saved I bit, and preserves unrelated
D/A/F bits. Failed try-locks restore IRQ state. Guards cannot move between CPUs.
Do not nest resource locks, allocate/format inside allocator or table critical
sections, or wait for another CPU while holding them.

Console output uses one IRQ-save lock per formatted message; guest byte output
uses the same lock. Polling is bounded and CPU0 alone consumes UART RX. Emergency
output tries the console lock once and emits a bounded message. If the faulting
CPU already holds it, the failure record survives without waiting for the lock.

[`FatalState`](../../tvisor_util/failure.rs) preserves a first-failure record
for each logical CPU plus a pre-context slot, and a shared fatal flag. A reader
acquires only complete snapshots and never waits for an interrupted writer.
Inherited TPIDR is compared against known slot addresses before any dereference.
Evidence includes failure kind, syndrome, PC and fault address; panic text is
best effort. A fatal condition prevents guest entry and requests worker stop.

## Synchronized EL2 mapping updates

Ordinary images keep EL2 tables frozen. Only opt-in TLB acceptance builds reserve
two backing pages and a complete table path for one RW/NX Normal Inner Shareable
scratch page at VA `0x1_0000_0000`. The narrow replacement API checks the exact
old descriptor and cannot allocate tables, split blocks, thaw ordinary editing,
or change code, stacks, devices or guest mappings. Both backing pages and tables
remain allocated until reset.

[`UpdateState`](../../tvisor_util/tlb_update.rs) serializes updates and lifecycle
membership changes. The retaining set includes CPU0 and all enabled Online or
Parked workers; missing/stopped members cannot be silently excluded. Updates
run with the guest deactivated under the host HCR profile.

1. CPU0 acquires the gate, validates retaining membership and initializes the
   alternate reserved backing with an epoch signature.
2. Sequence-numbered `TlbQuiesce` requests admit that epoch in bounded IRQ work.
   An SGI receipt is not proof of quiescence or completed architectural TLBI.
3. Each owner reads the old alias, locally synchronizes, publishes its quiescent
   epoch and stops accessing the alias. CPU0 also reaches this safe point.
4. After all owners are quiet, CPU0 locks the tables and performs
   break-before-make: invalid descriptor, maintenance, replacement descriptor,
   maintenance. [Each maintenance call](../../src/tlb_sync.S) executes:

   ```asm
   dc   cvac, x0
   dsb  ish
   tlbi alle2is
   dsb  ish
   isb
   ```

5. After releasing the table lock, CPU0 publishes the synchronization stage.
   Each owner locally synchronizes, reads the new signature and acknowledges
   the exact epoch. It makes no further scratch access after that acknowledgement.
6. Only complete synchronization permits repurposing the old backing. Tests
   poison that page to expose stale translations; they do not free it.

Waits have counter deadlines and bounded poll counts. Once an epoch starts,
missing acknowledgements, wrong signatures, descriptor failures and timeouts
are terminal: fail the protocol, record fatal state, notify peers and halt.
No rollback, retry, guest continuation or reclamation follows partial completion.
A late acknowledgement cannot make a failed epoch reusable. The second broadcast
is conservative and applies the same maintenance after each descriptor store.

These are host EL2 stage-1 updates, not guest stage-2 shootdown support. See
[hardware acceptance](../testing/hardware-tests.md) for Online/Parked remaps
and late-acknowledgement tests.

## Architectural reference

[Arm's AArch64 memory-management guide](https://documentation-service.arm.com/static/68e680a2587fd957f5efd742),
sections 8.1–8.3, describes descriptor publication, break-before-make and TLB
maintenance. Concrete ownership, retaining membership and fail-stop policy above
are tvisor's implementation contracts.
