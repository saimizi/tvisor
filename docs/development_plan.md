# Tvisor platform discovery and execution-environment development plan

## 1. Purpose

Tvisor enters at EL2 through U-Boot, validates the inherited CPU and platform
state, and then performs an irreversible takeover. It installs its own stack,
exception vectors, EL2 stage-1 translation tables, and physical-page allocator.
The tables come from its fixed runtime footprint; after the switch, it
initializes the allocator directly from usable RAM while retaining the original
DTB pages used by the global FDT handle.

The completed discovery and execution-environment work follows this dependency
order:

```text
         live U-Boot DTB
                 |
                 v
        validated generic FDT parser
                 |
                 v
          platform discovery
                 |
                 v
      temporary SystemInfoBuilder
                 |
                 v
    normalized MemoryMap finalization
                 |
                 v
         final owned SystemInfo
                 |
                 v
     tvisor virtual-memory-map design
                 |
                 v
 private stack, vectors, allocator, and EL2 page tables
```

This document records the phased implementation plan through the physical-page
allocator, initial single-vCPU guest, and later SMP milestones.

## 2. Current codebase and constraints

The current implementation has these properties:

- `src/main.rs` provides an assembly `main` that clears `.bss` and enters the
  no-return Rust boot sequence.
- `rust_main` validates the DTB and EL2 handoff state, builds `SystemInfo`,
  prepares linker-owned translation tables, and enters private EL2.
- `tvisor_util/aarch64_reg.rs` contains typed AArch64 register accessors.
- `tvisor_util/diag.rs` collects the read-only handoff state.
- `tvisor_util/debug_util.rs` supplies the early mini-UART debug path.
- `src/boot.rs` installs tvisor's private stack, vectors, and EL2 translation
  regime, then initializes the physical allocator.
- `src/mm.rs` owns the global physical-page allocator, page-table preparation,
  retained-DTB mapping, and one-shot post-switch memory-map storage.
- `scripts/rpi.ld` links tvisor at physical address `0x0400_0000` and exports
  page-aligned image, section, guard, stack, and bootstrap-table symbols.
- `docs/address_translation.md` describes EL2 stage 1 and guest stage 2.
- `docs/check_handoff_state.md` defines the diagnostic phase and the observed
  U-Boot handoff register state.
- `docs/uboot_rpi4_memory.md` records the observed Raspberry Pi 4 RAM, LMB,
  U-Boot, DTB, and peripheral layout.
- `docs/peripheral_address_translation.md` explains how DT `ranges` converts
  bus addresses into ARM physical addresses.

The takeover path does not return. Before the switch, every write remains
inside tvisor's validated fixed runtime footprint. After the switch, U-Boot
runtime memory needs no explicit reservation or reclamation input; the live
DTB pages remain permanently excluded while borrowed.

No code may assume that the development board's captured `bdinfo` layout is a
stable ABI. Installed RAM, reservations, the DTB address, and U-Boot relocation
can change between boots or software versions.

## 3. Design principles

### 3.1 Separate representation from policy

Use three layers:

1. A generic FDT layer validates and reads the flattened device-tree format.
2. A platform-discovery layer interprets standard and supported
   Raspberry-Pi/BCM2711 bindings.
3. A memory-policy layer decides what tvisor can allocate or map.

The FDT parser must not contain tvisor allocation policy. Conversely, the
memory manager must not depend on raw FDT byte slices.

### 3.2 Use owned, physical-address-based system information

`SystemInfoBuilder` temporarily contains decoded discovery values rather than
pointers into the source DTB. It is consumed to create a final `SystemInfo`
containing the normalized `MemoryMap` and retained runtime platform data. All
region boundaries use CPU physical addresses and half-open intervals:

```text
[start, end)
```

This convention makes empty ranges, adjacency, overlap checks, subtraction,
and overflow validation unambiguous.

### 3.3 Remain allocation-free during early discovery

The parser and initial database must work before a heap exists. Use iterators,
borrowed parser views, and fixed-capacity owned arrays. Capacity exhaustion is
an explicit error; it must never silently discard a RAM or reservation entry.

### 3.4 Treat live inputs conservatively

Unknown or ambiguous reserved-memory entries remain reserved. Unknown DT
properties are skipped safely. MMIO is never allocatable RAM. Integer overflow,
out-of-bounds offsets, invalid cell widths, and untranslatable addresses are
reported as errors.

### 3.5 Separate host and guest descriptions

The U-Boot working DTB describes the host platform. It must not later be handed
unchanged to a guest. A guest receives a separately generated DTB describing
guest IPA memory and only its assigned or emulated devices.

## 4. Proposed module boundaries

The exact names may be refined during implementation, but the intended
responsibilities are:

```text
tvisor_util/
  fdt.rs              FDT format validation and read-only traversal
  system_info.rs      temporary builder, final SystemInfo, and region types
  platform.rs         generic DT-to-SystemInfoBuilder discovery
  bcm2711.rs          BCM2711-compatible details when bindings are insufficient
  memory_map.rs       normalization, reservation subtraction, and validation
  aarch64_reg.rs      architectural register accessors
  diag.rs             inherited-state diagnostics
  debug_util.rs       early UART diagnostics

src/
  main.rs             boot sequencing and current return/no-return boundary
  boot.rs             later handoff arguments and initialization sequencing
  mm.rs               later allocator and EL2 mapping policy
  exception.rs        later private EL2 vectors and exception handling
```

Reusable parsing, data representation, and pure map algorithms belong in the
`tvisor_util` library so they can be host-tested. Hardware transition code
belongs in the tvisor binary.

## 5. Phase 0: implement DTB handoff and generic FDT parsing

### Goal

Define exactly how tvisor receives the live working DTB from both U-Boot
`bootelf` and `go`, then validate and traverse that DTB with a generic,
read-only FDT parser.

Implementing the parser in this first phase lets the handoff be verified using
the actual DTB header and contents. It also gives the following platform-model
phase concrete parser output on which to base its owned data model.

### Registers, memory structures, and exception levels

- Entry at EL2 and the existing U-Boot stack.
- U-Boot's standalone-application `argc`/`argv` entry convention.
- The different argument lists constructed by `bootelf` and `go`.
- The DTB physical address supplied explicitly by the selected launch command.
- FDT header, structure block, strings block, and memory reservation block.
- FDT big-endian encoding and bounded byte-slice access.
- Structure tokens (`BEGIN_NODE`, `END_NODE`, `PROP`, `NOP`, and `END`).
- Strings-block offsets and property byte ranges.
- Inherited `#address-cells` and `#size-cells`.
- U-Boot's live LMB and relocated runtime ranges.
- No system-register changes; parsing occurs at EL2 under the inherited U-Boot
  execution environment.

### Files and modules

- Update `docs/development_plan.md` if the observed calling convention differs
  from this plan.
- Add a handoff-contract section to `docs/uboot_rpi4_memory.md` or a focused
  handoff document.
- Later changes will touch `src/main.rs`/`src/boot.rs`, but this phase should
  add only the handoff/inspection path without changing takeover behavior.
- Add small, redistributable DTB fixtures under a test-data directory. Include
  a minimal synthetic tree and a captured Raspberry Pi 4 tree, with its origin
  recorded.
- Before implementing the binary format from scratch, evaluate existing Rust
  crates for `no_std`, no-allocation operation, robust bounds checks,
  reservation-block access, inherited-cell handling, and licensing.
- Add `tvisor_util/fdt.rs`, or a small project-owned wrapper there around the
  selected crate, and export it from `tvisor_util/lib.rs`.
- Update `Cargo.toml` only if the evaluation selects a dependency.

Use a tagged argument rather than assigning meaning to a fixed `argv` index:

```text
fdt=<hexadecimal-physical-address>
```

For example, with an ELF staged at `${loadaddr}`:

```text
bootelf -d ${fdt_addr} ${loadaddr} fdt=${fdt_addr}
```

For a raw image or an already loaded entry point at `${tvisor_entry}`:

```text
go ${tvisor_entry} fdt=${fdt_addr}
```

U-Boot calls both as standalone applications with `x0 = argc` and `x1 = argv`,
but their arrays differ:

```text
bootelf ... <image_addr> fdt=<addr>:
    argv[0] = "fdt=<addr>"

go <entry_addr> fdt=<addr>:
    argv[0] = "<entry_addr>"
    argv[1] = "fdt=<addr>"
```

The handoff decoder must scan the bounded argument list for the unique `fdt=`
tag, parse its value as hexadecimal, and reject missing, duplicate, malformed,
or overflowing values. It must not assume that `argv[0]` is the DTB address.
This is a tvisor-specific convention layered over U-Boot's application ABI; it
is not the AArch64 Linux boot convention in which `x0` directly holds the DTB
pointer.

The `bootelf -d` option asks U-Boot to perform its configured FDT setup. `go`
has no equivalent option, so Phase 0 must also compare the trees used by the
two launch paths and document any required U-Boot `fdt` preparation commands.
Both paths must ultimately provide a validated working DTB suitable for the
same tvisor parser.

The generic parser must provide:

- header/version/offset/size validation;
- bounded node and property iteration;
- absolute-path lookup;
- NUL-terminated string and string-list access;
- big-endian 32-bit cell decoding;
- inherited address-cell and size-cell lookup;
- `reg` and `ranges` tuple decoding;
- aliases and phandle lookup;
- FDT memory-reservation iteration;
- safe skipping of unknown properties and nodes.

This layer must not interpret Linux, U-Boot, Raspberry Pi, or tvisor policy.
Its API should expose generic tree mechanics from a lifetime-bounded input
slice. Later platform discovery will copy selected decoded values into owned
structures.

### Acceptance criteria and verification

