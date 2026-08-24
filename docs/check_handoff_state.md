# U-Boot handoff-state diagnostic design

## 1. Purpose

Before tvisor replaces the execution environment inherited from U-Boot, it
must observe and document that environment. This diagnostic phase answers:

- Which core and exception level entered tvisor?
- Which stack pointer is active, and is it aligned?
- Are the EL2 MMU and caches enabled?
- Which EL2 translation tables and memory attributes are installed?
- Is stage-2 translation already enabled?
- Where are the inherited exception vectors, and which exceptions are masked?
- Which architectural features are implemented by the Cortex-A72?
- Can tvisor report the state and return without changing it?

The result will be evidence for a later, permanent handoff design. It is not
the permanent handoff itself.

## 2. Scope and safety contract

The diagnostic runs as an ELF application invoked by U-Boot `bootelf`. It uses
the U-Boot stack, translation tables, exception vectors, interrupt state, and
mini-UART configuration, then returns through the normal AArch64 calling
convention.

During this phase tvisor may:

- read architectural system registers available at EL2;
- copy their values into an ordinary Rust snapshot on the current stack;
- decode and print the values through the inherited mini UART;
- append invariant failures to the debug error stack;
- read the current SP and check its alignment.

It must not:

- write any register described in this document;
- change `SCTLR_EL2`, the cache state, or the MMU state;
- change `HCR_EL2`, `TCR_EL2`, `TTBR0_EL2`, or `MAIR_EL2`;
- replace `VBAR_EL2` or alter `DAIF`;
- install a private stack or translation table;
- initialize or reconfigure the UART or GPIO pins;
- execute cache, TLB, or branch-predictor maintenance;
- enable secondary cores;
- enter a guest or execute `ERET`;
- retain pointers into the U-Boot stack after returning.

The only intentionally modified external state is:

- bytes appended to `[0x0000_1000, 0x0000_1100)`;
- bytes transmitted through the mini UART.

The UART must be drained before returning to U-Boot.

## 3. Diagnostic sequence

The implementation order is fixed:

1. Capture all readable handoff registers into a local snapshot.
2. Initialize and clear the debug error stack.
3. Validate only the invariants defined as errors in this document.
4. Append each failed invariant to the error stack.
5. Print all raw values and selected decoded fields through UART.
6. Capture a second snapshot of the system registers that must remain stable.
7. Compare the stable fields and record an error if any changed.
8. Drain UART output.
9. Return zero if no fatal invariant failed; otherwise return one.

The initial capture comes before `debug_init()` so that the snapshot is as
close as possible to the Rust entry point. It still is not the exact machine
state at the `bootelf` branch: the compiler-generated function prologue can
already have adjusted SP and used the U-Boot stack.

Capturing the exact entry-time general-purpose registers and SP requires a
future assembly entry shim. That work belongs to the permanent-handoff phase.

## 4. Snapshot model

The snapshot is a temporary Rust value, not a new format in debug memory. The
byte error stack remains the only low-memory diagnostic format.

The conceptual snapshot is:

```rust
struct HandoffState {
    current_el: u64,
    mpidr_el1: u64,
    spsel: u64,
    sp: u64,

    sctlr_el2: u64,
    hcr_el2: u64,
    tcr_el2: u64,
    ttbr0_el2: u64,
    mair_el2: u64,

    vtcr_el2: u64,
    vttbr_el2: u64,

    vbar_el2: u64,
    daif: u64,
    cptr_el2: u64,

    cnthctl_el2: u64,
    cntvoff_el2: u64,

    id_aa64pfr0_el1: u64,
    id_aa64mmfr0_el1: u64,
}
```

Every field stores the unmodified raw register value. Decoded values are
derived from this snapshot rather than replacing raw values.

The structure does not need `#[repr(C)]` while it remains local Rust data and
is never exchanged across an ABI or persisted. Add a representation attribute
only if a future interface requires a defined binary layout.

## 5. Register access

Use one small function per system register. A register name is part of the A64
instruction encoding and cannot be supplied as an ordinary runtime argument.

For example:

```rust
#[inline(always)]
fn read_sctlr_el2() -> u64 {
    let value: u64;

    unsafe {
        core::arch::asm!(
            "mrs {value}, SCTLR_EL2",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }

    value
}
```

Read the current SP with `MOV`, not `MRS`:

```rust
#[inline(always)]
fn read_sp() -> u64 {
    let value: u64;

    unsafe {
        core::arch::asm!(
            "mov {value}, sp",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }

    value
}
```

These functions are safe wrappers because the register reads have no caller-
visible side effects and are only compiled for the AArch64 EL2 target. The
inline assembly remains the internal unsafe operation.

## 6. Register specification and diagnostic policy

### 6.1 `CurrentEL`

`CurrentEL[3:2]` encodes the current exception level. Decode it with:

```text
EL = (CurrentEL >> 2) & 0b11
```

Policy:

- EL2 is required.
- Any other value appends `InvalidEL2State` and makes the return status one.
- If execution is not at EL2, do not read EL2-only registers; report the error,
  drain UART if possible, and return immediately.

### 6.2 `MPIDR_EL1`

`MPIDR_EL1` is the Multiprocessor Affinity Register. It identifies the
processing element (PE) on which tvisor is executing and describes that PE’s
position in the system affinity hierarchy. Software can read it at EL2 with:

```asm
mrs xN, MPIDR_EL1
```

Reading the register does not select or start a core. Each PE has its own
`MPIDR_EL1` value, and the combined affinity fields identify that PE within the
system.

| Bits | Field | Description |
| --- | --- | --- |
| `[63:40]` | `RES0` | Reserved; reads as zero. |
| `[39:32]` | `Aff3` | Affinity level 3 identifier. |
| `31` | `RES1` | Reserved; reads as one. It is not the uniprocessor flag. |
| `30` | `U` | Uniprocessor-system flag. One means the system contains one PE; zero means it is a multiprocessor system. |
| `[29:25]` | `RES0` | Reserved; reads as zero. |
| `24` | `MT` | Performance-interdependence indicator for PEs at affinity level 0. |
| `[23:16]` | `Aff2` | Affinity level 2 identifier. |
| `[15:8]` | `Aff1` | Affinity level 1 identifier. |
| `[7:0]` | `Aff0` | Affinity level 0 identifier. |

