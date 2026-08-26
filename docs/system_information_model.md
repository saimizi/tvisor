# Tvisor system-information model design

## 1. Purpose

This document defines the Phase 1 system-information model described in
`docs/development_plan.md`. The model is the owned boundary between platform
discovery and the later memory-map and execution-environment code.

Phase 0 provides a validated, borrowed view of the U-Boot working DTB. Later
phases will decode that view and other live inputs into `SystemInfo`. Once all
required information has been copied, `SystemInfo` must remain valid without
accessing the original DTB.

Phase 1 defines and tests the data model only. It does not yet populate the
model from the DTB, choose allocatable memory, reclaim U-Boot memory, or change
any EL2 registers.

## 2. Requirements

The model must:

- work in `no_std` without a heap;
- own every stored value and have no lifetime tied to the FDT parser;
- store CPU physical addresses as checked 64-bit values;
- use half-open address ranges `[start, end)`;
- preserve the source and meaning of reservations;
- distinguish described RAM from RAM later proven usable;
- report fixed-capacity exhaustion instead of dropping entries;
- support deterministic iteration and allocation-free formatting;
- contain no Raspberry Pi 4 or BCM2711 parsing policy; and
- be usable by host unit tests as well as EL2 code.

The model is a description of the host platform. It is not a guest physical
address map and must not be exposed directly to a guest VM.

## 3. Layer boundary

The intended ownership boundary is:

```text
U-Boot arguments and live DTB bytes
               |
               v
 borrowed FDT nodes/properties/iterators       tvisor_util/fdt.rs
               |
               v
 binding and platform interpretation           tvisor_util/platform.rs
               |
               v
 owned SystemInfo values                       tvisor_util/system_info.rs
               |
               v
 normalization and allocation policy           tvisor_util/memory_map.rs
```

`system_info.rs` must not import `dtoolkit`. In particular, its public types
must not contain `Fdt`, `Node`, `Property`, borrowed strings, or raw pointers.
The platform layer converts parser-specific values into the owned types.

The existing `ConsoleKind` and `ConsoleInfo` are platform facts rather than
FDT parser mechanics. During Phase 1 they should move from `fdt.rs` to
`system_info.rs`; `discover_console` can return the relocated type without
changing its behavior.

## 4. Address and range representation

### 4.1 Physical addresses

Use an explicit wrapper rather than `usize`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysAddr(u64);
```

The DT format can describe addresses wider than the current Rust pointer
width. Keeping addresses as `u64` also makes host tests independent of the
host architecture. Conversion to `usize` is allowed only at an MMIO or memory
access boundary and must use `usize::try_from`.

`PhysAddr` should provide a value accessor and checked addition. It must not
provide implicit pointer conversion.

### 4.2 Half-open regions

All address intervals use `[start, end)`. The core region type is conceptually:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysRegion {
    start: PhysAddr,
    size: u64,
}
```

Construction validates that:

- `size` is non-zero;
- `start + size` does not overflow `u64`; and
- a constructor accepting `start` and `end` rejects `end <= start`.

The type should expose:

- `start()`, `size()`, and checked `end()`;
- `contains_address(address)`;
- `contains_region(other)`;
- `overlaps(other)`; and
- `is_adjacent(other)`.

Adjacency is not overlap. For example, `[0x1000, 0x2000)` and
`[0x2000, 0x3000)` are adjacent but do not overlap.

Page alignment is deliberately not an invariant of `PhysRegion`. DT regions
and device registers can be unaligned or smaller than a page. Code that builds
page tables or allocates pages must apply its own checked alignment policy.

Suggested construction errors are:

```rust
pub enum RegionError {
    Empty,
    EndBeforeStart,
    AddressOverflow,
}
```

## 5. Fixed-capacity collection

Early discovery cannot use `Vec`. Define a reusable collection:

```rust
pub struct FixedList<T, const N: usize> {
    entries: [Option<T>; N],
    len: usize,
}
```

Phase 1 records are all `Copy`, which permits a simple, safe `Option<T>`
implementation without `MaybeUninit` or manually managed drop state.

The collection should provide:

- `const fn new()`;
- `len()`, `capacity()`, `is_empty()`, and `is_full()`;
- `push(value) -> Result<(), CapacityError>`;
- immutable indexed access; and
- an iterator over exactly the initialized prefix.