- **Host:** Tests decode representative `bootelf` and `go` argument arrays,
  including missing, duplicate, malformed, and overflowed `fdt=` arguments.
  Parser tests cover truncated blocks, invalid offsets, missing terminators,
  unknown tokens, zero/one/two-cell addresses and sizes, multiple tuples,
  empty `ranges`, nested buses, aliases, phandles, and reservation entries. A
  documented method can obtain the working DTB and inspect it with standard DT
  tools; fixture provenance and expected key nodes are recorded.
- **AArch64 build:** The handoff decoder and parser build as `no_std` without
  allocation or architecture-specific assumptions in the parser.
- **Raspberry Pi 4:** Launch tvisor once with `bootelf` and once with `go`.
  Both runs report the intended DTB address, validate its magic and
  `totalsize`, traverse it to print a bounded structural summary, and return
  safely to U-Boot. Invalid input produces a diagnostic error rather than an
  abort or out-of-bounds read. Record whether the relevant RAM and reservation
  information is equivalent after any required FDT preparation.

No DTB address should be inferred from a hard-coded `bdinfo` snapshot.

## 6. Phase 1: define the owned system-information model

### Goal

Create the owned data model into which all platform sources will be
normalized, using the Phase 0 parser API and observed Raspberry Pi 4 DTB as
inputs to the design without coupling the model to the parser's concrete
borrowed node/property types.

### Registers, memory structures, and exception levels

- CPU physical addresses and checked 64-bit region arithmetic.
- RAM, reserved-memory, MMIO, CPU, and console descriptions.
- Reservation origin, ownership, and attributes such as `no-map` and
  `reusable`.
- No new system registers; pure data handling works on host and at EL2.

The discovery model is temporary:

```rust
pub struct SystemInfoBuilder {
    pub ram: RegionList<RamRegion, MAX_RAM_REGIONS>,
    pub reserved: RegionList<ReservedRegion, MAX_RESERVED_REGIONS>,
    pub mmio: RegionList<MmioRegion, MAX_MMIO_REGIONS>,
    pub cpus: RegionList<CpuInfo, MAX_CPUS>,
    pub console: Option<ConsoleInfo>,
}
```

After discovery, the builder is consumed to create the final `SystemInfo`,
which retains the normalized `MemoryMap`, CPU information, bus translations,
and console information rather than duplicate raw memory records.

`ReservedRegion` should identify at least firmware, device, bootloader, DTB,
tvisor, Linux policy, and unknown origins. The design must distinguish raw RAM
from RAM that remains usable after reservations are subtracted.

### Files and modules

- Add `tvisor_util/system_info.rs`.
- Export the module from `tvisor_util/lib.rs`.
- Add host unit tests beside the types or in `tests/`.

### Acceptance criteria and verification

- **Host:** Unit tests cover range construction, checked end calculation,
  containment, overlap, adjacency, list capacity, and formatting.
- **AArch64 build:** `cargo build --target aarch64-unknown-none` succeeds
  without an allocator.
- **Raspberry Pi 4:** No behavioral change is required yet; the diagnostic
  program continues to run and return to U-Boot.

## 7. Phase 2: discover RAM and reservations

### Goal

Populate the RAM and reserved portions of `SystemInfoBuilder` from the live DTB
while keeping the original blob and U-Boot memory intact.

### Registers, memory structures, and exception levels

- `/memory` nodes, recognized primarily by `device_type = "memory"` rather
  than an exact node name.
- The root `#address-cells` and `#size-cells` used by memory `reg` tuples.
- The FDT memory reservation block.
- `/reserved-memory`, its child `reg` or dynamic `size`, `alignment`,
  `alloc-ranges`, `no-map`, `reusable`, and `compatible` properties.
- The live DTB's own page-rounded physical extent.
- Tvisor's image extent.
- Execution remains at EL2 under U-Boot's stage-1 translation regime.

Dynamic `/reserved-memory` entries that specify `size` without a resolved
`reg` require explicit policy. Initially, report and conservatively account for
them; do not invent a physical placement unless firmware/U-Boot has already
fixed one up in the working DTB.

### Files and modules

- Add `tvisor_util/platform.rs`.
- Extend `tvisor_util/system_info.rs` with reservation metadata as needed.
- Update `scripts/rpi.ld` to export page-aligned image start/end and useful
  section boundaries instead of duplicating the linked range in Rust.
- Update `src/main.rs` to invoke discovery only after the existing handoff
  validation succeeds.

### Acceptance criteria and verification

- **Host:** Fixture tests reproduce all RAM banks and reservation entries;
  multiple banks, reservations above 4 GiB, zero-size ranges, overflow, and
  capacity exhaustion are tested.
- **AArch64 build:** Linker symbols resolve and the target builds without
  warnings caused by the new boundary declarations.
- **Raspberry Pi 4:** UART output lists RAM and reservations and agrees with
  the live DTB/U-Boot inspection. It explicitly shows the tvisor image and DTB
  as reserved. Tvisor returns safely to U-Boot.

## 8. Phase 3: discover MMIO, CPUs, and the early console