`Aff0` is the lowest affinity level and `Aff3` is the highest. The hierarchy
describes implementation-defined processor topology; software must not assume
that a particular level always means thread, core, cluster, or socket on every
machine. Compare the complete tuple `(Aff3, Aff2, Aff1, Aff0)` when identifying
a PE. The affinity fields are extracted as follows:

```text
Aff0 = (MPIDR_EL1 >>  0) & 0xff
Aff1 = (MPIDR_EL1 >>  8) & 0xff
Aff2 = (MPIDR_EL1 >> 16) & 0xff
Aff3 = (MPIDR_EL1 >> 32) & 0xff
```

When `MT` is one, PEs that differ only in `Aff0` are very interdependent in
their performance. This can describe processing elements that share substantial
execution resources. `MT == 0` does not prove that no resources are shared, so
the diagnostic records this bit but does not use it as a topology invariant.

On Raspberry Pi 4, U-Boot normally enters tvisor on the boot PE with affinity
`0.0.0.0`. A typical raw value is `0x0000000080000000`: bit 31 is the required
`RES1` value, while `U == 0`, `MT == 0`, and all four affinity fields are zero.
Other cores normally differ in `Aff0`, but tvisor should decode the register
rather than relying on that board-specific expectation.

Policy:

- Print the raw value, `U`, `MT`, and all four affinity fields.
- The initial diagnostic expects the U-Boot boot core, normally affinity
  `0.0.0.0`, but a different affinity is observational rather than fatal.
- Do not treat bit 31 as part of the affinity or as evidence of a uniprocessor
  system.
- Do not start or modify another core.

### 6.3 `SPSel` and `SP`

`SPSel` is the Stack Pointer Select register. It determines which stack-pointer
register is named by the architectural `SP` operand while executing at an
exception level that has its own stack pointer. At EL2, it selects between
`SP_EL0` and `SP_EL2`.

| Bits | Field | Description |
| --- | --- | --- |
| `[63:1]` | `RES0` | Reserved; reads as zero. |
| `0` | `SP` | Selects the stack pointer used by the current exception level. |

The selection has the following meaning while tvisor executes at EL2:

```text
SPSel.SP == 0: the SP operand accesses SP_EL0 (EL2t)
SPSel.SP == 1: the SP operand accesses SP_EL2 (EL2h)
```

The `t` and `h` suffixes describe the stack selection used by an exception
level: `t` uses `SP_EL0`, while `h` uses that exception level’s dedicated stack
pointer. Therefore, at EL2, `EL2t` uses `SP_EL0` and `EL2h` uses `SP_EL2`.
This selection does not change the current exception level.

`SP_EL0` and `SP_EL2` are separate hardware registers that can each hold a
stack address:

- `SP_EL0` is the stack pointer that EL0 always uses. Higher exception levels
  can also select this shared stack pointer with `SPSel.SP == 0`.
- `SP_EL2` is the dedicated EL2 stack pointer. Only execution at EL2 can use it
  as the current `SP`, by selecting `SPSel.SP == 1`.

The available selection depends on the current exception level:

| Current EL | Stack-pointer choices |
| --- | --- |
| EL0 | `SP_EL0` only |
| EL1 | `SP_EL0` or `SP_EL1` |
| EL2 | `SP_EL0` or `SP_EL2` |
| EL3 | `SP_EL0` or `SP_EL3` |

Having a dedicated stack pointer lets exception handlers avoid relying on the
stack used by lower-level software. For example, when execution enters tvisor
at EL2 because of an exception from a guest, EL2 can use `SP_EL2` immediately
instead of using the guest-visible `SP_EL0`. A typical future arrangement is:

```text
EL2 hypervisor code -> SP_EL2 -> private per-core tvisor stack
EL1 guest kernel    -> SP_EL1 -> guest kernel stack
EL0 guest process   -> SP_EL0 -> guest userspace stack
```

Each participating core will eventually require its own valid EL2 stack even
though the architectural stack-pointer register is called `SP_EL2`. The
multicore entry path must choose and install that core’s stack before running
ordinary Rust code on the core.

During the current `bootelf` diagnostic, tvisor has not installed its own
stack. It is still using whichever stack pointer and stack memory U-Boot handed
over. The diagnostic therefore observes both `SPSel` and the selected `SP`
value without assuming that the inherited selection is tvisor’s permanent
configuration.

Read `SPSel` with `MRS` and retain only bit 0:

```rust
#[inline(always)]
fn read_spsel() -> u64 {
    let value: u64;

    unsafe {
        core::arch::asm!(
            "mrs {value}, SPSel",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }

    value & 1
}
```

`SPSel` tells tvisor which stack-pointer register is selected, but it does not
contain the stack address. Read the address currently named by `SP` separately
with `MOV`:

```rust
#[inline(always)]
fn read_sp() -> u64 {
    let value: u64;

    unsafe {
        core::arch::asm!(
            "mov {value}, sp",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }

    value
}
```

If `SPSel.SP == 0`, this value came from `SP_EL0`; if `SPSel.SP == 1`, it came
from `SP_EL2`. The diagnostic must not change `SPSel`, because selecting another
stack before installing a valid stack value can immediately make normal Rust
stack accesses unsafe.

The AArch64 procedure-call standard requires SP to be 16-byte aligned at a
public interface.

Policy:

- Print the raw `SPSel` value, decoded selected register (`SP_EL0` or `SP_EL2`),
  current SP address, and `SP & 0xF`.
- Either stack selection is recorded as inherited state, not rejected.
- A nonzero `SP & 0xF` appends `InvalidStackAlignment` and makes the return
  status one.
- Do not write `SPSel`, `SP_EL0`, or `SP_EL2` during the diagnostic phase.
- This is an observation after the Rust function prologue, not the exact SP at
  the branch from U-Boot.

