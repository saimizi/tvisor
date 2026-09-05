# Phase 9: Guest IPA memory map, stage-2 translation, and vCPU execution design

## 1. Status and scope

This document defines the Phase 9 guest physical environment, Stage-2 translation regime, and single-vCPU execution model for tvisor on the Raspberry Pi 4.

Phase 9 establishes:
- the guest Intermediate Physical Address (IPA) layout;
- the Stage-2 translation tables and descriptor encodings;
- `VTCR_EL2`, `VTTBR_EL2`, `HCR_EL2`, and `CPTR_EL2` register configurations;
- the `VcpuContext` data structure and assembly world-switch (`__vcpu_run`);
- Stage-2 exception decoding and exit handling in `src/exception.rs`;
- an allocation-free guest Device Tree (DTB) generator; and
- a controlled position-independent EL1 test payload.

Phase 9 runs exclusively on the boot physical CPU (Core 0) and creates exactly one guest vCPU (vCPU 0). Secondary physical CPUs remain parked.

---

## 2. Guest IPA memory layout

The guest address space uses the standard ARM64 virtual platform convention (QEMU `virt` style), separating low-IPA virtual device windows from guest RAM:

```text
Guest IPA Range                     Size        Purpose
0x0000_0000_0000 .. 0x0000_3FFF_FFFF 1 GiB       Reserved for virtual devices (GIC, PL011, VirtIO)
0x0000_4000_0000 .. 0x0000_401F_FFFF 2 MiB       Phase 9 Guest RAM Window (16 KiB sparse backed RAM)
0x0000_4020_0000 .. [configurable)   Variable    Extended Guest RAM Window (Phase 10 Linux kernel & initrd)
```

### 2.1 Low IPA device window `[0x0000_0000, 0x4000_0000)`
Reserved for future virtual devices:
- `0x0800_0000` – `0x0801_FFFF`: Future Virtual GICv2 distributor (`GICD`) and CPU interface (`GICC` backed by physical `GICV`).
- `0x0900_0000` – `0x0900_0FFF`: Future Emulated PL011 UART register page.
- In Phase 9, no virtual devices are populated in Stage 2. Any access to `[0x0000_0000, 0x4000_0000)` is unmapped and triggers a Stage-2 Data/Instruction Abort.

### 2.2 Phase 9 sparse guest RAM mappings (16 KiB backed within 2 MiB window)
Within the 2 MiB Phase 9 window `[0x4000_0000, 0x4020_0000)`, tvisor allocates and Stage-2 maps exactly 16 KiB (4 individual 4 KiB pages) for the controlled EL1 test payload with distinct permissions. Only these backed pages are advertised in the guest DTB `/memory` node:

| Guest IPA Range | Size | Purpose | Stage-2 Access | Stage-2 Exec | Advertised in DTB |
|---|---|---|---|---|---|
| `0x4000_0000 .. 0x4000_0FFF` | 4 KiB | Test payload code | Read-Only | Executable | **Yes** (Bank 0) |
| `0x4000_1000 .. 0x4000_1FFF` | 4 KiB | Scratch test data | Read/Write | Execute-Never | **Yes** (Bank 0) |
| `0x4000_2000 .. 0x4000_2FFF` | 4 KiB | Stack guard page | **Unmapped** | N/A | **No** (Unmapped hole) |
| `0x4000_3000 .. 0x4000_3FFF` | 4 KiB | Guest EL1 stack (`sp = 0x4000_4000`) | Read/Write | Execute-Never | **Yes** (Bank 1) |
| `0x4010_0000 .. 0x4010_0FFF` | 4 KiB | Generated Guest DTB | Read-Only | Execute-Never | **Yes** (Bank 2) |

All other addresses in `[0x4000_0000, 0x4020_0000)` (including the stack guard page at `0x4000_2000`) remain unmapped in Stage-2 and are excluded from the guest DTB.

All guest RAM backing pages and Stage-2 table pages are allocated individually on-demand from tvisor's [`GlobalPageAllocator`](../tvisor_util/page_allocator.rs) and tracked by `GuestResourceManager`.

---

## 3. Stage-2 translation regime

### 3.1 Address space width and granule
- **IPA Width**: 39 bits (`T0SZ = 25`, covering 512 GiB).
- **Translation Granule**: 4 KiB (`TG0 = 0b00`).
- **Starting Level**: Level 1 (`SL0 = 0b01`).
- **Lookup Levels**: 3 levels (L1 $\rightarrow$ L2 $\rightarrow$ L3 $\rightarrow$ 4 KiB page).
  ```text
  IPA[38:30]   IPA[29:21]   IPA[20:12]   IPA[11:0]
   L1 index     L2 index     L3 index    Page offset
    9 bits       9 bits       9 bits       12 bits
  ```
