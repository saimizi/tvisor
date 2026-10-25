# Verified lab-rpi4 secondary startup contract

Phase 11.0 discovery is complete for the board profile below. No secondary has
been released by tvisor. This contract supplies the prerequisites for 11.1/11.2;
the 11.3 trampoline must still validate actual entry registers before admitting
a secondary to Rust or normal execution.

## Firmware and bootloader identity

The board's SD `start4.elf` is byte-for-byte equal to `boot/start4.elf` from
[Raspberry Pi firmware revision effea745e592ec8a97fc54093a5673a7e6e515c9](https://github.com/raspberrypi/firmware/tree/effea745e592ec8a97fc54093a5673a7e6e515c9).
Both copies were loaded as data into separate RAM buffers and U-Boot `cmp.b`
reported **2,263,968 identical bytes**. SHA-256 of the reference, and therefore
of the compared installed file, is:

```text
2cc344f6171797317ae0b6197191803b45e2f3cda091f0da31c0a3f0bfc47593
```

Its embedded build identifies April 30, 2025 and VideoCore source revision
`5560078dcc8591a00f57b9068d13e5544aeef3aa`. SD `config.txt` has no active
`armstub`, `kernel`, `kernel_address`, `kernel_old`, or include override. The
boot partition contains `kernel8.img` (621,128 bytes, CRC32 `40583031`), and the
stub's patched primary entry is `0x80000`. The installed U-Boot banner is
2024.01, timestamp Jan 08 2024 15:37:48 UTC, with aarch64-poky-linux-gcc 13.4.0.
The banner timestamp is an identifier, not proof of when it was compiled.

The exact U-Boot build `.config` and downstream source revision are not
available locally. No secondary behavior is inferred from an upstream default
configuration: the actual firmware instructions, DTB, zero cells, and boot
handoff were inspected instead. This profile uses the Raspberry Pi firmware
holding loop, not an assumed U-Boot secondary trampoline. Unknown firmware
bytes, topology, or reservations are rejected before allocator takeover.

The firmware image contains, at file offset `0x1dc090`, the entire 336-byte
armstub reproduced from [Raspberry Pi tools revision fc0e73c13865450e95edd046200e42a6e52d8256](https://github.com/raspberrypi/tools/blob/fc0e73c13865450e95edd046200e42a6e52d8256/armstubs/armstub8.S)
with `GIC=1` and `BCM2711=1`. Every immutable byte of the live stub matches that
build. The only excluded comparison bytes are `0xd8..0xff`: release cells must
be zero, while firmware patches the DTB and primary-entry pointers.
The canonical binary and its original BSD license are kept in
`tvisor_util/fixtures/`. To reproduce it with GNU AArch64 tools:

```sh
aarch64-linux-gnu-gcc -DGIC=1 -DBCM2711=1 -c armstub8.S -o armstub.o
aarch64-linux-gnu-ld --section-start=.text=0 armstub.o -o armstub.elf
aarch64-linux-gnu-objcopy -O binary armstub.elf armstub.bin
```

This is a functional startup profile check, not a secure/measured-boot design.

## Topology, backend and holding-memory ownership

| Item | Verified description |
|---|---|
| Platform | BCM2711, Cortex-A72; boot MPIDR `0x80000000`, affinity zero |
| CPU nodes | Enabled affinities 0, 1, 2, 3; logical boot CPU zero |
| CPU enable method | Per-CPU `spin-table`; parent legacy method is not substituted |
| Release cells | Physical `0xd8`, `0xe0`, `0xe8`, `0xf0`; all initially zero |
| Holding loop | `WFE` at `0x7c`, cell load at `0x80`, zero check at `0x84` |
| Secondary branch | `BR x4` at `0xa4` after x0-x3 are zeroed |
| Live dependencies while parked | Code and cells in physical `[0x0, 0x1000)`; no stack accesses or translation-table walks |
| Reservation | Whole first page in the live DTB reservation block, retained permanently |
| PSCI | Not advertised; no SMC/HVC or version/feature probes executed |

Cells are separate 64-bit locations, although all share one 64-byte cache line.
The verified firmware selects a cell using its low MPIDR bits; tvisor's generic
CPU discovery continues to match full affinity values. A shared-cell backend is
not silently substituted for this profile. An event can wake every WFE waiter,
but zero cells keep unrequested CPUs in the firmware loop.

The stub and its initial GIC setup fit in `[0x0, 0x150)`; its complete parked
path has no references to relocated U-Boot code, stacks or page tables. The
MMIO touched during the earlier GIC initialization is no longer accessed by the
parking loop. Reserving the entire first page therefore protects the live
holding dependencies for this matched profile. Reclaiming unrelated U-Boot RAM
after the boot CPU's private-stack/table switch does not reclaim those bytes.

At each boot `src/smp.rs` validates the DT reservation, processor identity,
boot-CPU SMPEN, inherited identity translation of the low page, immutable stub
bytes, topology and zero cells. Physical address zero is read through assembly
after an `AT S1E2R` check; no null Rust pointer/reference is dereferenced. The
result is retained in a once-published contract. After memory discovery,
`src/boot.rs` checks the full holding page is reserved and absent from usable
RAM **before mapping allocator RAM or initializing the allocator**. An unknown
profile or missing reservation halts; it does not guess new memory bounds.

Issue [#29](https://github.com/saimizi/tvisor/issues/29) remains deferred. These
checks protect allocator ownership, not a general prohibition against future
explicit guest stage-2 mappings. The current guest mapping path excludes this
page, as documented in that issue.

## Entry state and coherency contract

The matched firmware configures Non-secure AArch64 EL2h entry with DAIF masked.
It writes `SCTLR_EL2 = 0x30c50830`, so M/C/I are clear and data accesses are
little-endian. It initializes CNTFRQ_EL0 to 54 MHz and CNTVOFF_EL2 to zero,
permits FP/SIMD at EL3, and writes `CPUECTLR_EL1.SMPEN = 1` before parking.
The observed boot CPU reads CPUECTLR_EL1 as `0x40`.

For Cortex-A72, SMPEN enables reception of broadcast instruction-cache/TLB
maintenance; it is not a generic switch creating data-cache coherency. The
[Arm Cortex-A72 TRM, CPUECTLR_EL1](https://documentation-service.arm.com/static/60368ce38f952d2e4134dc2e)
describes coherent L1/L2 data caches and requires SMPEN before enabling caches,
MMU or maintenance. This verified same-cluster firmware path establishes the
entry prerequisite. Consistent Normal-memory attributes/shareability and the
publication protocol remain required when secondaries join tvisor mappings.

The inherited SP, TPIDR, vectors, HCR, EL2 timer/FP trap state and general
registers other than x0-x3 are not a Rust calling environment. The 11.3 assembly
entry must establish them, preserve SMPEN, handle local stale translation/cache
state correctly, and validate CurrentEL/MPIDR/control registers before crossing
the approved shared-Rust-data boundary. No live secondary register measurement
has been claimed during 11.0; that observation requires the later guarded entry.
The profile is for the checked cold-boot path, not CPU hotplug or warm resume.

## Publication and release protocol for 11.3

This protocol is specified here; no release operation is implemented in 11.0.

1. Before release, reserve and initialize every reachable CPU's private storage,
   trampoline, startup records and page tables. Freeze the shared EL2 mappings
   and finish once-only initialization. Use bounded generation-tagged records.
2. Make all data/code the MMU-off secondary will read visible at the point of
   coherency, including trampoline instructions, table descriptors, records,
   mailbox and stack initialization. Complete the required cache maintenance
   and barriers before publishing a nonzero entry address. Cleaning code only
   to the point of unification is insufficient for the MMU/cache-off reader.
3. In a reviewed temporary EL2 mapping of the holding page, store the requested
   physical trampoline entry as a naturally aligned little-endian 64-bit value.
   Clean the containing cache line to PoC and complete a `DSB SY` before `SEV`.
   The other release cells in that line must remain unchanged. Volatile access
   alone is insufficient. Remove the temporary mapping according to the EL2
   mapping/TLB rules; this is never a guest mapping.
4. Secondary assembly identifies its full affinity and selects its private
   stack/state. Before enabling the shared translation regime, perform the
   required local maintenance and establish matching MAIR/TCR/shareability.
   Acquire-observe the boot CPU's release-published ready generation only once
   the verified coherent memory regime permits that protocol, then enter Rust.
5. Wait for bounded arrival and online acknowledgements. A timeout does not
   permit recycling a potentially live core's memory or writing a new attempt
   over its records. Reset before retrying.

The [AArch64 boot protocol](https://docs.kernel.org/arch/arm64/booting.html)
requires a reserved holding area and little-endian release cells. Linux's
[spin-table implementation](https://github.com/torvalds/linux/blob/v6.18/arch/arm64/kernel/smp_spin_table.c)
likewise publishes the entry, cleans the cell to PoC, and sends an event. Exact
instruction/cache-maintenance sequences are reviewed again with the actual
11.3 trampoline; publication is not replaced by an acquire load or ISB.

## Validation and artifacts

- 102 native ARM64 library tests passed: the original 95 plus seven profile
  tests covering every immutable-byte mutation, changed/nonzero release cells,
  missing/partial/overflowing DT reservations, stale usable-RAM classification,
  and allocator exclusion/release behavior for the holding page.
- Bare-metal build and formatting/whitespace checks passed.
- Normal board boots verified the live profile and pre-allocation reservation,
  then reached the existing one-vCPU Linux BusyBox shell. UART commands completed
  with a zero status marker. No secondary was released by tvisor.
- A RAM-only DTB copy with its low-page reservation removed was rejected with
  `MissingHoldingReservation` before private takeover/allocator initialization.
  The original DTB, low page, SD files and saved environment were untouched.
- This is targeted boot/guard validation, not a quick/overnight regression run.

Raw evidence is local and ignored under `docs/evidence/phase11_0_completion/`:
`inspection.txt`, `reference_compare.txt`, `comparison.txt`, `contract_boot.txt`,
`contract_guest.txt`, `negative_boot.txt`, `final_boot.txt`, and `final_guest.txt`.
The source files, built reference stub and official firmware binary are retained
there for reproduction. This document and the canonical 336-byte fixture carry
the reviewable findings without requiring generated evidence to be committed.