### 6.4 `SCTLR_EL2`

Relevant fields are:

| Bit | Field | Meaning |
| --- | --- | --- |
| 0 | `M` | EL2 stage-1 MMU enable |
| 1 | `A` | Alignment checking enable |
| 2 | `C` | Data/unified cache enable |
| 3 | `SA` | EL2 stack-alignment checking enable |
| 12 | `I` | Instruction-cache enable |
| 19 | `WXN` | Writable mappings execute-never |
| 25 | `EE` | EL2 data endianness; zero is little-endian |

Policy:

- Print the raw value and every field above.
- `M`, `C`, and `I` are observations; the diagnostic accepts either value.
- `EE == 1` conflicts with the little-endian tvisor build, appends
  `UnexpectedEL2Endianness`, and makes the return status one.
- Never write `SCTLR_EL2` in the diagnostic phase.

### 6.5 `HCR_EL2`

`HCR_EL2` is the Hypervisor Configuration Register. It controls how EL2
virtualizes EL1 and EL0: stage-2 translation, physical-exception routing,
virtual exceptions, traps, and EL1's execution state. Most fields affect
execution below EL2; reading the register does not alter current EL2 execution.

Read it with `mrs xN, HCR_EL2`. The Cortex-A72 implements the original
Armv8-A controls relevant to this diagnostic:

| Bits | Field | Meaning when set |
| --- | --- | --- |
| `0` | `VM` | Enables stage-2 translation for Non-secure EL1/EL0 using `VTCR_EL2` and `VTTBR_EL2`. It does not enable EL2 stage 1. |
| `1` | `SWIO` | Changes treatment of AArch32 set/way cache maintenance. |
| `2` | `PTW` | With stage 2 active, faults a stage-1 table walk whose stage-2 attributes are not Normal memory. |
| `3` | `FMO` | Routes physical FIQ to EL2. |
| `4` | `IMO` | Routes physical IRQ to EL2. |
| `5` | `AMO` | Routes physical SError to EL2. |
| `6` | `VF` | Makes a virtual FIQ pending when delivery conditions permit. |
| `7` | `VI` | Makes a virtual IRQ pending when delivery conditions permit. |
| `8` | `VSE` | Makes a virtual SError pending. |
| `9` | `FB` | Forces broadcast behavior for selected cache/TLB maintenance. |
| `[11:10]` | `BSU` | Upgrades the shareability domain of barriers below EL2. |
| `12` | `DC` | Default-cacheability control for EL1/EL0 stage 1; it is unrelated to `SCTLR_EL2.C`. |
| `13` | `TWI` | Traps eligible `WFI` (Wait For Interrupt) execution to EL2. |
| `14` | `TWE` | Traps eligible `WFE` (Wait For Event) execution to EL2. |
| `[18:15]` | `TID0..3` | Trap defined groups of feature-identification register accesses. |
| `19` | `TSC` | Traps an EL1 `SMC` instruction to EL2. |
| `20` | `TIDCP` | Traps selected implementation-defined functionality. |
| `21` | `TACR` | Traps AArch32 auxiliary-control-register accesses. |
| `22` | `TSW` | Traps AArch32 set/way cache maintenance. |
| `23` | `TPCP` | Traps cache maintenance by physical address. |
| `24` | `TPU` | Traps cache maintenance to Point of Unification. |
| `25` | `TTLB` | Traps selected EL1 TLB-maintenance instructions. |
| `26` | `TVM` | Traps writes to selected EL1 virtual-memory control registers. |
| `27` | `TGE` | Routes general exceptions normally handled at EL1 to EL2, changing the overall EL1/EL0 exception model. |
| `28` | `TDZ` | Traps AArch32 use of `DC ZVA`. |
| `29` | `HCD` | Disables normal use of `HVC` at EL1. |
| `30` | `TRVM` | Traps selected reads of EL1 virtual-memory control registers. |
| `31` | `RW` | Selects EL1 execution state: one is AArch64, zero is AArch32. It does not change EL2's state. |

Do not confuse physical routing with virtual injection. `FMO`, `IMO`, and
`AMO` route physical exceptions to EL2; `VF`, `VI`, and `VSE` make
virtual exceptions pending below EL2. Guest `PSTATE.DAIF` and the interrupt
controller also participate, so HCR fields alone do not describe delivery.

`VM` adds a second translation stage:

```text
EL1/EL0 VA -- stage 1 (EL1 registers) --> IPA
           -- stage 2 (VM, VTCR_EL2, VTTBR_EL2) --> PA
```

When `VM == 0`, nonzero `VTCR_EL2` or `VTTBR_EL2` values can be stale.
When `VM == 1`, both stages must permit an access and stage-2 faults are
handled at EL2.

For an AArch64 guest, permanent handoff will eventually require `RW == 1`
before entering EL1. An inherited zero is only observational while tvisor is
already executing as AArch64 at EL2.

Later Arm revisions add optional fields above bit 31. Decode those only after
feature-register checks establish that they exist. Always retain the complete
raw `u64`. Some fields have architecturally unknown reset values, and U-Boot
can leave its own configuration; permanent handoff must construct a deliberate
value rather than reuse this snapshot.

Policy:

- Print the raw value and at least `VM`, `PTW`, `FMO`, `IMO`, `AMO`,
  `VF`, `VI`, `VSE`, `TWI`, `TWE`, `TTLB`, `TVM`, `TGE`, `HCD`,
  `TRVM`, and `RW`.
- `VM` reports whether stage-2 translation for EL1/EL0 is enabled; it does not
  control EL2 stage-1 translation.
- `RW` reports the intended execution state of EL1 and does not affect the
  current AArch64 execution at EL2.
- No inherited field is rejected during the observation phase.
- If `VM == 1`, clearly mark `VTCR_EL2` and `VTTBR_EL2` as active state in the
  report.
- Report physical-routing and virtual-pending controls separately.
- Compare raw `HCR_EL2` in the before/after snapshots. A change appends
  `HandoffStateChanged`.
