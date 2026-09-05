# Draft tvisor EL2 memory-map design

## 1. Status and scope

This document is the Phase 5 draft for tvisor's own EL2 stage-1 address
space. It defines the mapping policy needed by later stack, exception, and
page-table work. Phase 5 does not create translation tables or write
`SCTLR_EL2`, `TCR_EL2`, `TTBR0_EL2`, or `MAIR_EL2`.

The first implementation prioritizes a small, observable takeover over a
final virtual-address layout. It uses identity mappings so that the same code
addresses remain valid before, during, and after replacement of U-Boot's EL2
translation regime. A higher-half layout can be added after takeover is
reliable.

This design concerns tvisor's EL2 stage 1 only. Guest IPA layout and stage-2
translation are separate designs.

## 2. Inputs and terminology

The map is derived from `MemoryMap`, not from fixed Raspberry Pi RAM sizes:

- `RAM` describes CPU-visible physical RAM;
- `RESERVED` is unavailable for allocation after takeover;
- `USABLE` can be allocated after the no-return takeover; and
- `MMIO` contains CPU physical peripheral windows.

Reservation and mapping policy are different. A reserved RAM page can still
need a mapping—for example, tvisor's image is permanently reserved from the
allocator but must be mapped for execution. Conversely, ordinary usable RAM
does not need to be mapped until tvisor intends to access it.

All intervals are half-open: `[start, end)`.

## 3. Initial decisions

| Property | Draft decision | Reason |
| --- | --- | --- |
| EL and regime | EL2 stage 1 | Tvisor executes at EL2. |
| VHE mode | Disabled (`HCR_EL2.E2H = 0`) | The first implementation uses the conventional EL2 register and descriptor regime. |
| Address space | TTBR0_EL2 only | One address space is sufficient initially. |
| VA size | 39 bits (`T0SZ = 25`) | Covers 512 GiB and uses a simple three-level 4 KiB walk. |
| Granule | 4 KiB (`TG0 = 0b00`) | Supported on Cortex-A72 and matches existing page rounding. |
| Initial VA-to-PA policy | Identity mapped | Preserves PC, stack, and pointers across the switch. |
| PA size | Derived from `ID_AA64MMFR0_EL1.PARange` | Do not copy U-Boot's `TCR_EL2.PS` blindly. |
| Table walks | Inner-shareable, WB/WA cacheable | Appropriate for Normal RAM containing page tables. |
| RAM attribute | Normal, WB/WA cacheable | Required for code, data, stacks, and tables. |
| MMIO attribute | Device-nGnRE | Prevents unsafe caching and speculation while allowing early write acknowledgement. |
| Writable mappings | Execute-never | Enforces W^X for data, stacks, tables, and MMIO. |
| Higher-half map | Deferred | Adds no value to the first takeover and complicates the transition. |
| Full physical direct map | Deferred | Initially map only memory tvisor actively uses. |

Before table construction, tvisor must verify that every identity-mapped PA
fits within the selected 39-bit VA space and the supported output PA size. A
failure is fatal; silently truncating an address is forbidden.

The observed Raspberry Pi 4 currently uses addresses below 4 GiB, so it fits
comfortably. The checks remain necessary because the data model stores 64-bit
physical addresses.

The takeover path must reject an unexpected `HCR_EL2.E2H = 1`; VHE changes
EL2 stage-1 control and permission semantics and requires a separate design.

## 4. Draft virtual layout

The initial VA equals the PA for every valid mapping:

```text
EL2 virtual address                         CPU physical address

0x00000000_04000000  tvisor image     ---> 0x00000000_04000000
0x00000000_040c2000  bootstrap arena  ---> 0x00000000_040c2000
0x00000000_FE215000  mini UART page   ---> 0x00000000_FE215000
```

Exact image and bootstrap ends are derived rather than hard-coded:

- the image uses linker section symbols;
- the bootstrap arena uses linker symbols within the tvisor runtime image;
- the UART page is derived from the DTB-selected console register range.

The address above is from the current debug build and is not an ABI. Code uses
`__bootstrap_tables_start` and `__bootstrap_tables_end`; source changes may
move both symbols.

### 4.1 Mandatory takeover mappings

The first table must contain only the mappings needed to survive takeover:

| Object | Lifetime | Attribute and permissions |
| --- | --- | --- |
| Transition assembly | Permanent image | Normal, read-only, executable at EL2 |
| `.text` | Permanent image | Normal, read-only, executable at EL2 |
| `.rodata` | Permanent image | Normal, read-only, execute-never |
| `.data`, `.bss`, `.got` | Permanent image | Normal, read/write, execute-never |
| Tvisor vector table | Permanent image | Normal, read-only, executable at EL2 |
| Bootstrap page tables | Tvisor-owned | Normal, read/write, execute-never |
| Private EL2 boot stack | Tvisor-owned | Normal, read/write, execute-never |
| Stack guard page | Tvisor-owned VA gap | Invalid descriptor |
| Allocator metadata, if needed | Tvisor-owned | Normal, read/write, execute-never |
| Mini UART register page | Platform MMIO | Device-nGnRE, read/write, execute-never |
| Live DTB | Temporary | Normal, read-only, execute-never |

The live DTB mapping is omitted if discovery has already copied every required
value before the switch. If retained, it is removed after takeover and after
all borrowed references have disappeared.

Tvisor does not map U-Boot's code, heap, old stack, or translation tables into
its steady-state address space.

### 4.2 Later mappings

After takeover, the physical allocator may take pages from `USABLE`. A page is
mapped into EL2 only when tvisor needs to access it. Later phases may add:

- per-CPU stacks and guard pages;
- heap pages;
- guest-memory access windows;
- stage-2 table pages;
- selected device pages; and
- an optional direct map of allocatable physical RAM.

These additions must not weaken the initial W^X or MMIO attribute policy.

## 5. Bootstrap arena

The first private page tables and stack must exist before the general physical
allocator. The linker reserves a dedicated 64 KiB, page-aligned `NOLOAD`
bootstrap-table arena inside `[__image_start, __image_end)`. Table preparation
is split into explicit construction and policy steps:
`mm::setup_bootstrap_page_table` zeros the arena and maps linker-owned boot
objects, while `rust_main` adds usable RAM, the retained DTB, and UART MMIO
before validating the completed table set.

The arena contains the root and subordinate EL2 translation tables. The boot
stack and guard remain separate linker sections. The complete tvisor runtime
is inserted into the permanent reservation set during DTB discovery, so the
post-switch allocator cannot return any arena page.

The arena contains exactly sixteen pages. Linker assertions and runtime checks
enforce its size and alignment, while `TableSet` reports exhaustion if the
mandatory mappings exceed that capacity.

## 6. Translation-table structure

With a 4 KiB granule and 39-bit VA space, translation begins at level 1:

```text
L1 entry: 1 GiB range
L2 entry: 2 MiB range
L3 entry: 4 KiB range
```

Mapping policy:

1. Use the largest descriptor that preserves exact attributes, permissions,
   and boundaries.
2. Never widen a mapping across an undiscovered hole or adjacent MMIO.
3. Split around image section boundaries, guard pages, and device pages.
4. Use L3 pages for the UART and other sub-2-MiB MMIO ranges.
5. Permit L1/L2 blocks for large homogeneous Normal-memory mappings added in
   later phases.
6. Reject descriptor output-address overflow or misalignment.

The initial minimal map will mostly use L3 mappings because the image sections,
stack guard, tables, and UART require fine-grained permissions. This is an
acceptable bootstrap cost.

## 7. MAIR_EL2 policy

The draft defines only two attribute indices:

| AttrIdx | MAIR byte | Meaning | Use |
| --- | --- | --- | --- |
| 0 | `0xFF` | Normal WB/WA cacheable | Image, stack, page tables, RAM |
| 1 | `0x04` | Device-nGnRE | UART and other mapped MMIO |

Unused MAIR entries are zero. Descriptors must use an explicit named index;
raw numeric indices should not be scattered through the mapper.

Normal-memory descriptors are Inner Shareable. Device mappings use the
architecturally appropriate shareability selected by the descriptor helper.
All leaf descriptors set the Access Flag initially so the first access does
not generate an access-flag fault.

## 8. Permission policy

The mapper applies least privilege:

```text
text/vector:  EL2 read-only, executable
rodata:       EL2 read-only, XN
data/BSS/GOT: EL2 read/write, XN
stack/heap:   EL2 read/write, XN
page tables:  EL2 read/write, XN
MMIO:         EL2 read/write, XN
```

With `HCR_EL2.E2H = 0`, this is an EL2-only translation regime; lower
exception levels do not use these tables. Guest access will later be governed
by its EL1 stage 1 and tvisor's stage 2 rather than by exposing this EL2 map.