### Goal

Complete the minimum platform database required to build correct EL2 mappings
and retain diagnostic output after changing translation tables.

### Registers, memory structures, and exception levels

- `/soc/ranges` and any nested bus `ranges` needed to translate a device's
  `reg` address to CPU physical space.
- Enabled nodes (`status` absent or `okay`) and their `compatible` strings.
- `/chosen/stdout-path`, optional UART options after `:`, and `/aliases`.
- `/cpus`, CPU `reg` affinity values, and enable/status information.
- `MPIDR_EL1` to correlate the executing core with DT CPU entries.
- Mini UART/GPIO MMIO required by `debug_util`.

The database should record only MMIO regions needed by tvisor initially,
along with larger known bus windows where useful for validation. It must not
assume that a DT unit address is already an ARM physical address.

### Files and modules

- Extend `tvisor_util/platform.rs`.
- Add `tvisor_util/bcm2711.rs` for narrowly scoped BCM2711 knowledge that is
  not expressed sufficiently by standard bindings.
- Refactor `tvisor_util/debug_util.rs` so UART MMIO remains disabled until the DTB-selected console has been translated to a
  CPU physical register base.
- Reuse `tvisor_util/aarch64_reg.rs` for `MPIDR_EL1`; do not specialize the
  generic register wrapper for platform discovery.

### Acceptance criteria and verification

- **Host:** Tests cover one and multiple `ranges` translations, nested buses,
  empty identity `ranges`, untranslated addresses, disabled devices, alias
  resolution, and `stdout-path` options.
- **AArch64 build:** The DTB-first console-discovery path compiles without allocation and performs no
  MMIO before debug initialization.
- **Raspberry Pi 4:** The discovered mini-UART CPU physical address agrees
  with the current working DTB and existing output (`0xFE21_5040` for the
  observed configuration). CPU affinity agrees with `MPIDR_EL1`. UART remains
  operational and tvisor returns safely to U-Boot.

## 9. Phase 4: merge, normalize, and validate the physical database

### Goal

Produce a deterministic usable-RAM map from all sources without yet allocating
from it.

### Registers, memory structures, and exception levels

- DT RAM and reservation regions.
- Tvisor image sections from linker symbols.
- Current stack extent, once its bounds can be established.
- Live DTB extent.
- Loaded guest images, initrds, future page-table pools, and other explicit
  tvisor-owned allocations.
- BCM2711 MMIO windows and discovered device MMIO.
- No register writes; EL2 remains in the inherited environment.

Normalization must:

- sort ranges;
- merge compatible overlapping or adjacent ranges;
- preserve conflicting ownership information;
- reject arithmetic overflow;
- detect contradictory RAM/MMIO classifications;
- subtract every active reservation from raw RAM;
- split RAM around reservations;
- produce a separate `usable_ram` list;
- fail explicitly if fixed capacities are insufficient.

U-Boot ownership is temporary and is not part of the normalized runtime map.
Before takeover, tvisor writes only its linker-defined runtime footprint; the
initial stack, vectors, and page tables are statically reserved there. U-Boot
`bdinfo` and LMB output remain deployment evidence used to validate that fixed
window, not runtime arguments or a handoff ABI.

Normalization produces one `usable_ram` allocation view. It excludes
permanent SoC, firmware, device, policy, live-DTB, and tvisor reservations and
is consumed only after the no-return transition.

Linux-policy reservations such as CMA/reusable pools remain reserved in the
first implementation. Reclaiming them requires a later documented ownership
policy. For a dynamic reservation that has `alloc-ranges`, the initial policy
conservatively excludes each complete allocation window because firmware has
not selected a final address. A dynamic reservation without an allocation
window makes the usable map unresolved and causes normalization to fail.

### Files and modules

- Add `tvisor_util/memory_map.rs`.
- Extend `tvisor_util/system_info.rs` with normalized/usable views.
- Add linker symbols to `scripts/rpi.ld` as required.
- Add a concise map dump to `src/main.rs` using `debug_util`.
- Update `docs/uboot_rpi4_memory.md` if hardware observation reveals new
  reservation classes.

### Acceptance criteria and verification

- **Host:** Table-driven tests cover subtraction with no overlap, full cover,
  prefix/suffix removal, middle split, multiple reservations, adjacency,
  overlapping reservations, addresses above 4 GiB, and list exhaustion.
  Property tests may be added if they remain deterministic and host-only.
- **AArch64 build:** The complete discovery/normalization pipeline builds with
  no heap.
- **Raspberry Pi 4:** UART prints sorted `RAM`, `RESERVED`, `MMIO`, and
  `USABLE` ranges. No usable range overlaps tvisor, the live DTB, U-Boot's
  still-live area, or MMIO. The values are checked against `bdinfo`, the DTB,
  and `docs/uboot_rpi4_memory.md`; tvisor still returns to U-Boot.

This phase is the gate before designing or enabling tvisor page tables.