- Never write `HCR_EL2` in the diagnostic phase. Changing exception routing
  or trap controls without valid EL2 vectors can make the next event
  unrecoverable.

### 6.6 `TCR_EL2`, `TTBR0_EL2`, and `MAIR_EL2`

These three registers describe EL2 stage-1 translation. They form one
configuration and must be interpreted together:

```text
EL2 virtual address
    |
    | TCR_EL2 selects address size, granule, and table-walk attributes
    | TTBR0_EL2 points to the first translation table
    | table descriptors select MAIR_EL2 attribute bytes
    v
physical address
```

They affect ordinary EL2 translation only when `SCTLR_EL2.M == 1`. A raw
value can remain nonzero after the MMU is disabled, so the diagnostic records
every register but labels the group inactive when `M == 0`.

#### 6.6.1 `TCR_EL2`

`TCR_EL2` is the Translation Control Register for the non-VHE EL2 stage-1
regime used by the Cortex-A72.

| Bits | Field | Meaning |
| --- | --- | --- |
| `[5:0]` | `T0SZ` | Size offset of the input virtual-address region. The nominal input address width is `64 - T0SZ`. |
| `[9:8]` | `IRGN0` | Inner cacheability used for translation-table walks. |
| `[11:10]` | `ORGN0` | Outer cacheability used for translation-table walks. |
| `[13:12]` | `SH0` | Shareability used for translation-table walks. |
| `[15:14]` | `TG0` | Translation granule selected for the table rooted at `TTBR0_EL2`. |
| `[18:16]` | `PS` | Maximum physical output-address size of the EL2 translation regime. |
| `20` | `TBI` | When implemented and enabled, the top byte of applicable EL2 virtual addresses is ignored for translation. |

`IRGN0` and `ORGN0` use the same encoding:

| Encoding | Table-walk cacheability |
| --- | --- |
| `00` | Normal memory, Non-cacheable |
| `01` | Normal memory, Write-Back Read-Allocate Write-Allocate |
| `10` | Normal memory, Write-Through Read-Allocate, no Write-Allocate |
| `11` | Normal memory, Write-Back Read-Allocate, no Write-Allocate |

`SH0` is:

| Encoding | Shareability |
| --- | --- |
| `00` | Non-shareable |
| `01` | Reserved |
| `10` | Outer Shareable |
| `11` | Inner Shareable |

`TG0` is:

| Encoding | Granule |
| --- | --- |
| `00` | 4 KiB |
| `01` | 64 KiB |
| `10` | 16 KiB |
| `11` | Reserved |

`PS` encodes the maximum output PA size:

| Encoding | PA width |
| --- | --- |
| `000` | 32 bits, 4 GiB |
| `001` | 36 bits, 64 GiB |
| `010` | 40 bits, 1 TiB |
| `011` | 42 bits, 4 TiB |
| `100` | 44 bits, 16 TiB |
| `101` | 48 bits, 256 TiB |
| `110` | 52 bits, when the required architecture feature is implemented |
| `111` | Reserved |

An encoding is not proof that the processor implements it. Validate granule
and PA-size support using `ID_AA64MMFR0_EL1` before constructing a permanent
`TCR_EL2`. `T0SZ`, `TG0`, and the starting table level together determine
the number of table levels and the alignment required for the root table.

#### 6.6.2 `TTBR0_EL2`

`TTBR0_EL2` is Translation Table Base Register 0 for EL2. On the baseline
Cortex-A72 translation regime:

| Bits | Field | Meaning |
| --- | --- | --- |
| `[47:1]` | `BADDR` | Physical base-address field for the initial EL2 stage-1 translation table. |
| `0` | `CnP` or `RES0` | Common-not-Private when `FEAT_TTCNP` exists; otherwise reserved zero. Cortex-A72 does not provide this later optional feature. |
| `[63:48]` | `RES0` | Reserved in the baseline non-VHE Cortex-A72 regime. |

The useful root address is not obtained safely by applying one universal
alignment mask. Which low `BADDR` bits are valid depends on `TCR_EL2.T0SZ`,
`TG0`, the initial lookup level, and implemented PA size. The diagnostic may
print the baseline `BADDR[47:1]` field, but it must not walk or dereference the
table until those constraints have been validated.

`TTBR0_EL2` belongs to tvisor's EL2 stage 1. Do not confuse it with
`VTTBR_EL2`, which points to a guest's stage-2 tables.

#### 6.6.3 `MAIR_EL2`

`MAIR_EL2` contains eight independent memory-attribute encodings:

```text
Attr0 = MAIR_EL2[7:0]
Attr1 = MAIR_EL2[15:8]
...
Attr7 = MAIR_EL2[63:56]
```

An EL2 stage-1 leaf descriptor contains a three-bit `AttrIndx` field. Value
`N` selects `MAIR_EL2.AttrN`. Unreferenced attribute bytes have no effect,
even if they are nonzero.

Common complete attribute-byte encodings are:

| Value | Memory type |
| --- | --- |
| `0x00` | Device-nGnRnE |
| `0x04` | Device-nGnRE |
| `0x08` | Device-nGRE |
| `0x0c` | Device-GRE |
| `0x44` | Normal memory, Outer and Inner Non-cacheable |
| `0xff` | Normal memory, Outer and Inner Write-Back, Read-Allocate and Write-Allocate |

For Normal memory, the upper nibble defines Outer cacheability and the lower
nibble defines Inner cacheability. Device encodings describe gathering,
reordering, and early-write-acknowledgement properties. Tvisor must use Device
memory for MMIO and suitable Normal memory for RAM; an incorrect memory type
can break device ordering or cache coherency.

Policy:

- Always print the raw values.
- Print `TCR_EL2.T0SZ`, nominal VA width, `IRGN0`, `ORGN0`, `SH0`, `TG0`,
  `PS`, and `TBI`.
- Print the baseline `TTBR0_EL2.BADDR[47:1]` field and bit 0, but label bit 0
  reserved on Cortex-A72 rather than claiming that `CnP` is implemented.
