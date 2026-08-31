# Phase 9 Design and Implementation Questions

## 1. Context and Goals

Phase 9 establishes the foundations for guest virtualization on the boot physical CPU (single vCPU) before booting a full guest OS in Phase 10. The core deliverables are:

1. **Guest IPA layout and Stage-2 address translation** (`VTCR_EL2`, `VTTBR_EL2`, Stage-2 page tables).
2. **Single-vCPU execution context and world-switch mechanism** (`__vcpu_run`, `eret`, context save/restore).
3. **Stage-2 exception handling and trap decoding** (`ESR_EL2`, `FAR_EL2`, `HPFAR_EL2`, syndrome parsing).
4. **Guest Device Tree (DTB) generator** (emitting the guest-visible CPU and
   RAM IPA ranges needed by the Phase 9 platform).
5. **Controlled EL1 test payload** (exercising normal execution, traps, and deliberate stage-2 translation faults).

Below are the key architectural and design questions requiring alignment before starting implementation.

---

## 2. Guest IPA Memory Layout

### Question 2.1: What base address and size should be used for guest IPA RAM?

* **Option A (Recommended — QEMU virt style)**:
  * Guest RAM Base: `0x4000_0000` (1 GiB offset).
  * Low IPA window `[0x0000_0000, 0x4000_0000)` (1 GiB) reserved for virtual devices (e.g. virtual GIC at `0x0800_0000`, emulated PL011 UART at `0x0900_0000`, VirtIO at `0x0A00_0000`).
  * *Rationale*: Follows the familiar QEMU `virt` convention and leaves a
    stable low-IPA window for future virtual devices. Linux itself discovers
    the layout from the guest DTB and does not require this address.
* **Option B (Zero-based RAM)**:
  * Guest RAM Base: `0x0000_0000`.
  * MMIO placed at high IPA (e.g., above RAM).
  * *Rationale*: Simpler arithmetic, but non-standard for ARM64 virtual machines.


**Answer:** Implement Option A. Define `GUEST_RAM_BASE` as `0x4000_0000` and
keep the guest RAM size configurable. The controlled Phase 9 payload needs only
a small initial allocation, such as 2 MiB; that size is a test configuration,
not a permanent limit for Linux guests. Reserve the low IPA window in the
layout, but do not describe or map virtual devices that Phase 9 has not
implemented.

### Question 2.2: What IPA address space width (`VTCR_EL2.T0SZ`) should be configured?

* **Option A (40-bit IPA)**:
  * `T0SZ = 24` (40-bit IPA, 1 TiB space), 4 KiB translation granule (`TG0 = 0b00`), Level 1 walk start (`SL0 = 1`).
  * *Rationale*: A Level 1 start requires two concatenated 4 KiB L1 tables so
    that their combined 1,024 entries consume IPA bits `[39:30]`.
* **Option B (Recommended — 39-bit IPA)**:
  * `T0SZ = 25` (39-bit IPA, 512 GiB space), 4 KiB translation granule, Level 1 walk start with one ordinary 4 KiB L1 table.


**Answer:** Implement Option B. A 39-bit IPA is ample for tvisor and naturally
fits one three-level walk:

```text
IPA[38:30]  L1 index     9 bits
IPA[29:21]  L2 index     9 bits
IPA[20:12]  L3 index     9 bits
IPA[11:0]   page offset 12 bits
```

Use `T0SZ=25`, `SL0=0b01`, and `TG0=0b00`. Derive `VTCR_EL2.PS` independently
from the processor's supported physical-address range. This avoids the two
contiguous, 8 KiB-aligned L1 pages required by the 40-bit, Level 1-start
alternative. A 40-bit IPA could instead use an L0-to-L3 four-level walk, but
neither form provides a useful benefit for the Raspberry Pi 4 guests planned
here.


---

## 3. Controlled EL1 Test Payload

### Question 3.1: How should the Phase 9 test payload be packaged and delivered?

* **Option A (Recommended — Embedded binary in tvisor)**:
  * Compile a minimal AArch64 bare-metal test routine directly inside the `tvisor` binary (e.g. in a `.payload` section or static byte slice `TEST_PAYLOAD_BIN`).
  * At boot, tvisor allocates guest RAM pages via `GlobalPageAllocator`, copies the payload bytes into the assigned physical pages, performs data-cache clean to PoC / instruction-cache invalidate, and maps them to guest IPA.
  * *Rationale*: Self-contained, host-testable, and does not require complex U-Boot TFTP/multi-binary load scripts on the Raspberry Pi 4 hardware during early stage-2 bringup.
* **Option B (Separate U-Boot loaded payload)**:
  * Load a separate ELF or binary via U-Boot (e.g. via an extra `payload=<addr>` boot argument).
  * *Rationale*: Decouples payload compilation from tvisor, but adds operational overhead to hardware testing.