## 10. Phase 5: define tvisor's virtual and physical memory map

### Goal

Turn the verified physical database into a documented EL2 virtual-address and
mapping policy. Do not switch translation tables until the design is reviewed.

### Registers, memory structures, and exception levels

- EL2 virtual addresses and CPU physical addresses.
- `ID_AA64MMFR0_EL1` capabilities: supported physical address range and
  translation granules.
- `MAIR_EL2` memory types.
- `TCR_EL2` address size, granule, walk cacheability, and shareability.
- `TTBR0_EL2` root-table address.
- `SCTLR_EL2.M`, `.C`, and `.I`.
- Translation-table descriptors, code/data permissions, execute-never bits,
  access flag, and shareability.
- Private stacks with guard pages, exception vectors, UART MMIO, page-table
  pool, allocator metadata, heap, and an optional physical direct map.

At minimum, the design must decide:

- whether the first private mapping is identity-only or includes a higher-half
  layout;
- the supported initial physical-address width;
- 4 KiB granule and block/page mapping policy;
- Normal cacheable RAM versus Device-nGnRE MMIO attributes;
- read/execute permissions for text and read/write/XN permissions for data,
  BSS, stacks, tables, and heap;
- how code continues executing across the `TTBR0_EL2` switch;
- which mappings are temporary and when they are removed.

### Files and modules

- Review and refine the draft in `docs/tvisor_memory_map.md`.
- Update `docs/address_translation.md` only for architectural clarification,
  not board-specific layout constants.
- Plan future `src/mm.rs`, `src/exception.rs`, and linker-script changes.

### Acceptance criteria and verification

- **Host:** A pure page-table-layout calculator can be unit-tested against
  expected VA-to-PA mappings, attributes, permissions, and table counts.
- **AArch64 build:** Compile-time constants and descriptor encodings pass
  static assertions and build for the target.
- **Raspberry Pi 4:** This is a design phase; the existing diagnostic remains
  the hardware baseline. Review confirms that every currently executed code,
  stack, data, UART, DTB, and transition address will remain mapped.

## 11. Phase 6: establish private EL2 foundations

### Goal

Install the minimum recovery and storage infrastructure required before
switching away from U-Boot's page tables.

### Registers, memory structures, and exception levels

- Tvisor-owned, 16-byte-aligned EL2 boot stack and later per-CPU stacks.
- Tvisor-owned, 2048-byte-aligned vector table and `VBAR_EL2`.
- `SPSel` and `SP_EL2`/current `SP` selection.
- `DAIF` during transition.
- `ESR_EL2`, `ELR_EL2`, `SPSR_EL2`, and `FAR_EL2` in exception reporting.
- Reserved page-table pool selected only from normalized usable RAM.

Switching to a private stack changes the current return-to-U-Boot contract.
Until an explicit no-return entry path exists, keep a separate diagnostic path
that preserves and restores U-Boot's stack and callee-saved state.

### Files and modules

- Follow the reviewed foundation design in
  `docs/private_el2_foundations.md`.
- Add `src/boot.rs` for the assembly/Rust boundary and entry modes.
- Add `src/exception.rs` and vector assembly.
- Add stack and vector sections/symbols to `scripts/rpi.ld`.
- Extend `tvisor_util/aarch64_reg.rs` only with reusable architectural register
  operations needed by the transition.
- Update `src/main.rs` to distinguish diagnostic-return and takeover paths.

### Acceptance criteria and verification

- **Host:** Exception-frame and stack-layout structures have size/alignment
  tests where possible.
- **AArch64 build:** Disassembly confirms stack alignment, vector alignment,
  preserved registers on the return path, and no stack use before a valid
  stack is selected.
- **Raspberry Pi 4:** First verify the old return path. Then run the explicit
  takeover path: UART works on the private stack, a deliberate test exception
  reaches tvisor's vector and prints syndrome information, and normal execution
  continues or halts predictably. Power recovery is available if the board
  becomes unresponsive.

## 12. Phase 7: build and switch to tvisor's EL2 stage-1 tables

### Goal

Replace the inherited U-Boot EL2 translation regime with tvisor-owned page
tables without losing instruction fetch, stack access, UART output, or
exception handling.

### Registers, memory structures, and exception levels

- Tvisor stage-1 translation tables rooted by `TTBR0_EL2`.
- `MAIR_EL2`, `TCR_EL2`, `TTBR0_EL2`, and `SCTLR_EL2`.
- Required `DSB`, `ISB`, and EL2 TLB invalidation operations.
- Cache state and maintenance required by the selected break-before-make or
  table-switch sequence.
- Identity transition mappings for the executing code, stack, vectors, UART,
  tables, and required data.
- EL2 only; guest stage 2 remains disabled (`HCR_EL2.VM == 0`).

The switch routine must be small, auditable assembly or tightly constrained
code. All addresses it touches before and immediately after the switch must be
valid under both the old and new regimes.

### Files and modules