- Print all eight raw `MAIR_EL2` attribute bytes.
- Decode the group as active translation state only when `SCTLR_EL2.M == 1`.
- When `M == 0`, label them inactive/stale rather than assuming that their
  contents describe current accesses.
- Do not walk U-Boot page tables in this milestone. A table walker requires
  careful validation of granule, levels, physical size, descriptor type, and
  memory accessibility and should be designed separately.
- Never write any of these registers during the diagnostic phase.

### 6.7 `VTCR_EL2` and `VTTBR_EL2`

These two registers configure stage-2 translation for EL1/EL0 guests. Stage 2
maps an Intermediate Physical Address (IPA), supplied by a guest's stage-1
translation, to the final Physical Address. `VTCR_EL2` selects the translation
regime's geometry and memory attributes, and `VTTBR_EL2` points at the initial
stage-2 translation table:

```text
guest virtual address
    |
    | guest stage-1 translation (controlled by the guest)
    v
intermediate physical address (IPA)
    |
    | VTCR_EL2 selects address size, granule, and table-walk attributes
    | VTTBR_EL2 points to the first stage-2 translation table
    v
physical address
```

Stage-2 translation is active only when `HCR_EL2.VM == 1`. The registers can
retain nonzero values while stage 2 is disabled, so the diagnostic records
them but labels the group inactive when `VM == 0`.

#### 6.7.1 `VTCR_EL2`

`VTCR_EL2` is the Virtualization Translation Control Register. It selects the
size and starting level of the stage-2 translation regime, the granule, the
shareability and cacheability of stage-2 walks, and the maximum output
physical-address size.

| Bits | Field | Meaning |
| --- | --- | --- |
| `[5:0]` | `T0SZ` | Size offset of the stage-2 input address space. The IPA width is `64 - T0SZ`. |
| `[7:6]` | `SL0` | Starting level of the stage-2 lookup. |
| `[9:8]` | `IRGN0` | Inner cacheability used for stage-2 translation-table walks. |
| `[11:10]` | `ORGN0` | Outer cacheability used for stage-2 translation-table walks. |
| `[13:12]` | `SH0` | Shareability used for stage-2 translation-table walks. |
| `[15:14]` | `TG0` | Stage-2 translation granule. |
| `[18:16]` | `PS` | Maximum physical output-address size of the stage-2 regime. |
| `[19]` | `VS` | VMID Size (`FEAT_VMID16`): `0` selects an 8-bit VMID, `1` a 16-bit VMID. |
| `[32]` | `DS` | 52-bit stage-2 output-address and IPA support (`FEAT_LPA2`). |

Bit `31` is `RES1`. Bit `30` is `NSA` (`FEAT_SEL2`), not `DS`. The other
unlisted fields (`HA[21]`, `HD[22]`, `HWU59`–`HWU62[25:28]`, `NSW[29]`,
`SL2[33]`, and so on) are feature-dependent and are `RES0` on the Cortex-A72
when their corresponding features are not implemented.

`IRGN0` and `ORGN0` use the same encoding as `TCR_EL2`:

| Encoding | Table-walk cacheability |
| --- | --- |
| `00` | Normal memory, Non-cacheable |
| `01` | Normal memory, Write-Back Read-Allocate Write-Allocate |
| `10` | Normal memory, Write-Through Read-Allocate, no Write-Allocate |
| `11` | Normal memory, Write-Back Read-Allocate, no Write-Allocate |

`SH0` is:

| Encoding | Shareability |
| --- | --- |
| `00` | Non-shareable |
| `01` | Reserved |
| `10` | Outer Shareable |
| `11` | Inner Shareable |

`TG0` is:

| Encoding | Granule |
| --- | --- |
| `00` | 4 KiB |
| `01` | 64 KiB |
| `10` | 16 KiB |
| `11` | Reserved |

`PS` encodes the maximum output PA size with the same encodings as
`TCR_EL2.PS`:

| Encoding | PA width |
| --- | --- |
| `000` | 32 bits, 4 GiB |
| `001` | 36 bits, 64 GiB |
| `010` | 40 bits, 1 TiB |
| `011` | 42 bits, 4 TiB |
| `100` | 44 bits, 16 TiB |
| `101` | 48 bits, 256 TiB |
| `110` | 52 bits, when the required architecture feature is implemented |
| `111` | Reserved |

`SL0` selects the starting level of the stage-2 lookup. For the baseline 4 KiB
granule the encoding is:

| `SL0` | Starting level (4 KiB) |
| --- | --- |
| `10` | level 0 |
| `01` | level 1 |
| `00` | level 2 |
| `11` | level 3 (`FEAT_TTST`) |

`T0SZ` and `SL0` together fix the stage-2 input-address width and the number
of table levels:

| `T0SZ` | IPA width | `SL0` | Starting level |
| --- | --- | --- | --- |
| `16` | 48 bits | `10` | level 0 (levels 0–3) |
| `25` | 39 bits | `01` | level 1 (levels 1–3) |
| `34` | 30 bits | `00` | level 2 (below the 32-bit minimum) |
| `43` | 21 bits | `11` | level 3 (below the 32-bit minimum) |

The minimum stage-2 input-address width is 32 bits, so only the `T0SZ == 16`
and `T0SZ == 25` rows are valid starting levels with a 4 KiB granule. Validate
granule and PA-size support against `ID_AA64MMFR0_EL1` before constructing a
permanent `VTCR_EL2`.

#### 6.7.2 `VTTBR_EL2`

`VTTBR_EL2` is the Virtualization Translation Table Base Register. It holds the
base address of the initial stage-2 translation table:

| Bits | Field | Meaning |
| --- | --- | --- |
| `[63:48]` | `VMID` | Virtual Machine Identifier. With an 8-bit VMID (`VTCR_EL2.VS == 0`) only `[55:48]` are used and `[63:56]` are `RES0`; with `FEAT_VMID16` and `VS == 1` all 16 bits are used. |
| `[47:x]` | `BADDR` | Physical base address of the starting-level stage-2 table. |
| `[x-1:1]` | `RES0` | Reserved; must be zero. `x` is the alignment required for the starting table. |
| `[0]` | `CnP` | Common-not-Private (`FEAT_TTCNP`); `RES0` otherwise. |

