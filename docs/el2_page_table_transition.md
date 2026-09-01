# Phase 7: EL2 page tables and takeover transition

## 1. Purpose and safety boundary

Phase 7 replaces U-Boot's active EL2 stage-1 translation regime with tables
owned by tvisor. It consumes the reviewed Phase 5 identity map and the private
stack/vector foundations from Phase 6.

This path is explicitly no-return. Tvisor always builds and installs these
tables after validating the U-Boot handoff. Guest stage 2 remains disabled
throughout Phase 7.

The work is split into independently testable parts:

1. pure layout calculation;
2. descriptor and table construction;
3. validation and table inspection;
4. private-stack/vector activation; and
5. a small assembly-only translation transition.

## 2. Preconditions

The takeover path must stop before writing registers unless all of these hold:

- execution is at EL2 with `HCR_EL2.E2H == 0` and `HCR_EL2.VM == 0`;
- little-endian execution and table walks are selected;
- 4 KiB stage-1 granules are supported;
- `ID_AA64MMFR0_EL1.PARange` can represent every mapped PA;
- every mapped PA is below the 39-bit identity-VA limit;
- the private stack and vector table from Phase 6 are active;
- asynchronous exceptions are masked;
- the UART CPU physical page is known;
- the final memory map has been moved into tvisor-owned static storage;
- page-table storage lies entirely in the linker-owned bootstrap arena; and
- all mandatory identity mappings pass a software table walk.

Failure before the no-return boundary uses the diagnostic error path and
halts. Failure after it enters the private exception handler and also halts.

## 3. Bootstrap table storage

The linker reserves sixteen 4 KiB pages in a dedicated, aligned
`.bootstrap_tables` `NOLOAD` section inside the tvisor runtime footprint.
Linker assertions and runtime checks validate its size and alignment.

The first provider is monotonic:

```text
allocate_table_page():
    verify next + 4096 <= arena_end
    return zeroed next
    next += 4096
```

Every allocated table page is zeroed before a descriptor points to it. The
complete runtime footprint, including the arena, is a permanent tvisor
reservation. The arena is mapped Normal WB/WA, read/write, execute-never.

For the current sparse layout, the expected shape is approximately one L1
root, separate L2 tables for low RAM and the high UART area, and L3 tables for
the image, bootstrap objects, and UART page. This is an observation, not a
fixed page count; section boundaries can change it.

## 4. Mapping requests

The builder consumes page-aligned requests of the form:

```rust
Mapping {
    va: VirtAddr,
    pa: PhysAddr,
    size: u64,
    memory_type: MemoryType,
    access: Access,
    executable: bool,
}
```

The initial requests are:

- transition code and `.text`: Normal, read-only, executable;
- vectors: Normal, read-only, executable;
- `.rodata`: Normal, read-only, XN;
- `.data`, `.bss`, and `.got`: Normal, read/write, XN;
- private boot stack: Normal, read/write, XN;
- bootstrap page-table arena: Normal, read/write, XN;
- UART page: Device-nGnRE, read/write, XN; and
- no descriptor for the stack guard page.

The DTB is not mapped in the preferred path because discovery is complete
before takeover. Any retained borrowed DTB reference is therefore a fatal
precondition failure.

Requests must be sorted and non-conflicting. Exact duplicate mappings may be
deduplicated; overlapping mappings with different PA, attributes, or
permissions are rejected.

## 5. Descriptor policy

The 39-bit, 4 KiB configuration starts at L1:

| Level | Coverage | Allowed leaf |
| --- | ---: | --- |
| L1 | 1 GiB | block |
| L2 | 2 MiB | block |
| L3 | 4 KiB | page |

The builder selects the largest aligned leaf that does not cross a request or
permission boundary. The minimal takeover map is expected to use L3 pages for
most objects.

Leaf descriptors set:

- `Valid = 1` and the level-appropriate block/page type;
- `AF = 1`;
- `SH = Inner Shareable` for Normal memory;
- `AttrIdx = 0` for Normal WB/WA or `1` for Device-nGnRE;
- EL2 read-only or read/write access as requested;
- `XN = 1` for non-executable mappings and `XN = 0` only for executable text
  and vectors. In the EL2 translation regime this is descriptor bit 54; the
  EL0/EL1 regime's `PXN` interpretation must not be used by the EL2 walker.

Table descriptors contain only a validated next-table PA and required table
type bits. Software-owned or reserved bits remain zero. Descriptor creation
rejects alignment errors, address truncation, unsupported PA bits, and
attempts to replace an existing incompatible entry.

## 6. Register values

The builder produces, but does not itself install:

```text
MAIR_EL2
  Attr0 = 0xFF  Normal WB/WA
  Attr1 = 0x04  Device-nGnRE

TCR_EL2
  RES1  = bits 31 and 23
  T0SZ  = 25    39-bit VA
  TG0   = 0b00  4 KiB
  SH0   = 0b11  Inner Shareable
  IRGN0 = 0b01  WB/WA
  ORGN0 = 0b01  WB/WA
  PS    = validated PARange encoding
  TBI   = 0

TTBR0_EL2
  BADDR = root-table PA
  CnP   = 0 initially
```

The final `SCTLR_EL2` value is constructed from architectural RES1 policy and
reviewed features, not by copying U-Boot's raw value. Phase 7 enables `M`, `C`,
`I`, `SA`, and `WXN`; it retains little-endian operation. Optional alignment
checking beyond stack alignment is deferred until its effect on MMIO and
existing code is tested.

## 7. Software validation before installation

An allocation-free walker checks every mandatory address:

- first and last instruction pages;
- representative rodata, data, and BSS addresses;
- current private `SP` and the stack top/bottom pages;
- vector base and every vector slot;
- root and subordinate table pages; and
- UART registers used by `debug_util`.

For each address it verifies output PA, memory type, write permission, and
execute permission. It also proves the guard page and representative holes
are invalid. The root address, all next-table addresses, and every table page
must lie within the reserved bootstrap arena.

## 8. Cache and table visibility policy

The first switch keeps instruction and data caches enabled across the short
MMU-disabled window. The new tables describe RAM with cacheability compatible
with the inherited working environment, use identity aliases only, and do not
introduce a second VA for the same PA.

After the final descriptor store, the builder executes a table-publication
operation ending in `DSB ISHST`. If architectural feature checks show that
explicit data-cache cleaning to the point of coherency is required for table
walk visibility, every written table line is cleaned before that barrier.
This decision must be implemented from architectural cache-identification
registers, not from a Cortex-A72 assumption hidden in the mapper.

Phase 7 does not globally clean or invalidate U-Boot's caches and does not
clear `SCTLR_EL2.C` or `.I`. A later change to cacheability or alias policy
requires a separate cache-transition design.

## 9. Assembly transition

The register switch is a leaf assembly routine placed in identity-mapped
executable text. It receives final `MAIR_EL2`, `TCR_EL2`, `TTBR0_EL2`, and
`SCTLR_EL2` values entirely in registers and touches no stack or literal pool
during the critical interval.

Conceptual sequence:

```asm
    // Private SP_EL2/VBAR_EL2 are already active; DAIF is masked.
    dsb     sy

    mrs     x9, sctlr_el2
    bic     x9, x9, #1              // M = 0; keep C and I unchanged
    msr     sctlr_el2, x9
    isb

    msr     mair_el2, x0
    msr     tcr_el2, x1
    msr     ttbr0_el2, x2
    isb

    tlbi    alle2                   // boot PE; SMP not active yet
    dsb     sy
    isb

    msr     sctlr_el2, x3           // tvisor M/C/I/SA/WXN policy
    isb

    b       post_switch_checkpoint  // identity address, never return
```

This is a design-level sequence, not code ready to copy verbatim. The
implementation must be checked against the Arm ARM version targeted by the
toolchain, including required ordering around control-register changes. The
routine must not use `ret`, because its caller belongs to the abandoned
translation environment.

`TLBI ALLE2` is sufficient only while the boot PE is the sole active tvisor
processor. Before secondary CPUs or shared translations are enabled, the
design must adopt the appropriate shareable invalidation and barriers.