The list preserves insertion order. It does not sort, merge, deduplicate, or
reject overlapping regions. Raw platform descriptions can legitimately
overlap, and normalization belongs to Phase 4.

Capacity exhaustion is fatal to the current discovery operation. A caller
must propagate `CapacityError` and must not continue with an incomplete
database.

The implementation should reject `N == 0` at construction or support it
consistently in tests. It should not expose the internal `Option` array.

## 6. Region records

### 6.1 RAM

```rust
pub struct RamRegion {
    pub region: PhysRegion,
    pub source: RamSource,
}

pub enum RamSource {
    DeviceTree,
    Firmware,
    FirmwareCarveout,
}
```

`ram` describes the physical RAM layout, including firmware-owned carve-outs
that explain gaps in the CPU-visible banks. `DeviceTree` and `Firmware`
describe physical RAM learned from those sources. `FirmwareCarveout` marks an
interval backed by installed RAM but withheld by firmware; it is never an
allocation candidate. No `ram` entry by itself means that tvisor may allocate
the interval. Reservations are not subtracted in Phase 1.

### 6.2 Reservations

A fixed reservation has a known physical interval:

```rust
pub struct ReservedRegion {
    pub region: PhysRegion,
    pub origin: ReservationOrigin,
    pub owner: ReservationOwner,
    pub attributes: ReservationAttributes,
}
```

The initial origins should include:

```rust
pub enum ReservationOrigin {
    FdtReservationBlock,
    ReservedMemoryNode,
    Firmware,
    Device,
    Bootloader,
    Dtb,
    TvisorImage,
    LinuxPolicy,
    Unknown,
}
```

Origin answers where a reservation was learned or why it exists. Owner
answers which component currently controls its lifetime:

```rust
pub enum ReservationOwner {
    Firmware,
    Device,
    Bootloader,
    Tvisor,
    HostPolicy,
    Unknown,
}
```

Keeping these concepts separate prevents an FDT reservation-table entry, for
example, from being incorrectly treated as owned by the DTB itself.

DT `/reserved-memory` flags are represented without storing property names:

```rust
pub struct ReservationAttributes {
    pub no_map: bool,
    pub reusable: bool,
}
```

Unknown reservation semantics must map to conservative values and an
`Unknown` origin or owner. Reclaimability must never be inferred merely from
an absent flag.

On BCM2711, firmware can omit its VideoCore carve-out from `/memory` rather
than describing it in `/reserved-memory`. The platform layer adds the omitted
interval below the 1 GiB boundary as a `RamRegion` whose source is
`FirmwareCarveout`. It is not added to `reserved`, because `reserved` tracks
explicit regions handled by tvisor's reservation policy. This inference is
narrowly conditioned on the BCM2711 root `compatible`; the generic discovery
layer must not classify arbitrary gaps between RAM banks this way.

Dynamic `/reserved-memory` requests that contain `size` but no resolved `reg`
do not describe a physical interval. Store them separately rather than
inventing an address or placing an empty `ReservedRegion`:

```rust
pub struct DynamicReservation {
    pub size: u64,
    pub alignment: Option<u64>,
    pub origin: ReservationOrigin,
    pub owner: ReservationOwner,
    pub attributes: ReservationAttributes,
    pub alloc_ranges: FixedList<PhysRegion, MAX_DYNAMIC_ALLOC_RANGES>,
}
```

Allocation ranges are stored in another fixed-capacity list. A numeric source
identifier can be added later if required by the observed DTB.

### 6.3 MMIO

```rust
pub struct MmioRegion {
    pub region: PhysRegion,
    pub kind: MmioKind,
}

pub enum MmioKind {
    Console,
    InterruptController,
    Timer,
    BusWindow,
    Device,
    Unknown,
}
```

Bus translations retain both sides of a decoded `ranges` tuple for diagnostics:

```rust
pub struct BusTranslation {
    child: PhysRegion,
    parent: PhysRegion,
}
```

The child region is in the bus address space and the parent region is in the
CPU physical address space. Only the parent range is classified as MMIO.

Every MMIO range stored in `SystemInfo` is a CPU physical range after all DT
bus translations. A DT unit address or legacy peripheral bus address must not
be stored here before translation.