The table base must be aligned to the size of the starting-level translation
table, so the number of valid `BADDR` bits depends on `VTCR_EL2.T0SZ`, `SL0`,
and `TG0`. A starting table is not necessarily one complete granule in size: it
can hold fewer entries than a full granule-sized table. For the common case of
the 4 KiB granule starting at level 0, the starting table is a full 4 KiB (512
entries), so `BADDR[47:12]` hold the base address and `[11:1]` are `RES0`:

| Starting table size | Valid `BADDR` | `RES0` low bits |
| --- | --- | --- |
| 4 KiB | `[47:12]` | `[11:1]` |
| 16 KiB | `[47:14]` | `[13:1]` |
| 64 KiB | `[47:16]` | `[15:1]` |

`VTTBR_EL2` bit 0 is a `CnP` (Common-not-Private) hint when `FEAT_TTCNP` is
implemented, and `RES0` otherwise. `VTTBR_EL2` belongs to a guest's stage-2
tables and must not be confused with `TTBR0_EL2`, which points to tvisor's own
EL2 stage-1 tables.

Policy:

- Always capture and print their raw values.
- Treat them as active only if `HCR_EL2.VM == 1`.
- Print `VTCR_EL2.T0SZ`, nominal IPA width, `SL0`, `IRGN0`, `ORGN0`, `SH0`,
  `TG0`, `PS`, `VS`, and `DS`.
- Print `VTTBR_EL2.VMID`, `BADDR[47:1]`, and `CnP`; the valid `BADDR` bits
  depend on `VTCR_EL2.T0SZ`, `SL0`, and `TG0`.
- Do not reject nonzero inactive values because firmware can leave stale
  configuration in disabled registers.
- Do not disable stage 2 or invalidate stage-2 TLB entries in this phase.

#### 6.7.3 Testing

The accessor unit tests in `tvisor_util/diag.rs` build synthetic raw register
values and can be run on the host, because the bare-metal `aarch64-unknown-none`
target has no Rust test harness:

```bash
cargo test --lib --target x86_64-unknown-linux-gnu
```

The `.cargo/config.toml` `test-host` alias is equivalent:

```bash
cargo test-host
```

### 6.8 `VBAR_EL2`

`VBAR_EL2` is the Vector Base Address Register for EL2. It holds the base
address of the exception-vector table used when an exception is taken to EL2.
On Cortex-A72 the table occupies 2048 bytes (16 vector entries × 128 bytes), so
the base address must be 2048-byte aligned.

| Bits | Field | Meaning |
| --- | --- | --- |
| `[10:0]` | `RES0` | Reserved; must be zero. This enforces the 2048-byte alignment. |
| `[63:11]` | `VectorBase` | Base address of the EL2 exception-vector table. |

The AArch64 vector table contains 16 entries of 128 bytes each, selected by
the exception type and the source of the exception:

| Exception type | Source | Offset |
| --- | --- | --- |
| Synchronous | Current EL with `SP_EL0` (`EL2t`) | `0x000` |
| IRQ | Current EL with `SP_EL0` | `0x080` |
| FIQ | Current EL with `SP_EL0` | `0x100` |
| SError | Current EL with `SP_EL0` | `0x180` |
| Synchronous | Current EL with `SP_ELx` (`EL2h`) | `0x200` |
| IRQ | Current EL with `SP_ELx` | `0x280` |
| FIQ | Current EL with `SP_ELx` | `0x300` |
| SError | Current EL with `SP_ELx` | `0x380` |
| Synchronous | Lower EL, AArch64 | `0x400` |
| IRQ | Lower EL, AArch64 | `0x480` |
| FIQ | Lower EL, AArch64 | `0x500` |
| SError | Lower EL, AArch64 | `0x580` |
| Synchronous | Lower EL, AArch32 | `0x600` |
| IRQ | Lower EL, AArch32 | `0x680` |
| FIQ | Lower EL, AArch32 | `0x700` |
| SError | Lower EL, AArch32 | `0x780` |

The diagnostic does not dereference these entries. It only validates that the
inherited base address is aligned and records it for the permanent handoff.

Policy:

- Print the raw address and `VBAR_EL2 & 0x7FF`.
- A nonzero low field appends `InvalidVectorBaseAlignment` and makes the return
  status one.
- Do not inspect or invoke the vector entries and never replace `VBAR_EL2`.

### 6.9 `DAIF`

`DAIF` is a special-purpose register that exposes the exception mask bits of
the current PSTATE. It is a view of `PSTATE.D`, `PSTATE.A`, `PSTATE.I`, and
`PSTATE.F`:

| Bit | Field | Meaning |
| --- | --- | --- |
| `9` | `D` | Masks eligible debug exceptions. |
| `8` | `A` | Masks SError exceptions. |
| `7` | `I` | Masks IRQ exceptions. |
| `6` | `F` | Masks FIQ exceptions. |

Bits `[63:10]` and `[5:0]` are `RES0`.

A mask value of `1` means the corresponding exception class is masked; `0`
means unmasked, subject to the architecture's exception-routing and priority
rules.

Access:

- At EL1–EL3, `MRS DAIF` is accessible.
- At EL0, access depends on `SCTLR_EL1.UMA`; when `UMA == 0`, an EL0 `MRS DAIF`
  traps to EL1 or EL2 according to the configured routing. The diagnostic
  therefore reads `DAIF` only at EL1–EL3 and treats EL0 as unavailable.
- The warm-reset values of `D`, `A`, `I`, and `F` are `1` (all masked).

`MRS DAIF` reads the combined state. `MSR DAIF, Xt` writes it, while `DAIFSet`
and `DAIFClr` selectively set or clear individual mask bits.

Policy:

- Print the raw value and all four masks.
- Any combination is observational during this phase.
- Read `DAIF` only, at EL1–EL3; never use `MSR DAIF`, `DAIFSet`, or `DAIFClr`.
- Compare `DAIF` before and after reporting; a change appends
  `HandoffStateChanged`. This before/after comparison belongs to the section 8
  stable-state phase and is intentionally deferred from this section.