**Answer:** Implement Option A as a small, position-independent AArch64
assembly payload in a dedicated linker section with start/end symbols. It must
not refer directly to tvisor symbols because it executes after being copied to
a different address. At runtime, allocate guest backing pages, copy the
payload, perform the required data-cache clean and instruction-cache
invalidation, and map its entry IPA executable through stage 2. This keeps the
hardware procedure to one tvisor image and one U-Boot launch.

### Question 3.2: What specific test sequences should the Phase 9 payload execute?

* Proposed test sequence:
  1. **Checkpoint 1 (Normal EL1 Execution)**: Write and verify memory patterns in assigned guest RAM.
  2. **Checkpoint 2 (System Register Access)**: Read EL1 registers (`CurrentEL`, `MPIDR_EL1`, `SCTLR_EL1`) and verify proper virtualization.
  3. **Checkpoint 3 (Deliberate Hypercall / Trap)**: Issue `HVC #0` or `BRK` to verify EL2 trap capture, argument passing, and clean resumption back to EL1.
  4. **Checkpoint 4 (Deliberate Stage-2 Fault)**: Attempt to read/write an unmapped guest IPA (e.g. `0x3000_0000`), verifying that tvisor's EL2 exception handler catches the Stage-2 Data Abort, decodes `ESR_EL2` / `HPFAR_EL2` / `FAR_EL2`, and reports the syndrome accurately.

**Answer:** Use the proposed sequence with these constraints:

1. Test normal reads and writes only in assigned guest RAM.
2. Confirm `CurrentEL == EL1`, and read `MPIDR_EL1` and `SCTLR_EL1`. Initialize
   `VMPIDR_EL2` deliberately for vCPU 0 rather than accidentally exposing an
   unreviewed physical affinity value.
3. Use `HVC #0`, not `BRK`, for resumable checkpoints. Put a checkpoint number
   in `x0`; EL2 must save the guest context, diagnose the HVC, and resume at the
   instruction after it. At least two HVC checkpoints should prove that resume
   works.
4. Make the unmapped load/store the final payload operation. EL2 must diagnose
   the stage-2 fault and finish the test; Phase 9 does not need to skip the
   faulting instruction or resume from this terminal fault.

The payload does not access a UART. Tvisor reports checkpoints through its own
physical Mini UART; guest UART emulation begins in Phase 10.

---

## 4. Stage-2 Page Table Management

### Question 4.1: How should Stage-2 page tables be allocated?

* **Option A (Recommended — Direct allocation from `GlobalPageAllocator`)**:
  * Allocate 4 KiB pages on-demand from the Phase 8 physical-page allocator whenever a new table level is created.
  * *Rationale*: Reuses the validated Phase 8 allocator directly.
* **Option B (Pre-allocated static/bootstrap arena)**:
  * Carve out a fixed memory arena at initialization dedicated exclusively to stage-2 tables.

**Answer:** Implement Option A. Allocate each stage-2 table page from the
global allocator and keep it allocated for the guest's lifetime. The stage-2
builder must either validate all inputs before allocation or release pages
allocated by a failed partial construction. With the selected 39-bit IPA, the
single L1 root and all lower-level tables are ordinary individually allocated
4 KiB pages.

### Question 4.2: Should Stage-2 support 2 MiB / 1 GiB block mappings in addition to 4 KiB pages?

* **Option A (Pages and 2 MiB block descriptors)**:
  * Implement 4 KiB leaf pages for fine-grained mappings (e.g. payload, DTB, device MMIO) and 2 MiB block descriptors for contiguous guest RAM chunks.
* **Option B (Recommended — 4 KiB pages only initially)**:
  * Implement strictly 4 KiB pages in Phase 9, deferring block descriptors to Phase 10.

**Answer:** Implement Option B. Four-KiB leaves are sufficient for the small
controlled payload and allow guest IPA pages to have discontiguous physical
backing. Defer 2 MiB and 1 GiB block descriptors until measurements or larger
guest mappings justify their alignment, contiguity, splitting, and attribute
complexity.

---

## 5. vCPU Execution Context and World Switch

### Question 5.1: Structure and scope of `VcpuContext`

* What registers should be captured in `VcpuContext` for Phase 9?
  * **General Purpose**: `x0`–`x30`, `SP_EL1` (`SP_EL0` if needed).
  * **Exception State**: `ELR_EL2` (guest PC), `SPSR_EL2` (guest PSTATE: initial EL1h mode `0x3c5`).
  * **EL1 System Registers**: `SCTLR_EL1`, `CPACR_EL1`, `TTBR0_EL1`, `TTBR1_EL1`, `TCR_EL1`, `MAIR_EL1`, `VBAR_EL1`, `CONTEXTIDR_EL1`, `FAR_EL1`, `ESR_EL1`.
  * **Control/Syndrome**: Exit reason, `ESR_EL2`, `FAR_EL2`, `HPFAR_EL2`.
* Is floating point / NEON (`CPTR_EL2.TFP = 1` trapping) deferred until full Linux guest support in Phase 10? *(Recommended: Yes, trap on FP/SIMD access in Phase 9)*.

**Answer:** Keep persistent guest state and exit information separate.