Overlapping device windows and parent bus windows are allowed in the raw
database. They carry different descriptive granularity and are evaluated by
later policy.

### 6.4 CPUs

```rust
pub struct CpuInfo {
    pub affinity: u64,
    pub status: CpuStatus,
    pub enable_method: CpuEnableMethod,
    pub is_current: bool,
}

pub enum CpuStatus {
    Enabled,
    Disabled,
}

pub enum CpuEnableMethod {
    AlreadyRunning,
    SpinTable { release_address: PhysAddr },
    Psci,
    Unknown,
}
```

`affinity` stores the architectural affinity value derived from the CPU
node's `reg` property, not a sequential array index. Phase 3 will compare it
with the affinity fields in `MPIDR_EL1` to identify the executing CPU.

No CPU topology, logical-ID assignment, or secondary-core startup policy is
performed in Phase 1.

### 6.5 Console

```rust
pub enum ConsoleKind {
    MiniUart,
}

pub struct ConsoleInfo {
    pub kind: ConsoleKind,
    pub registers: PhysRegion,
}
```

The register range is a CPU physical MMIO range. UART line options such as
baud rate may be added later as owned numeric fields if tvisor starts
configuring the UART rather than inheriting U-Boot's setup.

The console range may also appear in `mmio`. `console` identifies the active
debug device, while the MMIO entry participates in memory-map validation.
This is intentional semantic cross-reference, not accidental duplication.

## 7. SystemInfo structure

The initial owned database is:

```rust
pub struct SystemInfo {
    pub ram: FixedList<RamRegion, MAX_RAM_REGIONS>,
    pub reserved: FixedList<ReservedRegion, MAX_RESERVED_REGIONS>,
    pub dynamic_reserved:
        FixedList<DynamicReservation, MAX_DYNAMIC_RESERVATIONS>,
    pub mmio: FixedList<MmioRegion, MAX_MMIO_REGIONS>,
    pub bus_translations: FixedList<BusTranslation, MAX_BUS_TRANSLATIONS>,
    pub cpus: FixedList<CpuInfo, MAX_CPUS>,
    pub console: Option<ConsoleInfo>,
}
```

The fields may initially have private storage with read-only accessors and
controlled insertion methods. This is preferable if it helps keep capacity
errors and local validation consistent.

Suggested initial capacities are:

```rust
pub const MAX_RAM_REGIONS: usize = 8;
pub const MAX_RESERVED_REGIONS: usize = 64;
pub const MAX_DYNAMIC_RESERVATIONS: usize = 16;
pub const MAX_DYNAMIC_ALLOC_RANGES: usize = 4;
pub const MAX_MMIO_REGIONS: usize = 64;
pub const MAX_BUS_TRANSLATIONS: usize = 16;
pub const MAX_CPUS: usize = 32;
```

These are implementation limits, not architectural limits. Before they are
committed, Phase 1 should count the entries in the captured Raspberry Pi 4 DTB
and leave documented headroom. Later discovery must fail visibly if a limit
is exceeded. Increasing a capacity should require no parser or policy change.

`SystemInfo::new()` creates an empty database. Phase 1 should not define a
global mutable instance. The boot sequence should eventually construct one in
tvisor-owned storage and pass a shared reference to later initialization
steps. The lifetime and publication mechanism can be selected when Phase 2
integrates discovery.

## 8. Invariants and validation boundaries

Phase 1 constructors enforce local invariants:

- every fixed physical region is non-empty and non-overflowing;
- every address is a CPU physical address;
- dynamic reservation sizes and alignments are non-zero;
- a supplied alignment is a power of two; and
- collection capacity failures are explicit.

Phase 1 does not enforce global map invariants. In particular, it does not:

- require RAM regions to be sorted or disjoint;
- reject a reservation outside described RAM;
- reject overlapping reservations;
- subtract reservations from RAM;
- decide that reusable memory is allocatable;
- require all MMIO to be outside RAM; or
- assign logical CPU identifiers.

Phase 4 must exclude every `RamSource::FirmwareCarveout` entry before deriving
usable RAM; it must not treat the entry as ordinary RAM and rely on reservation
subtraction.