`SCTLR_EL2.WXN` should be enabled after the transition so any writable mapping
is execute-never even if a descriptor is accidentally too permissive. Linker
symbols must expose page-aligned section boundaries; otherwise permission
separation cannot be enforced without mapping unrelated bytes together.

## 9. Register configuration

The future table switch will construct register values from named fields:

- `TCR_EL2.T0SZ = 25` for 39 VA bits;
- `TCR_EL2.TG0 = 0b00` for 4 KiB;
- `TCR_EL2.SH0 = 0b11` for Inner Shareable walks;
- `TCR_EL2.IRGN0 = 0b01` and `ORGN0 = 0b01` for WB/WA walks;
- `TCR_EL2.PS` encoded from the validated `PARange` capability;
- `TTBR0_EL2.BADDR` set to the bootstrap root-table PA;
- `MAIR_EL2` containing Attr0 `0xFF` and Attr1 `0x04`; and
- final `SCTLR_EL2.M/C/I/WXN` enabled with the reviewed alignment policy.

Reserved and implementation-defined register bits must be written using
architecturally defined reset/RES1 policy, not copied wholesale from U-Boot.
The implementation must validate 4 KiB granule support before building tables.

## 10. Transition invariants

Phase 7 will define the exact assembly sequence. This memory map guarantees the
conditions that sequence needs:

1. The transition routine has the same VA and PA.
2. The private stack has the same VA and PA and is selected before U-Boot's
   translation is disabled.
3. The root tables are physically addressable and their writes have reached
   the required visibility point.
4. The tvisor vector table is identity-mapped before `VBAR_EL2` selects it.
5. UART remains mapped with Device attributes after the new MMU is enabled.
6. No instruction, literal, global, stack slot, or required MMIO access during
   the switch targets an absent mapping.
7. No code returns to U-Boot after its mappings and stack are abandoned.

Changing `TCR_EL2` and `MAIR_EL2` while U-Boot's translation regime is active
is not part of this design. The later transition routine will enter an
identity-addressed window, perform the required cache maintenance, barriers,
MMU disable, TLB invalidation, register installation, and MMU enable entirely
in assembly.

## 11. Post-takeover ownership policy

After the no-return boundary:

- U-Boot runtime, old stack, old translation tables, and staging buffers need
  no runtime reservation or explicit reclamation pass;
- the live DTB may be released only after all required information is owned;
- the bootstrap arena remains tvisor-owned;
- firmware carve-outs, permanent reservations, and MMIO never enter the RAM
  allocator; and
- allocating a physical page does not automatically create an EL2 mapping.

The current dynamic CMA allocation-window policy remains conservative. Phase
5 does not decide whether Linux-oriented CMA policy should be discarded for a
standalone hypervisor.

## 12. Planned implementation boundaries

Later phases should add or change:

- `scripts/rpi.ld`: page-aligned symbols for text, rodata, writable data,
  vectors, and bootstrap objects;
- `src/boot.rs`: diagnostic-return and no-return entry paths;
- `src/exception.rs`: private EL2 vectors and exception reporting;
- `src/mm.rs`: layout calculation, descriptor creation, and table building;
- `tvisor_util/aarch64_reg.rs`: reusable typed writes and invalidation helpers;
  and
- `src/main.rs`: orchestration only, without embedding descriptor arithmetic.

Pure layout and descriptor logic should remain host-testable and independent
of MMIO or system-register writes.

## 13. Acceptance criteria for the design

Before Phase 6 begins, review must confirm:

- the selected VA and PA widths cover every mandatory mapping;
- every instruction and data access needed during takeover is identity-mapped;
- image sections can receive distinct permissions at page boundaries;
- stack guard pages remain invalid;
- Normal RAM and MMIO use different MAIR attributes;
- page-table memory is sourced only from the linker-owned tvisor runtime;
- table count and bootstrap-arena size can be calculated without allocation;
- the diagnostic return path remains separate from the no-return path; and
- no U-Boot address becomes a permanent board constant.

Phase 5 is complete only after this draft is reviewed and its open questions
are resolved. No Raspberry Pi register writes are required for this phase.

## 14. Open questions

1. What private boot-stack size and exception-stack policy should Phase 6 use?
2. Should the bootstrap arena have a maximum size independent of its calculated
   minimum?
3. Should Device-nGnRnE replace Device-nGnRE for the first UART mapping?
4. Should tvisor keep or discard the DTB's Linux-oriented dynamic CMA request?
5. When should a higher-half layout or physical direct map be introduced?
6. Which `SCTLR_EL2` alignment checks should be enabled at the first switch?