- Review the transition design in `docs/el2_page_table_transition.md` before
  implementing any control-register writes.
- Add `src/mm.rs` and, if useful, reusable descriptor helpers under
  `tvisor_util`.
- Extend `src/boot.rs` with the synchronized table-switch routine.
- Extend `scripts/rpi.ld` for page-table alignment/reservation.
- Use `src/exception.rs` to diagnose translation faults.
- Update the final memory-map design with the implemented values.

### Acceptance criteria and verification

- **Host:** Descriptor encoding, table walking, boundary mappings,
  permissions, memory attributes, and unmapped guard regions are tested.
- **AArch64 build:** Inspect ELF sections and disassembly; verify all transition
  symbols are mapped and aligned. The build emits the intended `MAIR_EL2` and
  `TCR_EL2` values through typed encodings.
- **Raspberry Pi 4:** Print checkpoints immediately before and after the
  `TTBR0_EL2`/translation transition. Confirm UART and exceptions work under
  the new tables. Read back and print `SCTLR_EL2`, `TCR_EL2`, `TTBR0_EL2`, and
  `MAIR_EL2`. Exercise mapped RAM and UART, then deliberately test that a guard
  or unmapped address faults into tvisor's handler.

The takeover path does not return to U-Boot after this point.

## 13. Phase 8: add the post-takeover physical-page allocator

### Goal

Allocate pages only from the validated usable-RAM map after tvisor has
installed its private EL2 execution environment.

### Registers, memory structures, and exception levels

- Physical-page allocator metadata.
- Permanent reservations: tvisor image, stacks, vectors, tables, allocator
  metadata, firmware/no-map regions, and active devices.
- The original U-Boot working DTB, retained because the global FDT handle
  continues to borrow it.
- EL2 stage-1 mappings for allocator-managed RAM.

Initialize the allocator only after all of these are true:

1. the takeover path cannot return to U-Boot;
2. tvisor uses its own stack, vectors, UART setup, and translation tables;
3. all required handoff data has been copied into owned storage or explicitly
   retained;
4. the finalized memory map has been moved into tvisor-owned storage; and
5. page-rounded permanent reservation boundaries have been resolved.

The global FDT handle still borrows the original DTB. Its page-rounded region
is therefore permanently excluded and identity-mapped read-only/XN. A later design
may copy the blob into tvisor-owned storage and then release the original pages.

### Files and modules

- Extend `src/mm.rs` with the allocator.
- Reserve the initial table arena in `scripts/rpi.ld` so no allocator is
  required before takeover.
- Update `src/boot.rs` with post-switch allocator initialization.
- Add diagnostic allocator statistics and reservation dumps.

### Acceptance criteria and verification

- **Host:** Allocator tests cover every region boundary, allocation/free,
  exhaustion, double-free detection policy, reserved-page exclusion, and
  multiple discontiguous RAM banks.
- **AArch64 build:** The target builds without relying on a global heap unless
  and until the allocator intentionally provides one.
- **Raspberry Pi 4:** Allocate, write, verify, and free test pages from every
  eligible bank. Confirm no allocation intersects permanent reservations.
  Confirm former U-Boot RAM requires no runtime reclamation input while the
  original DTB remains retained and UART and exception handling remain
  operational.

## 14. Phase 9: prepare for guest execution

### Goal

Use the owned host database and allocator as the foundation for a separately
defined guest physical environment. Guest launch itself should be planned and
implemented as a subsequent project milestone.

Phase 9 runs only on the boot physical CPU and creates exactly one guest vCPU.
It must not start secondary physical CPUs. Guest and trap state should have
clear ownership boundaries so it can later become per-vCPU state, but the
initial implementation does not add synchronization for inactive CPUs.

### Registers, memory structures, and exception levels

- Guest IPA layout and assigned PA pages.
- Stage-2 translation tables.
- `VTCR_EL2`, `VTTBR_EL2`, `HCR_EL2`, and VMID management.
- Guest EL1 register state and EL2 `ERET` state.
- A generated guest DTB containing guest IPA addresses.
- EL2 exception handling for stage-2 faults and traps.

### Files and modules

- Add a guest-memory-map design document.
- Later add guest, stage-2, and guest-DTB-builder modules.
- Reuse `SystemInfo` for host resources but do not expose it directly to the
  guest.

### Acceptance criteria and verification

- **Host:** Generated guest DTB and stage-2 mappings agree: every guest memory
  and device range described in the DTB is mapped with intentional permissions,
  and no host-only region is exposed.
- **AArch64 build:** Guest-entry and stage-2 modules build with checked register
  encodings.
- **Raspberry Pi 4:** Before booting a real guest, a controlled EL1 payload can
  access assigned RAM/UART and produces a diagnosed stage-2 fault for an
  unassigned IPA.

## 15. Phase 10: boot a real single-vCPU guest

### Goal