Those decisions require a complete set of platform inputs and belong to the
normalization and policy phases. Retaining raw records and provenance makes
conflicts diagnosable instead of hiding them during parsing.

## 9. Formatting and diagnostics

All core value and record types should derive `Debug`, `Clone`, `Copy`,
`PartialEq`, and `Eq` where appropriate. Implement `Display` for
`PhysAddr`, `PhysRegion`, and capacity/construction errors so hardware
diagnostics can use `core::fmt` without allocation.

Use a stable hexadecimal format for physical ranges, for example:

```text
[0x0000000004000000, 0x0000000004200000)
```

Formatting must not be used as serialization. Tests should primarily compare
typed values, with a small number of exact formatting tests to keep UART map
dumps readable.

## 10. Error handling

Errors should identify the failed operation without embedding borrowed parser
data. The initial model-level errors are:

- invalid or overflowing region construction;
- invalid dynamic-reservation size or alignment; and
- fixed-list capacity exhaustion.

Phase 2 will define discovery errors that wrap these errors and identify the
kind of list that overflowed. A single generic `OutOfMemory` error would be
misleading because capacity exhaustion is a compile-time database limit, not
a heap allocation failure.

No API may silently clamp an address, truncate a size, discard an entry, or
replace an earlier record when full.

## 11. Phase 1 implementation sequence

1. Add `tvisor_util/system_info.rs` with `PhysAddr`, `PhysRegion`, their
   checked constructors, queries, and formatting.
2. Add and test `FixedList<T, N>` using safe initialized storage.
3. Add the RAM, reservation, dynamic-reservation, MMIO, CPU, and console
   records and enums.
4. Add `SystemInfo`, initial capacities, construction, accessors, and
   insertion methods.
5. Move `ConsoleKind` and `ConsoleInfo` from `fdt.rs` into the new module and
   adapt `discover_console` and `main.rs` without changing runtime behavior.
6. Export `system_info` from `tvisor_util/lib.rs`.
7. Run host tests, formatting checks, and the AArch64 build.

The steps should be implemented as one reviewable Phase 1 change. DTB-based
RAM and reservation discovery remains Phase 2.

## 12. Verification and acceptance criteria

### 12.1 Host tests

Tests must cover:

- valid region construction at low addresses and above 4 GiB;
- zero size, reversed bounds, and `u64` end overflow;
- boundary behavior of address and region containment;
- disjoint, adjacent, partially overlapping, and fully overlapping ranges;
- checked physical-address addition;
- empty, partially filled, and full fixed lists;
- insertion at exact capacity and explicit rejection of the next entry;
- deterministic iteration and indexed access;
- zero-capacity behavior, if supported;
- dynamic reservation size and alignment validation;
- construction and equality of each record kind; and
- stable address/range/error formatting.

Tests must not require an AArch64 host or a DTB fixture because this phase is
testing the owned representation, not discovery.

### 12.2 AArch64 build

The following must succeed without enabling an allocator:

```text
cargo fmt --all -- --check
cargo test-host
cargo build --target aarch64-unknown-none
```

`test-host` is the repository alias that overrides the default bare-metal
target with `x86_64-unknown-linux-gnu`. The target artifact must not gain a
dependency on `alloc` through the new module.

### 12.3 Raspberry Pi 4

Phase 1 intentionally introduces no new platform discovery or EL2 state
change. A smoke test should verify that both existing launch paths still:

- parse the same DTB;
- discover the Mini UART at the same CPU physical address;
- print the existing diagnostics; and
- return to U-Boot with status `0x0`.

No `SystemInfo` dump is required until Phase 2 begins populating the database.

## 13. Deferred decisions

The following items are explicitly outside Phase 1:

- parsing `/memory`, `/reserved-memory`, `/cpus`, or general device nodes;
- assigning dynamic reserved-memory addresses;
- copying or reclaiming the original DTB;
- describing U-Boot's live allocations when they are absent from the DTB;
- normalizing or subtracting physical ranges;
- selecting usable RAM or creating a physical allocator;
- defining tvisor's virtual address layout;
- building EL2 stage-1 or guest stage-2 page tables; and
- creating a guest-specific device tree.

These decisions depend on complete discovery and policy and remain in the
later phases of `docs/development_plan.md`.