### 6.10 `CPTR_EL2`

`CPTR_EL2` controls traps for floating-point, Advanced SIMD, and related
coprocessor functionality. On the Cortex-A72, `HCR_EL2.E2H` is effectively `0`,
so the baseline Armv8.0 register layout applies, with the fixed field layout:

| Bits | Field | Meaning |
| --- | --- | --- |
| `[63:32]` | `RES0` | Reserved. |
| `[31]` | `TCPAC` | Traps EL1 access to `CPACR_EL1`. |
| `[30:21]` | `RES0` | Reserved. |
| `[20]` | `TTA` | Trace trap; `RES0` on the Cortex-A72. |
| `[19:14]` | `RES0` | Reserved. |
| `[13:12]` | `RES1` | Reserved, read-as-one. |
| `[11]` | `RES0` | Reserved. |
| `[10]` | `TFP` | Traps FP/Advanced SIMD instructions. |
| `[9:0]` | `RES1` | Reserved, read-as-one. |

With `E2H == 0`, the `TFP` meanings are:

- `TFP == 0`: FP/Advanced SIMD instructions are not trapped by this control.
- `TFP == 1`: FP/Advanced SIMD instructions executed at EL2, EL1, and EL0 are
  trapped to EL2 (reported with `ESR_ELx.EC == 0x07`).

Access:

- `CPTR_EL2` is accessible at EL2 and EL3.
- It is not accessible at EL0 or EL1.

Policy:

- Print the complete raw value and `TFP`.
- The diagnostic must remain integer-only: inherited `TFP` may be `1`, in which
  case accidental FP/SIMD use at EL2 would itself trap. Do not execute
  floating-point or SIMD instructions as part of the diagnostic.
- Do not modify trap controls.

### 6.11 `CNTHCTL_EL2` and `CNTVOFF_EL2`

`CNTHCTL_EL2` controls timer/counter access from EL0 and EL1. On the Cortex-A72,
`HCR_EL2.E2H` is effectively `0`, so the baseline Armv8.0 register layout
applies. The field layout is:

| Bits | Field | Meaning |
| --- | --- | --- |
| `0` | `EL1PCTEN` | When `0`, traps EL0 and EL1 physical-counter accesses to EL2. |
| `1` | `EL1PCEN` | When `0`, traps EL0 and EL1 physical-timer accesses to EL2. |
| `2` | `EVNTEN` | Enables the event stream from `CNTPCT_EL0`. |
| `3` | `EVNTDIR` | Event-stream trigger transition direction. |
| `[7:4]` | `EVNTI` | Event-stream trigger bit select. |
| `[11:8]` | `RES0` | Reserved. |
| `[63:12]` | `RES0` | Reserved (feature-dependent in later revisions). |

`EL1PCTEN` / `EL1PCEN` meanings (despite the `EL1` name, they cover EL0 too):

- `0`: applicable EL0 and EL1 physical-counter/timer accesses are trapped to
  EL2 when EL2 is enabled. For EL0, `CNTKCTL_EL1.EL0PCTEN` / `EL0PTEN` can
  instead route the trap to EL1.
- `1`: this `CNTHCTL_EL2` control does not trap to EL2. This does not by itself
  guarantee EL0 access, because `CNTKCTL_EL1` still controls EL0 separately.

`CNTVOFF_EL2` is the 64-bit virtual counter offset; it holds no sub-fields and
is printed as a raw value. The virtual count seen below EL2 is computed as the
physical count minus `CNTVOFF_EL2` (modulo `2^64`).

Access:

- Both registers are accessible at EL2 and EL3, and are not accessible at EL0
  or EL1.

Policy:

- Print both raw values and the two access-control bits.
- Do not read a changing counter into the stable-state comparison; `CNTHCTL_EL2`
  and `CNTVOFF_EL2` are control/offset values, not the counter itself.
- Do not change timer access or the virtual offset.

### 6.12 `ID_AA64PFR0_EL1`

Relevant feature fields include:

| Bits | Field | Purpose |
| --- | --- | --- |
| `[11:8]` | `EL2` | EL2 implementation and execution-state support |
| `[19:16]` | `FP` | Floating-point support |
| `[23:20]` | `AdvSIMD` | Advanced SIMD support |
| `[27:24]` | `GIC` | System-register GIC interface support |

Policy:

- Print the raw value and these fields.
- An `EL2` field indicating no EL2 is inconsistent with executing at EL2;
  append `UnsupportedEL2Feature` and return one.
- Other fields are capability observations.

### 6.13 `ID_AA64MMFR0_EL1`

Relevant memory-model fields include:

| Bits | Field | Purpose |
| --- | --- | --- |
| `[3:0]` | `PARange` | Implemented physical-address range |
| `[7:4]` | `ASIDBits` | Supported ASID width |
| `[23:20]` | `TGran16` | 16 KiB translation-granule support |
| `[27:24]` | `TGran64` | 64 KiB translation-granule support |
| `[31:28]` | `TGran4` | 4 KiB translation-granule support |

Policy:

- Print the raw value and these fields.
- Do not yet reject a granule encoding. The permanent MMU design will select a
  granule and then turn the relevant capability into a required invariant.
- Use `PARange` when designing address masks; do not assume all 64 address bits
  are implemented.

## 7. Error-stack allocation

Existing values retain their meaning:

| Value | Error |
| --- | --- |
| `0x01` | `InvalidEL2State` |
| `0x02` | `WaitUartIoComplete` |
| `0x03` | `UartTxTimeout` |

Reserve the following values for the handoff diagnostic:

| Value | Error | Fatal |
| --- | --- | --- |
| `0x04` | `InvalidStackAlignment` | Yes |
| `0x05` | `UnexpectedEL2Endianness` | Yes |
| `0x06` | `InvalidVectorBaseAlignment` | Yes |
| `0x07` | `UnsupportedEL2Feature` | Yes |
| `0x08` | `HandoffStateChanged` | Yes |