Boot one real guest operating system while tvisor and the guest both remain on
the boot physical CPU. Establish image loading, guest boot data, interrupts,
timers, trap handling, and a guest console before introducing physical
concurrency. The first console is an emulated PL011 whose output is forwarded
through tvisor's physical Mini UART; Linux never owns the physical debug UART.

### Registers, memory structures, and exception levels

- Guest kernel, optional initrd, and generated-DTB placement in guest IPA
  space, backed by allocator-owned physical pages.
- One persistent vCPU context containing the guest-visible EL1 registers and
  the EL2 return state required by `ERET`.
- `HCR_EL2`, `CPTR_EL2`, `CNTHCTL_EL2`, `CNTVOFF_EL2`, `VTCR_EL2`, and
  `VTTBR_EL2` policies for a single guest.
- Physical and virtual interrupt-controller state required by the selected
  guest boot milestone.
- Guest timer state and EL2 handling for trapped or virtualized timer access.
- A guest-visible PL011 register page at a fixed IPA. It remains absent from
  the stage-2 map so accesses trap to EL2 for emulation.
- Stage-2 MMIO-abort state, including `ESR_EL2`, `FAR_EL2`, `HPFAR_EL2`,
  `ELR_EL2`, and the guest general-purpose register named by the syndrome.
- Virtual PL011 transmit, status, configuration, and peripheral-identification
  registers, followed later by interrupt/receive state. Tvisor retains
  exclusive control of the physical Mini UART.
- Stage-2 mappings for guest RAM and intentionally assigned or emulated
  devices; host-only RAM and MMIO remain inaccessible.

Bring up the console in two steps:

1. Implement TX-only, polled early-console support. Emulate `UARTDR` writes by
   forwarding the low byte to the Mini UART, report a ready transmitter through
   `UARTFR`, and accept or ignore configuration writes without exposing the
   physical UART. No guest UART interrupt or RX path is required for this
   checkpoint.
2. After early Linux output is stable, implement enough PL011 state for the
   normal AMBA probe and `ttyAMA0` console, including the PL011 peripheral and
   PrimeCell identification registers. Add receive buffering and virtual-GIC
   interrupt injection only when an interactive guest console is required.

VirtIO console and the general VirtIO device framework are deferred until
after the physical and guest SMP phases.

### Files and modules

- Add a guest-image loader and explicit guest boot-configuration structures.
- Add a single-vCPU context module and guest entry/resume loop.
- Add a generic trapped-MMIO dispatcher and a virtual-PL011 device module.
- Decode supported stage-2 MMIO accesses, update the referenced guest register
  for reads, and advance `ELR_EL2` only after successful emulation.
- Add the minimal interrupt, timer, and device policy required by the chosen
  guest.
- Extend the guest-DTB builder with `/chosen`, memory, CPU, interrupt, timer,
  fixed-clock, and emulated-PL011 descriptions that match the implemented
  virtual platform. Give the PL011 node its virtual IPA, compatible strings,
  clock references, and—when implemented—virtual interrupt. Set `stdout-path`
  and an appropriate `earlycon=pl011,mmio32,<ipa>` boot argument without
  copying the host UART node or its physical address.
- Document the supported guest image format and boot protocol.

### Acceptance criteria and verification

- **Host:** Image placement, DTB generation, IPA-to-PA mappings, and guest boot
  arguments agree at every boundary. Malformed or overlapping images fail
  without modifying allocator state. MMIO tests cover supported access widths,
  read/write direction, guest-register updates, transmitter status, ignored or
  retained configuration state, peripheral-identification values, unsupported
  offsets, and correct PC advance.
- **AArch64 build:** Guest entry/resume code and interrupt/timer register
  encodings build without relying on physical SMP support. The stage-2 abort
  path can distinguish the virtual PL011 IPA from invalid guest accesses and
  resume an emulated access without corrupting the vCPU context.
- **Raspberry Pi 4:** A single-vCPU guest reaches a defined boot checkpoint or
  userspace console. Linux early-console bytes appear through the physical Mini
  UART while tvisor retains its own diagnostic output. Deliberate stage-2 and
  trapped-register tests return to the EL2 handler with decoded diagnostics;
  an unsupported UART access fails visibly instead of being silently accepted.

## 16. Phase 11: add tvisor physical SMP

### Goal

Start and manage the Raspberry Pi 4's secondary physical CPUs after the
single-vCPU guest path is stable. Make tvisor's runtime state and EL2 execution
safe under physical concurrency without yet exposing additional vCPUs to the
guest.

### Registers, memory structures, and exception levels

- DT CPU topology, `MPIDR_EL1` affinity, and each CPU node's `enable-method`.
- The firmware interface selected by discovery, such as PSCI, or the supported
  platform-specific secondary-entry mechanism.
- Per-CPU EL2 stacks with guard pages, exception state, current-task/vCPU
  pointers, and a `TPIDR_EL2`-based per-CPU lookup policy.
- Shared EL2 stage-1 tables and synchronization for page-table changes.
- Locks or other serialization for the page allocator, UART, guest state, and
  any other mutable global object currently protected only by the single-core
  and DAIF-masked policy.