## 10. Post-switch checkpoint

The first code after enabling translation performs only bounded operations:

1. write a fixed UART checkpoint without allocation;
2. read back `SCTLR_EL2`, `TCR_EL2`, `TTBR0_EL2`, and `MAIR_EL2`;
3. verify a stack-local canary;
4. read immutable and writable image canaries;
5. walk the active tables in software; and
6. report success, then halt.

The first hardware milestone does not initialize the physical allocator.
Phase 8 initializes it after this checkpoint; no U-Boot reclamation pass is
required.

After the positive test passes, separate opt-in tests deliberately access the
unmapped guard page and a representative unmapped VA. Each must enter the
private synchronous exception vector and report `ESR_EL2`, `ELR_EL2`, and
`FAR_EL2`. Fault tests run one per fresh boot and halt afterward.

## 11. Failure and recovery

There is no rollback after `SCTLR_EL2.M` is cleared. Any post-boundary failure:

- records a bounded error when UART remains available;
- halts with `DAIF` masked;
- does not branch to U-Boot; and
- requires board reset or smart-plug power recovery.

Checkpoints use unique numeric identifiers so the last visible checkpoint can
identify whether failure occurred before MMU disable, after register install,
after MMU enable, or during validation.

## 12. Modules and API boundaries

- `src/mm.rs`: request collection, layout calculation, table allocation,
  descriptor building, software walking, and register-value construction;
- `src/boot.rs`: no-return orchestration and assembly switch;
- `src/exception.rs`: translation-fault reporting;
- `scripts/rpi.ld`: page-aligned permission and transition symbols;
- `tvisor_util/aarch64_reg.rs`: typed architectural fields and reusable
  barrier/TLB helpers only; and
- `src/main.rs`: select the explicit takeover test, with no descriptor math.

The pure `mm` core should move under `tvisor_util` if keeping it in the binary
prevents host testing. Hardware register writes remain in the architecture or
boot layer, never in the layout calculator.

## 13. Verification gates

### Host

- Encode and decode every descriptor field.
- Map 4 KiB pages, 2 MiB blocks, and 1 GiB blocks.
- Split on alignment, attribute, and permission boundaries.
- Reject overlap conflicts, overflow, out-of-range PA, and table exhaustion.
- Walk first/last addresses and reject holes and the guard page.
- Calculate deterministic table counts for the Raspberry Pi fixture.

### AArch64 build

- Linker symbols are page-aligned and ordered.
- `readelf` shows vectors executable and stacks/tables writable and NOBITS as
  designed.
- `objdump` confirms the critical switch contains no stack access, literal
  load, call, or return.
- Emitted `MAIR_EL2` and `TCR_EL2` values match typed host-tested encodings.

### Raspberry Pi 4

1. Boot from a fresh reset and verify the fixed post-switch checkpoint.
2. Read back and print the active registers and stack pointer.
3. On separate fresh boots, verify guard-page and unmapped-VA faults.

Every test is no-return and performed only while verified power recovery is
available.

## 14. Open review questions

1. Does the targeted Arm architecture revision require additional ordering in
   the proposed MMU-disable/register-install sequence?
2. Is Device-nGnRE acceptable for the first UART mapping, or should it use the
   stricter Device-nGnRnE type?
3. Which cache-identification checks are sufficient to omit explicit table
   clean-to-PoC operations on the Cortex-A72?
4. Should the first implementation support L1/L2 leaves, or deliberately use
   only L3 pages to reduce builder complexity?
5. Which exact `SCTLR_EL2` RES1 and alignment-bit policy applies to the minimum
   supported Armv8-A revision?

## 15. Architectural references

- Arm, [Learn the Architecture: Memory Management](https://developer.arm.com/-/media/Arm%20Developer%20Community/PDF/LearnTheArchitecture-MemoryManagement-101811_0100_00_en.pdf).
- Arm, [Arm Architecture Reference Manual for A-profile architecture](https://developer.arm.com/documentation/ddi0487/latest/).
