# Removing U-Boot runtime-memory information from the takeover contract

## 1. Status and scope

This document proposes a future simplification of tvisor's U-Boot handoff and
EL2 takeover design. It is a design note only. The change must not be
implemented while Phase 9 guest bring-up is in progress.

The proposed work is intentionally deferred until the current Phase 9 behavior
has been completed and recorded. It changes the boot contract, early page-table
storage, memory-map normalization, and allocator initialization, so combining
it with guest-entry work would make failures unnecessarily difficult to
isolate.

The proposal removes these runtime arguments:

```text
lmb=<start>:<size>
bootmem=<start>:<size>
```

It does not remove U-Boot as the bootloader. U-Boot remains responsible for
loading tvisor, supplying the live host DTB address, entering tvisor at EL2,
and transferring execution to tvisor's entry point.

## 2. Problem statement

The current implementation constructs two physical-memory views:

```text
INITIAL_USABLE = RAM - permanent reservations - U-Boot handoff reservations
USABLE         = RAM - permanent reservations
```

The repeatable `lmb=` and `bootmem=` arguments provide the temporary U-Boot
handoff reservations. Before replacing U-Boot's EL2 translation regime,
`mm::prepare` initializes the page allocator from `INITIAL_USABLE` and
dynamically allocates 16 contiguous 4 KiB pages for the new EL2 stage-1
translation tables.

This makes the safety of the table arena depend on a manually supplied and
complete description of U-Boot's current runtime allocations. The arrangement
is conservative, but it has several weaknesses:

- U-Boot LMB is an internal implementation mechanism, not a stable tvisor
  handoff ABI.
- The operator must copy every relevant range from the same boot session.
- A missing range can make pre-switch allocation unsafe.
- The arguments reach tvisor only after its image has already been loaded, so
  they cannot establish that the original load destination was safe.
- The transition-reservation and post-switch reclamation machinery exists
  primarily because tvisor allocates table memory outside its own image before
  takeover is complete.

The fundamental pre-takeover requirement is narrower: tvisor needs a safe
place for the code, data, stack, vectors, and translation tables required to
cross the no-return boundary.

## 3. Existing load-address assumption

Tvisor is linked at physical address `0x0400_0000`. The current boot procedure
already assumes that the tvisor runtime footprint beginning at that address is
ordinary RAM and does not overlap live U-Boot or firmware state.

The fixed address was selected and tested using the Raspberry Pi 4 DTB,
`bdinfo`, U-Boot load configuration, and captured hardware behavior. It is a
board boot contract, not a universally portable address.

Embedding the bootstrap tables does not introduce a new kind of placement
assumption. It expands and makes explicit the footprint covered by the
existing assumption:

```text
TVISOR_RUNTIME = [__image_start, __image_end)
```

The complete interval, rather than only the downloaded file bytes, must be
safe before U-Boot transfers control.

## 4. Proposed design

### 4.1 Linker-owned bootstrap table arena

Reserve 64 KiB for the initial EL2 stage-1 translation tables in a dedicated,
page-aligned linker section. The section is BSS-like but remains distinct from
ordinary `.bss` so its purpose, bounds, mapping, and size are visible.

An illustrative linker layout is:

```ld
.bootstrap_tables (NOLOAD) : ALIGN(0x1000) {
    __bootstrap_tables_start = .;
    . += 0x10000;
    __bootstrap_tables_end = .;
}

. = ALIGN(0x1000);
__image_end = .;
```

The exact section order is an implementation decision, but the section must:

- lie within `[__image_start, __image_end)`;
- start and end on 4 KiB boundaries;
- contain exactly 16 pages unless the table-capacity calculation is changed;
- be writable while the tables are constructed;
- be mapped as normal memory after the new regime becomes active;
- never be returned by the physical-page allocator while it remains in use.

Because a `NOLOAD` section has no bytes in the downloaded file, takeover code
must explicitly zero the arena before constructing descriptors. Correctness
must not depend on its power-on or U-Boot-provided contents.

### 4.2 No pre-switch physical allocation

`mm::prepare` must construct the initial table set directly in the linker-owned
arena. It must not initialize or call the general physical-page allocator to
find table storage before replacing `TTBR0_EL2`.

The pre-switch storage set becomes entirely static:

```text
tvisor runtime footprint
├── executable code
├── read-only and writable data
├── exception vectors
├── boot-stack guard
├── private boot stack
└── 64 KiB bootstrap table arena
```