- Cross-core barriers, cache visibility, interrupt-controller SGIs, and EL2 TLB
  shootdowns.
- CPU online, parked, and failed states.

### Files and modules

- Add `docs/tvisor_smp.md` before enabling a secondary CPU.
- Add per-CPU storage and physical-CPU lifecycle modules.
- Add the minimal synchronization primitives required by audited shared state.
- Extend linker and memory-map support for one guarded EL2 stack per physical
  CPU.
- Update allocator, UART, exception, and translation-table code to remove
  their documented single-core assumptions.

### Acceptance criteria and verification

- **Host:** Logical CPU-ID assignment, per-CPU indexing, startup-state
  transitions, locks, and simulated TLB-shootdown coordination are tested.
- **AArch64 build:** Every secondary entry path selects a valid private stack
  before using Rust, establishes per-CPU state, and joins shared EL2 mappings
  with the required barriers.
- **Raspberry Pi 4:** All discovered cores enter tvisor, report unique MPIDR and
  per-CPU stack values, safely exercise shared allocation and UART paths, and
  can be parked without disturbing the single-vCPU guest.

## 17. Phase 12: add guest SMP

### Goal

Expose multiple virtual CPUs to a guest after tvisor itself is safe on multiple
physical CPUs. Keep vCPU identity and lifecycle independent of the physical CPU
on which each vCPU currently executes.

### Registers, memory structures, and exception levels

- One complete EL1 and EL2-return context per vCPU.
- vCPU lifecycle states and a scheduler or explicit vCPU-to-physical-CPU
  assignment policy.
- Virtual GIC state, virtual SGIs/IPIs, and per-vCPU virtual timers.
- A guest-visible PSCI-compatible CPU on/off interface.
- Shared stage-2 tables, VMID rules, and cross-core stage-2 TLB invalidation.
- Guest DTB CPU topology, enable method, interrupt-controller, and timer nodes.

### Files and modules

- Add vCPU lifecycle, scheduling, and guest-PSCI modules.
- Extend interrupt and timer virtualization with per-vCPU state.
- Extend the guest-DTB builder to describe the selected virtual topology.
- Add synchronization around shared VM and stage-2 state.

### Acceptance criteria and verification

- **Host:** vCPU state transitions, virtual CPU startup, IPI routing, timer
  ownership, scheduling, and stage-2 shootdown targeting are tested.
- **AArch64 build:** Context-switch and virtual-interrupt paths preserve every
  architecturally required guest register.
- **Raspberry Pi 4:** A controlled multi-vCPU payload exchanges virtual IPIs
  and observes independent timers before an SMP guest OS is allowed to boot.
  The final milestone is a guest that discovers and successfully uses the
  intended number of virtual CPUs.

## 18. Test and review policy

Each phase should be a small reviewable change and must pass, in order:

1. formatting and host unit tests;
2. `cargo build --target aarch64-unknown-none`;
3. ELF/linker/disassembly inspection when entry, stack, sections, or page
   tables change;
4. a Raspberry Pi 4 test through `rpictl-mcp`, with serial output captured;
5. a power-off or known-safe board state after hardware testing.

Parser and map-processing changes should use malformed-input tests because the
DTB is an external binary input. Transition changes should use short UART
checkpoints and one new risk at a time. Do not combine initial private-stack,
vector-table, allocator, and translation-table switching into one hardware
experiment.

## 19. Current implementation order

Phases 0 through 8 are complete. Continue in this order:

1. Design the Phase 9 guest IPA map, device policy, and guest-memory ownership
   model.
2. Implement and host-test stage-2 tables, register encodings, and a single
   vCPU context.
3. Run a controlled EL1 payload on the boot physical CPU and validate normal
   accesses, traps, resume, and stage-2 faults.
4. Define and boot the Phase 10 real single-vCPU guest.
5. Design and implement Phase 11 tvisor physical SMP; do not start a secondary
   CPU while any global mutable state still relies on single-core safety.
6. Add Phase 12 guest SMP only after physical SMP, interrupts, timers, and TLB
   shootdowns are stable.

Each step must preserve a working checkpoint. Do not combine first guest entry,
first secondary-CPU entry, and first multi-vCPU guest execution in one hardware
change.

## 20. Completion criteria for platform discovery

The discovery milestone is complete when a Raspberry Pi 4 boot produces a
stable, sorted report containing:

- every DT-reported RAM bank;
- the FDT reservation block;
- supported `/reserved-memory` entries and their attributes;
- the live DTB and tvisor image extents;
- the live DTB and tvisor runtime reservations;
- BCM2711 MMIO windows and the active UART's translated CPU physical range;
- detected CPUs and the current `MPIDR_EL1` affinity;
- usable RAM after conservative reservation subtraction;
- no overlapping `USABLE`/`RESERVED` or `USABLE`/`MMIO` ranges.

Only then should tvisor commit to its own EL2 virtual memory layout.
