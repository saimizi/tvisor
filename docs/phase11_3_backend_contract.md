# Phase 11.3 lab-rpi4 backend contract

Task 2 deliverable, 2026-10-31. This specifies the supported entry profile and
its evidence. It does not activate release or certify the unfinished startup
implementation. Read with [the design](phase11_3_design.md) and
[the startup interface](rpi4_secondary_startup.md).

Current status: this dated Task 2 contract is implemented by the named backend.
Tasks 11–14 completed its local and physical acceptance conditions; see the
[completion handoff](phase11_3_status.md). The firmware trust boundary below
continues to apply.

## Supported profile and trust boundary

The backend targets lab-rpi4: Raspberry Pi 4 Model B / BCM2711, its single
Cortex-A72 cluster, and the firmware's AArch64 armstub spin-table handoff.
It supports one enabled secondary with a unique DT-described release cell.
It does not support PSCI, other CPU implementations, a monitor that changes the
entry regime, warm restart/hotplug, shared release cells, or multiple clusters.

Platform documentation identifies BCM2711's four A72 cores and shared 1 MB L2.
The single-cluster interpretation is the supported integration assumption,
consistent with that description and the observed board topology, not something
proved by the DT enable-method alone. See the official
[Raspberry Pi BCM2711 description](https://www.raspberrypi.com/documentation/computers/processors.html#bcm2711).

The backend trusts firmware to implement this profile and truthfully reserve
its live holding memory. Compatibility strings select a supported platform;
they do not authenticate firmware or establish live secondary state. No runtime
firmware instruction scan, binary hash allowlist, or fixed cell address is
required. A custom armstub or monitor must preserve this contract or supply a
separately reviewed adapter. Changed firmware requires bring-up revalidation.

## Physical entry and firmware evidence

The reviewed source is Raspberry Pi tools commit
`fc0e73c13865450e95edd046200e42a6e52d8256`,
[`armstubs/armstub8.S`](https://github.com/raspberrypi/tools/blob/fc0e73c13865450e95edd046200e42a6e52d8256/armstubs/armstub8.S).
An already retained local copy was inspected for this task; the upstream URL
was unavailable through the web fetcher during this review.

The source establishes the following sequence on each participating core:

1. Set `SCR_EL3 = 0x5b1`, selecting non-secure AArch64 execution and enabling HVC.
2. Set `ACTLR_EL3 = 0x73` and `CPUECTLR_EL1.SMPEN = 1`.
3. Set `SCTLR_EL2 = 0x30c50830`: translation, data cache and instruction cache
   disabled, little-endian data access.
4. Set `SPSR_EL3 = 0x3c9`, then ERET into EL2h with D/A/I/F masked.
5. A secondary waits in a stackless WFE/read loop and branches to its nonzero
   release address. Between entering EL2 and that branch it does not change
   translation, cache, endianness or execution-level state.

This establishes the expected physical entry under this firmware profile.
The trampoline must independently reject `CurrentEL != 8`, `SCTLR.M/C/EE != 0`,
and unsupported CPU identity before stack use or ordinary shared Rust access.
The backend expects I-cache off from firmware; the draft permits it on and
invalidates it before enabling its own regime. That compatibility requires
instruction-fetch/publication review in Tasks 6 and 9; this document does not
approve stale instruction-cache contents.

### Existing board observations and provenance

The 2026-10-25 evidence is in ignored
`docs/evidence/phase11_0_completion/`. These observations support the firmware
profile, but are not a new observation of a secondary's live registers:

| Artifact | SHA-256 |
|---|---|
| `armstub8-fc0e73c.S` | `af6ed222708c9227591165a5c8cdcf0e4be252122b3bd065fcd42c15f7ee9bf8` |
| built `armstub.bin` | `791dfd5dcb40fe9c433aefe69c074add3a03fb200b588c24188209467dd128cc` |
| captured `live-armstub.bin` | `b9ba567ca26f4670953df5b0202510b7cc6e472d6d4fbee93ec709894d9cbc9c` |
| `normalized-armstub.bin` | `c56a271d0502a9c4ab3b78e2a2d5420fc03e9b2066e0de7a4c4333ca39191002` |
| reference `official-start4.elf` | `2cc344f6171797317ae0b6197191803b45e2f3cda091f0da31c0a3f0bfc47593` |

Task 2 recomputed these hashes. Both stub binaries are 336 bytes; their eight
differing byte positions are all inside the firmware-patched range
`[0xf0, 0x100)`. There are no differences outside that range. The retained
`reference_compare.txt` records a 2,263,968-byte match between the installed
firmware copy loaded for inspection and the official reference.
`inspection.txt` records board identity, reservations and DT CPU descriptions.
These files are diagnostic inputs; their availability is not a boot dependency.

## Coherency and implementation-defined access

The primary reference is Arm's
[Cortex-A72 TRM, ARM 100095_0003_06_en](https://documentation-service.arm.com/static/60368ce38f952d2e4134dc2e),
sections 4.3.67 (printed pages 4-206–4-208) and 7.7.2 (7-310–7-311).

The A72's cluster L1 data caches and L2 remain coherent independently of SMPEN.
SMPEN enables reception of broadcast instruction-cache/TLB maintenance and must
be set before MMU/cache enablement or maintenance. Cores inside the cluster can
share Inner Shareable memory even when external broadcast signals are disabled.
Thus the supported inference is **CPU-to-CPU sharing inside this cluster**;
SMPEN is a separate maintenance prerequisite. No claim is made for DMA,
VideoCore, external coherent masters or another cluster.

The TRM permits EL2 reads of `CPUECTLR_EL1` using `S3_1_C15_C2_1`; its EL2 write
permission is controlled by `ACTLR_EL3.CPUECTLR`. The draft only reads it, after
checking MIDR implementer `0x41` and part `0xd08`. The reviewed firmware also
sets ACTLR_EL3 bit 1. Tvisor must not broaden implementation-defined accesses
or silently repair firmware state in the trampoline.

This contract further requires identical Normal cacheable, Inner Shareable
attributes for shared startup objects and tables on both CPUs, correct cache
publication before release, and no conflicting aliases. Firmware is trusted to
have left caches in a safe initial state, with no dirty secondary lines covering
startup memory. MMU/cache-off bits alone cannot prove that history.

The generic [AArch64 boot protocol](https://docs.kernel.org/arch/arm64/booting.html)
also requires consistent secondary execution level, coherency and initialized
timer state, and describes reserved spin-table storage and event-based release.
It is corroborating interface documentation; it does not establish that an
arbitrary DT-described firmware implements the specific EL2 profile above.

## Prerequisite-to-evidence decision table

| Prerequisite | Basis | Required runtime/bring-up check | Task 2 decision |
|---|---|---|---|
| Supported platform and CPU | Board inspection, BCM2711 documentation, reviewed firmware profile | Validate platform selection and MIDR; record full MIDR including revision at arrival and review revision-specific constraints | Profile specified; live secondary/revision confirmation pending |
| AArch64 non-secure EL2h, MMU off, LE, D-cache off | Reviewed SCR/SPSR/SCTLR setup and stackless holding path | Check CurrentEL and SCTLR before record/stack use; capture entry state where safe | Established for the supported firmware profile; hardware checkpoint pending |
| Safe instruction fetch | Firmware I-cache disabled; published physical entry | Review linked entry/literal pools, clean code and local I-cache invalidation | Publication/instruction review pending Tasks 6/9/11 |
| Same CPU coherency domain | Supported single A72 cluster and TRM guarantees | Confirm selected target belongs to supported cluster; matching shareable mappings; acquire/checkpoint visibility | Documented conditional platform basis; cross-core hardware validation pending |
| Maintenance participation | Firmware sets SMPEN; A72-defined behavior | Identify A72 before CPUECTLR read; reject SMPEN-off; retain observation | Documented; observation/reporting pending |
| Permitted CPUECTLR read | A72 TRM access rules, identified CPU | Read only after CPU identification; do not write implementation-defined state | Read contract established for A72 profile |
| Target ownership and unique cell | Owned topology, startup memory discovery and full DT reservations | Lowest enabled nonzero logical ID, full affinity, unique aligned cell including disabled nodes | Existing validators; final integration pending |
| Writable zero cell and correct attributes | Host mapping validation, discovery zero check | Validate complete page RW/NX Normal identity mapping; recheck zero immediately before store; use assembly for PA zero | Runtime prerequisite; cannot be a platform constant |
| Startup data/code/tables ready | Prepared resources and publication sequence | Check bounds, permissions, guards, stable record/context, register policy, table freeze, cache completion | Runtime prerequisite; implementation review pending |
| Counter deadline | Firmware initializes CNTFRQ; physical counter is architectural | Validate frequency/conversion before release, bounded wrap-safe wait | Runtime prerequisite; integrated validation pending |

## Integration rule and outstanding acceptance

`EntryContract.el2_mmu_off` and `.coherent` may represent this documented
supported profile only when backend selection and the firmware trust boundary
are explicit. They must not be derived solely from DT enable-method, the boot
CPU's registers, or a desire to enable release. The current caller-supplied
boolean API does not enforce that selection; Tasks 3/8/10 must make it explicit.

Task 9 removed the caller-supplied `release_cell_writable` and
`startup_memory_published` booleans. The [consuming publication pipeline](phase11_3_publication.md)
now requires validated/frozen resources and a completed cache publication before
it can produce the one-shot release handle. These operational results are not
firmware guarantees. Explicit backend selection remains part of Task 10.

Before declaring hardware acceptance, retain fresh board/DT inspection, full
primary and secondary identity, entry SCTLR/SMPEN observations, the acquired
checkpoint, image hashes and source provenance. Confirm actual secondary state,
publication visibility and parking on lab-rpi4. Unsupported observations must
produce rejection/timeout; revising the accepted regime requires an adapter
review. Tasks 6/9/11 still owe instruction-order/cache/errata review. Tasks
12–14 owe success, bounded failure and guest regression evidence.

Task 2 is complete as a documented backend contract with explicit conditions.
It grants no unconditional release approval and changes no runtime behavior.

## Task 10 backend selection

[`secondary_backend.rs`](../tvisor_util/secondary_backend.rs) now selects the
supported board/CPU/cluster/armstub interface profile explicitly before returning
its contract. [Boot integration](phase11_3_boot_integration.md) uses that named
backend, not freely supplied entry booleans. This preserves the documented
firmware trust boundary; actual secondary state still requires hardware evidence.