- **Root Table**: One standard 4 KiB page containing 512 64-bit descriptors. No multi-page concatenation is required for 39-bit Level-1 start.
- **Descriptors**: Phase 9 constructs strictly 4 KiB Level-3 leaf page descriptors. Block descriptors are deferred.

### 3.2 Stage-2 translation registers

| Register | Value / Bitfield | Meaning |
|---|---|---|
| `VTCR_EL2.T0SZ` | `25` (`0b011001`) | 39-bit IPA space ($2^{64-25} = 512\text{ GiB}$) |
| `VTCR_EL2.SL0` | `1` (`0b01`) | Starting lookup level is Level 1 |
| `VTCR_EL2.IRGN0` | `1` (`0b01`) | Normal memory, Inner Write-Back Read-Allocate Write-Allocate Cacheable |
| `VTCR_EL2.ORGN0` | `1` (`0b01`) | Normal memory, Outer Write-Back Read-Allocate Write-Allocate Cacheable |
| `VTCR_EL2.SH0` | `3` (`0b11`) | Inner Shareable |
| `VTCR_EL2.TG0` | `0` (`0b00`) | 4 KiB translation granule |
| `VTCR_EL2.PS` | From `ID_AA64MMFR0_EL1.PARange` | Physical address output size |
| `VTCR_EL2.RES1` | Bit 31 | Reserved, must be 1 |
| `VTTBR_EL2.VMID` | `1` (bits `[63:48]`) | Guest VMID 1 |
| `VTTBR_EL2.BADDR` | L1 root table PA (bits `[47:12]`) | 4 KiB-aligned physical address of L1 stage-2 table |
| `VTTBR_EL2.CnP` | `0` (bit 0) | Common not Private disabled |

### 3.3 Stage-2 descriptor encodings

#### Table descriptor (Level 1 and Level 2 pointing to next-level table):
```text
Bit 0       : Valid (1)
Bit 1       : Type (1 = Table)
Bits [47:12]: Next-level Table Physical Address (4 KiB aligned)
Bits [63:48]: 0
```

#### Leaf page descriptor (Level 3 mapping a 4 KiB page):
```text
Bit 0       : Valid (1)
Bit 1       : Type (1 = Page)
Bits [5:2]  : MemAttr[3:0]
              - 0b1111 : Normal Memory, Inner/Outer Write-Back Cacheable
              - 0b0001 : Device-nGnRE Memory
Bits [7:6]  : S2AP[1:0] (Stage-2 Access Permissions)
              - 0b00 : No access (fault)
              - 0b01 : Read-only
              - 0b10 : Write-only
              - 0b11 : Read/Write
Bits [9:8]  : SH[1:0] (Shareability)
              - 0b11 : Inner Shareable (for Normal Memory)
              - 0b00 : Non-Shareable (for Device Memory)
Bit 10      : AF (Access Flag, must be 1 to prevent AF fault)
Bits [47:12]: Output Physical Address (4 KiB aligned)
Bits [54:53]: XN[1:0] (Execute-Never)
              - 0b00 : Executable (EL1 and EL0)
              - 0b10 : Execute-Never (XN)
```

---

## 4. Hypervisor execution control (`HCR_EL2`, `CPTR_EL2`, `VMPIDR_EL2`)

For guest EL1 execution, tvisor configures:

* `HCR_EL2`:
  * `VM = 1` (bit 0): Enable Stage-2 translation.
  * `SWIO = 1` (bit 1): Set/Way Invalidation Override (turn set/way invalidation into clean+invalidate to prevent cache bypass).
  * `FMO = 0, IMO = 0, AMO = 0`: Physical interrupts masked during Phase 9.
  * `RW = 1` (bit 31): Execution state for EL1 is AArch64.
  * `TSC = 1` (bit 19): Trap `SMC` instructions to EL2.
  * `HCD = 0` (bit 29): `HVC` instruction is enabled.
* `CPTR_EL2`:
  * `TFP = 0` (bit 10) while tvisor runs at EL2, because Rust or compiler-generated
    code can use Floating Point / Advanced SIMD instructions.
  * The vCPU world switch sets `TFP = 1` immediately before `eret` into the guest
    and clears it at the start of the lower-EL exception entry, before returning
    to Rust. This traps guest FP/Advanced-SIMD use without trapping tvisor itself.