`VcpuContext` contains:

- `x0`-`x30`, `SP_EL0`, and `SP_EL1`;
- guest PC and PSTATE values loaded through `ELR_EL2` and `SPSR_EL2`;
- `SCTLR_EL1`, `CPACR_EL1`, `TTBR0_EL1`, `TTBR1_EL1`, `TCR_EL1`,
  `MAIR_EL1`, `VBAR_EL1`, and `CONTEXTIDR_EL1`.

A separate `VcpuExit` contains the vector number, exit reason, `ESR_EL2`,
`FAR_EL2`, and `HPFAR_EL2`. These registers describe why EL2 regained control
and are not guest EL1 register state. Initialize the first entry with
`SPSR_EL2=0x3c5`, selecting EL1h with asynchronous exceptions masked. Defer
FP/SIMD state: set `CPTR_EL2.TFP` and diagnose an FP/SIMD access as an
unsupported Phase 9 exit.

### Question 5.2: World switch assembly entry point

* Proposed signature:
  ```rust
  unsafe extern "C" fn __vcpu_run(context: *mut VcpuContext) -> VcpuExitReason;
  ```
* The function saves host callee-saved registers (`x19`–`x29`, `x30`, `sp`), switches `VTTBR_EL2` to the guest's VMID/table, loads guest registers from `context`, and executes `eret`.
* On exception from Lower EL (vector slots 8–11), the shared handler saves guest state into `context`, restores host callee-saved state, and returns to Rust with the exit code.

**Answer:** Use this model, but return a primitive FFI-safe value such as
`u64` from assembly and convert it to a Rust exit enum afterward. Reuse and
extend the existing EL2 vector table rather than installing a second VBAR.
The lower-EL AArch64 synchronous slot must capture every guest GPR before Rust
can clobber it and must also capture `HPFAR_EL2`. It then transfers control to
a vCPU-exit trampoline that restores the saved host callee-saved registers and
EL2 stack before returning from `__vcpu_run`.

Only lower-EL AArch64 synchronous exits are supported in Phase 9. Lower-EL IRQ,
FIQ, and SError entries remain fatal. An HVC resumes normally without manually
advancing `ELR_EL2`; the deliberate final stage-2 fault is reported without
resuming the guest.

---

## 6. Guest Device Tree (DTB) Generation

### Question 6.1: How should the guest DTB generator be designed?

* **Option A (Recommended — Minimal project-owned DTB serializer in `tvisor_util`)**:
  * Build a lightweight FDT emitter in `tvisor_util/fdt.rs` (or `guest_fdt.rs`) with no external dependencies that constructs:
    * `/` (root with `#address-cells = <2>`, `#size-cells = <2>`, model, compatible)
    * `/chosen` (bootargs if needed; no `stdout-path` before a guest console exists)
    * `/cpus` (single cpu@0 with compatible `arm,cortex-a72`)
    * `/memory@<base>` (guest RAM range)
  * Validated against the Phase 0 FDT parser on the host.
* **Option B (Hard-coded pre-compiled static DTB blob)**:
  * Compile a `.dts` to `.dtb` using `dtc` at build time and embed it as raw bytes.
  * *Tradeoff*: Less flexible when RAM size or device addresses change dynamically.

**Answer:** Implement Option A in a separate `tvisor_util/guest_fdt.rs` module,
not in the host-DTB parser. The builder uses a caller-provided fixed-capacity
buffer, writes the FDT's big-endian structures with checked offsets and
alignment, and reports capacity or format errors without requiring a heap.

The Phase 9 DTB describes only the root, one CPU, the assigned guest RAM, and a
minimal `/chosen` node if boot arguments are useful. Do not add `stdout-path`,
PL011, GIC, VirtIO, or any host physical device node before the corresponding
virtual device exists. Map the generated DTB into guest IPA space and verify on
the host that the existing FDT parser reads exactly the CPU and RAM ranges that
the stage-2 mapper exposes.

---

## 7. Proposed Phase 9 Implementation Roadmap

1. **Step 1 — Design Documentation**: Write `docs/guest_memory_map.md` capturing agreed layouts and register configurations.
2. **Step 2 — Stage-2 Translation Engine**: Implement Stage-2 descriptor encodings, table allocation, and host tests in `tvisor_util/stage2_translation.rs`.
3. **Step 3 — Guest DTB Builder**: Implement guest FDT writer in `tvisor_util/guest_fdt.rs` and verify roundtrip parsing with `Fdt`.
4. **Step 4 — vCPU Context & World Switch**: Implement `VcpuContext` and assembly `__vcpu_run` in `src/guest.rs` / `src/vcpu.rs`.
5. **Step 5 — Lower-EL Exception Handling**: Extend `src/exception.rs` to handle Lower-EL synchronous aborts, `HVC` calls, and stage-2 syndrome decoding.
6. **Step 6 — Test Payload & Hardware Verification**: Run the controlled EL1 payload on Raspberry Pi 4 hardware, verifying normal execution, traps, and diagnosed stage-2 aborts.