An observation such as MMU enabled, caches enabled, `SPSel == 0`, stage 2
enabled, or interrupts unmasked is printed but does not receive an error code
until the permanent-handoff contract explicitly requires a value.

When multiple invariants fail, append every error in validation order rather
than returning after the first failure, except when `CurrentEL != EL2`. EL2-only
register reads are unsafe at a lower exception level, so that case terminates
the remaining checks.

## 8. Stable-state comparison

The diagnostic promises not to modify inherited EL2 state. After UART
reporting, capture a second snapshot and compare:

- `CurrentEL`;
- `MPIDR_EL1`;
- `SPSel`;
- `SCTLR_EL2`;
- `HCR_EL2`;
- `TCR_EL2`;
- `TTBR0_EL2`;
- `MAIR_EL2`;
- `VTCR_EL2`;
- `VTTBR_EL2`;
- `VBAR_EL2`;
- `DAIF`;
- `CPTR_EL2`;
- `CNTHCTL_EL2`;
- `CNTVOFF_EL2`;
- both feature-identification registers.

Do not compare SP: ordinary Rust calls and formatting legitimately change SP
temporarily, and capture can occur at different stack depths. Check only its
alignment. Do not include running timer/counter registers.

If a stable field differs, append one `HandoffStateChanged` error and print the
name, before value, and after value if UART remains operational. The comparison
must not attempt to restore a changed register during this phase.

## 9. UART report format

Print fixed-width hexadecimal raw values so captures can be compared between
U-Boot or firmware versions:

```text
tvisor handoff diagnostic
  CurrentEL       0x0000000000000008  EL=2
  MPIDR_EL1       0x0000000080000000  Aff=0.0.0.0
  SPSel           0x0000000000000001  SP_EL2
  SP              0x0000000037b3ac90  align=0
  SCTLR_EL2       0x................  M=. C=. I=. A=. SA=. EE=. WXN=.
  HCR_EL2         0x................  VM=. RW=. FMO=. IMO=. AMO=. TWI=. TWE=. TGE=.
  TCR_EL2         0x................  active=<yes|no>
  TTBR0_EL2       0x................  active=<yes|no>
  MAIR_EL2        0x................  Attr0=.. Attr1=.. ... Attr7=..
  VTCR_EL2        0x................  active=<yes|no> T0SZ=. IPA_BITS=. SL0=. IRGN0=. ORGN0=. SH0=. TG0=. PS=. VS=. DS=.
  VTTBR_EL2       0x................  active=<yes|no> VMID=.... BADDR=.. CnP=.
  VBAR_EL2        0x................  align=...
  DAIF            0x................  D=. A=. I=. F=.
  CPTR_EL2        0x................  TFP=.
  CNTHCTL_EL2     0x................  EL1PCTEN=. EL1PCEN=.
  CNTVOFF_EL2     0x................
  ID_AA64PFR0_EL1 0x................  EL2=. FP=. AdvSIMD=. GIC=.
  ID_AA64MMFR0_EL1 0x...............  PARange=. ASIDBits=. TG4=. TG16=. TG64=.
  fatal_errors    <count>
  state_unchanged <yes|no>
```

Formatting must not allocate. Existing `core::fmt` UART output is sufficient.
If a transmit timeout occurs, `UartTxTimeout` remains available in the memory
error stack even though the text report is incomplete.

## 10. Return policy

Return zero only when:

- execution is at EL2;
- SP is observed 16-byte aligned;
- `SCTLR_EL2.EE == 0`;
- `VBAR_EL2` is 2048-byte aligned;
- the processor reports EL2 support;
- no stable inherited register changed.

UART failures remain recorded in the error stack. A UART drain failure occurs
during finalization, so the implementation must decide the return code after
`debug_fini()` or have `debug_fini()` return a result. The design treats UART
TX and drain failures as diagnostic failures and therefore expects a nonzero
return status.

## 11. Validation on Raspberry Pi 4

Build-time checks:

```text
cargo check
cargo clippy --bin tvisor --lib -- -D warnings
cargo fmt --all -- --check
```

Board procedure:

1. Clear or note the current U-Boot console.
2. Load the ELF at `0x0200_0000` and execute it with `bootelf`.
3. Save the complete UART report.
4. Confirm tvisor returns to a working U-Boot prompt.
5. Inspect the error stack:

   ```text
   U-Boot> md.b 0x00001000 0x100
   ```

6. Confirm a normal run begins with `00` and reports `state_unchanged yes`.
7. Repeat after U-Boot, firmware, device-tree, or boot-configuration updates
   and compare the raw register values.

Negative checks should be performed only through safe test hooks, not by
corrupting live EL2 state. Unit-test pure decoding and validation functions on
the host where practical. Hardware-only register reads remain covered by board
testing.

## 12. Output of this phase

After the diagnostic is reviewed on the real board, record:

- the U-Boot version and Raspberry Pi firmware version;
- the complete raw UART report;
- which inherited values are stable across several boots;
- which values change after firmware or U-Boot updates;
- the accepted entry invariants for permanent takeover;
- the normalization required for stack, vectors, MMU, caches, stage 2,
  interrupts, timers, and UART.

Only then should tvisor design the assembly entry point and the one-way
transition away from U-Boot.

## 13. References

- [UART support design for Raspberry Pi 4](uart_rpi4_design.md)
- [Peripheral address translation on Raspberry Pi 4](peripheral_address_translation.md)
- [U-Boot memory layout on Raspberry Pi 4](uboot_rpi4_memory.md)
- [Arm A-profile AArch64 register reference](https://developer.arm.com/documentation/ddi0601/latest)
- [Arm memory-management guide](https://developer.arm.com/-/media/Arm%20Developer%20Community/PDF/LearnTheArchitecture-MemoryManagement-101811_0100_00_en.pdf)
- [Arm exception-model guide](https://developer.arm.com/-/media/Arm%20Developer%20Community/PDF/Learn%20the%20Architecture/Exception%20model.pdf)