Consequently, the no-return transition needs no unidentified free RAM. Every
byte that it writes before the switch belongs to tvisor's linker-defined
runtime footprint, apart from intentional MMIO writes to the discovered UART.

### 4.3 Post-switch allocator initialization

The general physical-page allocator should be initialized after tvisor has
activated its private stack, vectors, and EL2 stage-1 translation regime.

Its managed memory should be derived from stable platform and ownership data:

```text
ALLOCATABLE_RAM = DTB RAM - permanent reservations
```

Permanent reservations include at least:

- the complete tvisor runtime footprint;
- the live host DTB while tvisor retains references to it;
- the FDT memory reservation block;
- supported `/reserved-memory` regions and conservatively handled dynamic
  reservations;
- firmware carve-outs inferred by the supported BCM2711 platform policy;
- MMIO and unpopulated physical-address ranges.

U-Boot runtime allocations are not permanent reservations. After the
no-return switch, tvisor neither executes U-Boot code nor uses U-Boot's stack,
page tables, allocator, or relocated runtime data. Those pages may therefore
enter the allocator without an explicit reclamation pass.

If the original DTB remains live, its page-rounded physical region must stay
in use until tvisor creates an owned copy or otherwise removes all references
to it.

### 4.4 Simplified handoff contract

The intended U-Boot invocation becomes conceptually:

```text
go <tvisor-entry> fdt=${fdt_addr}
```

Optional diagnostic arguments such as `fault=` may remain. The `lmb=` and
`bootmem=` arguments are removed.

The bootloader-side contract is:

1. Load the file-backed tvisor image at its linked address.
2. Ensure the complete runtime interval `[__image_start, __image_end)` is
   backed by RAM and does not overlap live bootloader or firmware state.
3. Supply a valid, readable DTB through `fdt=`.
4. Enter at EL2 using the documented entry convention.
5. Treat the transfer as irreversible on the successful path.

Item 2 is a deployment requirement. `filesize` is insufficient when the
runtime footprint contains `NOLOAD` sections. Build output or a documented
symbol-derived size must expose the required footprint to the boot procedure.

## 5. Why this removes the LMB dependency

Today, LMB information protects a dynamically selected table arena while
U-Boot's translation tables may still be active:

```text
LMB arguments
    -> transition reservations
    -> INITIAL_USABLE
    -> physical allocator
    -> bootstrap table arena
```

With linker-owned tables, that chain disappears:

```text
linker symbols
    -> bootstrap table arena inside tvisor
    -> TTBR0_EL2 switch
    -> post-switch physical allocator
```

Tvisor no longer needs to identify all U-Boot-owned memory. It needs only the
single explicit guarantee already inherent in loading a fixed-address
hypervisor: the complete tvisor runtime footprint is safe.

This is narrower than MiniVisorPi's convention of allocating early tables from
an assumed-low free range. Tvisor's bootstrap writes remain confined to one
statically reserved interval.

## 6. Runtime-footprint validation

The design must not confuse file size with runtime size:

```text
file footprint    = downloaded bytes
runtime footprint = __image_end - __image_start
```

The runtime footprint includes `.bss`, stack storage, guards, and bootstrap
tables even if they occupy no bytes in the ELF or raw binary payload.

Before adopting the design, the build and boot documentation should record:

- `__image_start`;
- `__image_end`;
- the total runtime size;
- bootstrap-table start, end, alignment, and page count;
- the maximum permitted runtime footprint inside the validated Pi 4 load
  window.

A build-time assertion should fail if the arena has the wrong alignment or
size. A deployment-time check should fail if the complete runtime footprint no
longer fits the region validated for the board and U-Boot configuration.

## 7. Expected code changes after Phase 9

The later implementation is expected to affect these areas.

### `scripts/rpi.ld`

- Add the dedicated bootstrap-table section and symbols.
- Include its end in `__image_end`.
- Add linker assertions for alignment and exact size.

### `src/mm.rs`

- Replace dynamic `allocate_contiguous(MAX_TABLE_PAGES)` table storage with
  linker-symbol-backed storage.
- Explicitly zero the table arena.
- Defer general allocator initialization until after the EL2 switch.
- Remove transition-reclamation metadata and `complete_takeover` if it has no
  remaining non-U-Boot purpose.
- Continue retaining the live DTB pages explicitly.