* `VMPIDR_EL2`:
  * Programmed with virtual affinity `0xC000_0000` (Bit 31 RES1, Bit 30 UP flag set, Affinity 0.0.0 for vCPU 0).

### 4.1 Publication and Activation Sequence
Before entering the guest:
1. Finish Stage-2 descriptor writes.
2. Execute `dsb ishst` to ensure table updates are visible to the page table walker.
3. Write `VTCR_EL2`, `VTTBR_EL2`, host-safe `CPTR_EL2` (`TFP = 0`), and
   `VMPIDR_EL2` while `HCR_EL2.VM` remains clear.
4. Execute `isb` so the new Stage-2 configuration and VMID are visible to subsequent instructions.
5. Invalidate guest TLBs with `tlbi vmalls12e1is` while `VTTBR_EL2` selects the guest VMID.
6. Execute `dsb ish` and `isb` to complete the invalidation.
7. Write `HCR_EL2` with `VM = 1`, then execute `isb` before launching `__vcpu_run`.

After the vCPU has stopped, tvisor invalidates the guest's translations while
`VTTBR_EL2` still selects its VMID, completes the invalidation with
`dsb ish; isb`, clears only `HCR_EL2.VM`, and then clears `VTTBR_EL2` before
releasing any guest or table pages. `CPTR_EL2` remains at a value containing
all architecturally required RES1 bits.

---

## 5. vCPU execution context and world switch

### 5.1 `VcpuContext`
Persistent guest EL1 state stored in tvisor memory:

```rust
#[repr(C, align(16))]
pub struct VcpuContext {
    // General purpose registers x0 through x30
    pub x: [u64; 31],
    // Stack pointers
    pub sp_el0: u64,
    pub sp_el1: u64,
    // Exception return state
    pub elr_el2: u64,   // Guest PC
    pub spsr_el2: u64,  // Guest PSTATE (initial 0x3c5 = EL1h, DAIF masked)
    // EL1 System Registers
    pub sctlr_el1: u64,
    pub cpacr_el1: u64,
    pub ttbr0_el1: u64,
    pub ttbr1_el1: u64,
    pub tcr_el1: u64,
    pub mair_el1: u64,
    pub vbar_el1: u64,
    pub contextidr_el1: u64,
}
```

### 5.2 `VcpuExit`
Ephemeral exit syndrome captured upon returning to EL2:

```rust
pub struct VcpuExit {
    pub vector: u64,
    pub esr_el2: u64,
    pub far_el2: u64,
    pub hpfar_el2: u64,
    pub reason: VcpuExitReason,
}

pub enum VcpuExitReason {
    Hvc { imm: u16, arg0: u64 },
    Stage2DataAbort { ipa: u64, is_write: bool, dfsc: u8 },
    Stage2InstructionAbort { ipa: u64, ifsc: u8 },
    SmcTrap,
    SysRegTrap,
    FpSimdTrap,
    Unknown,
}
```

### 5.3 Assembly world switch (`__vcpu_run`)
```text
Host EL2 Rust:
  calls __vcpu_run(context: *mut VcpuContext, exit: *mut VcpuExit) -> u64
      |
      v
  Save host callee-saved registers (x19..x29, x30, sp_el2) to host stack
  Load VTTBR_EL2 with guest root table PA & VMID
  Load guest EL1 system registers (SCTLR_EL1, TTBR0_EL1, etc.)
  Load SPSR_EL2, ELR_EL2, SP_EL1 from context
  Load guest GPRs (x0..x30) from context
  isb
  eret (drops to guest EL1)
      |
      | ... Guest executes at EL1 ...
      | Exception occurs (e.g. HVC #0, Stage-2 Data Abort)
      v
  EL2 Vector Table (Slot 8: Lower EL AArch64 Sync -> __vcpu_exit_handler)
  Save guest GPRs (x0..x30) into context
  Save SPSR_EL2, ELR_EL2, SP_EL1 into context
  Save guest EL1 system registers into context
  Read ESR_EL2, FAR_EL2, HPFAR_EL2 into exit record
  Restore host callee-saved registers and sp_el2
  Return exit reason to Rust caller
```

---

## 6. Guest Device Tree generator (`guest_fdt.rs`)

`guest_fdt.rs` is an allocation-free DTB serializer in `tvisor_util`. It writes into a caller-supplied buffer:

1. **Header**: Big-endian magic `0xd00dfeed`, totalsize, structure offset, strings offset, memory reservation offset, version 17.
2. **Memory Reservation Block**: Empty reservation list (`0, 0`).
3. **Structure Block**:
   - `/` (root node):
     - `#address-cells = <2>`
     - `#size-cells = <2>`
     - `model = "tvisor-virt-v1"`
     - `compatible = "tvisor,virt", "linux,dummy-virt"`
   - `/chosen`
   - `/cpus`:
     - `#address-cells = <1>`
     - `#size-cells = <0>`
     - `/cpus/cpu@0`:
       - `device_type = "cpu"`
       - `compatible = "arm,cortex-a72"`
       - `reg = <0>`
   - `/memory@40000000`:
     - `device_type = "memory"`
     - `reg = <0x00000000 0x40000000 0x00000000 0x00002000  0x00000000 0x40003000 0x00000000 0x00001000  0x00000000 0x40100000 0x00000000 0x00001000>` (describes exact backed RAM regions: payload/scratch, stack, and DTB; unmapped guard page `0x4000_2000` is omitted)
4. **Strings Block**: Compact NUL-terminated property names.

---

## 7. Controlled EL1 test payload sequence

The test payload is written in position-independent assembly (`.section .payload`), copied to guest RAM at `0x4000_0000`:

```text
Sequence:
1. Initialize SP_EL1 to 0x4000_4000 (top of stack page [0x4000_3000, 0x4000_4000)).
2. Checkpoint 1 (RAM Write/Read):
   Write pattern 0x5039_5041_594c_4f41 ("P9PAYLOA") to scratch page 0x4000_1000.
   Read back and verify equality.
   Issue `HVC #0` with x0 = 1, x1 = read pattern (Checkpoint 1 Passed).
   (On failure, issues `HVC #1` with x0 = 0xdead, x1 = read pattern).
3. Checkpoint 2 (Register Verification):
   Read `CurrentEL` -> verify EL1 (value == 0x04).
   Read `MPIDR_EL1` -> verify bit 30 (UP flag) and Aff0 == 0.
   Read `SCTLR_EL1` -> verify accessible without fault.
   Issue `HVC #0` with x0 = 2, x1 = MPIDR_EL1 (Checkpoint 2 Passed).
   (On failure, issues `HVC #2` / `HVC #3` with x0 = 0xdead, x1 = reg value).
4. Checkpoint 3 (Deliberate Stage-2 Translation Fault):
   Attempt to read from unmapped IPA `0x3000_0000`.
   Hardware triggers Stage-2 Data Abort -> traps to EL2.
   EL2 verifies:
     - Vector = 8 (Lower EL AArch64 Sync)
     - ESR_EL2.EC = 0x24 (Data Abort from Lower EL)
     - ESR_EL2.DFSC = 0x04..0x07 (Translation fault)
     - HPFAR_EL2 matches IPA 0x3000_0000
   EL2 reports "Phase 9 test successfully completed" and halts.
```

---

## 8. Verification and acceptance criteria

* **Host Tests (`cargo test-host`)**:
  * Stage-2 descriptor encoding and bitfield validation.
  * 39-bit Stage-2 table walking, mapping, and permissions checks.
  * Guest FDT serializer emits valid DTB describing exact sparse memory banks that parses identically with `Fdt`.
  * `guest_dtb_and_stage2_mappings_agree_exactly` verifies that every advertised DTB page has an active Stage-2 mapping and that unmapped pages (like stack guard) are not advertised.
  * `VTCR_EL2`, `VTTBR_EL2`, `HCR_EL2`, `CPTR_EL2`, and `VMPIDR_EL2` register decoding and bit assertions.
  * `GuestResourceManager` rollback error-retention and retry tests under injected failure.
* **Bare-Metal Build (`cargo build`)**:
  * Clean build with no warnings, zero global heap, and no stack use before SP selection.
* **Raspberry Pi 4 Execution**:
  * Allocates individual sparse guest RAM (16 KiB) and Stage-2 table pages from [`GlobalPageAllocator`](../tvisor_util/page_allocator.rs).
  * Enters guest EL1 via `__vcpu_run`.
  * Observes and logs Checkpoint 1 (`HVC #0`, x0=1) and Checkpoint 2 (`HVC #0`, x0=2) resumes.
  * Observes and logs the final deliberate Stage-2 translation fault on unmapped IPA `0x3000_0000`.
  * Executes architectural Stage-2 deactivation (`tlbi vmalls12e1is` with VMID 1 active $\rightarrow$ `dsb ish; isb` $\rightarrow$ clear `HCR_EL2.VM` and `VTTBR_EL2`), rolls back all allocated pages, and verifies that final allocator in-use page count matches the initial count (0 leaked pages).