### `src/main.rs` and `src/boot.rs`

- Remove parsing and insertion of `lmb=` and `bootmem=` regions.
- Remove the post-switch U-Boot-region reclamation step.
- Initialize the permanent allocator after takeover and report its resulting
  state directly.

### `tvisor_util/fdt.rs`

- Remove U-Boot LMB and boot-allocation argument parsers, errors, limits, and
  tests.
- Keep the `fdt=` parser and unrelated diagnostic arguments.

### `tvisor_util/system_info.rs` and `memory_map.rs`

- Remove bootloader-only reservation origins or owners if no remaining caller
  needs them.
- Collapse `initial_usable_ram` and `usable_ram` if their only difference was
  U-Boot transition ownership.
- Remove `transition_reserved` if it becomes empty by construction.
- Preserve permanent reservation metadata and conservative DTB policy.

### Documentation

- Simplify the README launch command.
- Recast `bdinfo` and LMB as deployment evidence, not runtime input.
- Update the memory-map and phase recap documents.
- Retain historical notes where they explain the reason for the former design.

These are expected changes, not an instruction to remove types mechanically.
Before deletion, each use must be audited for a non-U-Boot lifetime purpose.

## 8. Migration sequence

Implementation should occur as a separate post-Phase 9 change with small,
reviewable checkpoints:

1. Record the known-good Phase 9 build and Raspberry Pi serial output.
2. Add the linker arena without changing the table-allocation path; verify
   symbols and runtime footprint.
3. Make table construction use the embedded arena and verify the existing
   stage-1 switch and exception tests.
4. Move general allocator initialization to the post-switch path.
5. Remove `lmb=` and `bootmem=` from runtime parsing and memory normalization.
6. Remove obsolete transition-reclamation state.
7. Update launch documentation and hardware-validation scripts.
8. Re-run the unchanged Phase 9 guest test to prove behavioral equivalence.

The linker-arena change and the U-Boot-information removal should remain
separate commits if practical. That separation makes regressions in table
placement distinguishable from regressions in memory-map simplification.

## 9. Validation requirements

### Host and build validation

- The AArch64 ELF links successfully.
- `__bootstrap_tables_start` and `__bootstrap_tables_end` are page aligned.
- The arena is exactly 64 KiB and lies within the tvisor runtime footprint.
- The table-capacity calculation still fits within 16 pages.
- No pre-switch path calls the general physical-page allocator.
- No runtime parser accepts or requires `lmb=` or `bootmem=`.
- Memory-map tests confirm that permanent reservations remain excluded.
- Allocator tests confirm that the tvisor image and live DTB cannot be
  allocated.

### Raspberry Pi 4 validation

- Confirm the complete runtime footprint is safe in the selected load window.
- Boot without `lmb=` and `bootmem=` arguments.
- Reach every existing stage-1 takeover checkpoint.
- Confirm the private stack, vectors, UART, and exception handling operate
  after the switch.
- Confirm the original DTB remains readable.
- Allocate and access pages from both low RAM and the second DTB RAM bank.
- Re-run deliberate synchronous, guard-page, and unmapped-address tests.
- Re-run the unchanged Phase 9 guest payload and compare its output with the
  pre-change baseline.

## 10. Non-goals

This proposal does not:

- make tvisor position independent;
- allow U-Boot to choose an arbitrary load address;
- define a general firmware-neutral hypervisor handoff protocol;
- remove the `fdt=` runtime argument;
- eliminate DTB or firmware reservations;
- change guest IPA layout, stage-2 policy, or vCPU behavior;
- alter Phase 9 while it is under active development.

Supporting arbitrary placement would require a separate relocatable or
self-relocating image design. This proposal instead makes the existing fixed
Raspberry Pi 4 boot contract smaller, explicit, and testable.

## 11. Decision summary

After Phase 9 is complete, tvisor should embed its initial 64-KiB EL2
page-table arena in a dedicated linker section within the complete tvisor
runtime footprint. It should perform no general physical-memory allocation
before installing its private EL2 translation regime. Once takeover is
complete, it should initialize the allocator directly from DTB-described RAM
minus permanent reservations.

The `lmb=` and `bootmem=` arguments should then be removed from the runtime
interface. U-Boot LMB and `bdinfo` remain useful for validating the fixed load
window during development and deployment, but they no longer participate in
tvisor's memory-map construction or takeover correctness.
