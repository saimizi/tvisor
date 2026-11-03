# Platform discovery and firmware contracts

The supported physical startup profile is Raspberry Pi 4 Model B / BCM2711,
one Cortex-A72 cluster with up to four CPUs, Non-secure AArch64 EL2 and the
armstub spin-table interface. Compatibility strings select this supported
profile; they do not authenticate firmware or measure another CPU's live state.
Changed firmware, a custom armstub or a monitor requires revalidation of the
entry contract. Other CPUs, clusters, physical PSCI startup and warm hotplug are
unsupported.

## Host handoff and discovery

U-Boot supplies a readable, unchanged host DTB with `fdt=<physical-address>` and
a separately loaded Linux Image with `image=<physical-address>,<file-size>`.
[The parser](../../tvisor_util/fdt.rs) scans a bounded argument array and accepts
the different `go`/`bootelf` argv layouts. It rejects missing/duplicate/malformed
required values. The complete linker-defined tvisor footprint and all source
buffers must be safely placed before transfer; see [execution](../setup/raspberry-pi4.md).

The DTB provides enabled RAM banks, FDT reservations, `/reserved-memory`, `/soc`
MMIO translations, `/chosen` console selection, aliases, CPU topology/startup
methods, and GIC interface regions. Discovery is allocation-free and uses
bounded owned descriptions where data must survive takeover. The host DTB itself
remains reserved and mapped read-only because retained references still use it.
The guest receives a separately generated DTB, not the host tree.

The active console must resolve to the supported AUX mini UART. Tvisor reuses
firmware/U-Boot clock, baud and GPIO setup; it does not independently bring up
arbitrary UART hardware. The physical UART SPI is discovered for CPU0 input.
GIC discovery supports `arm,gic-400`/`arm,cortex-a15-gic`, translating the
distributor, CPU, hypervisor and virtual CPU interface regions independently.
Do not assume these interfaces are physically contiguous.

## Bus addresses and CPU addresses

See the [physical memory map](physical-memory-map.md) for RAM banks, carve-outs,
MMIO windows and hypervisor/guest allocation boundaries.

A node's `reg` is in its parent bus's address space. Decode `ranges` with the
child/parent `#address-cells` and child `#size-cells`; cells are big-endian u32.
For a containing half-open range:

```text
parent_address = parent_base + (child_address - child_base)
```

Repeat through parent buses to reach a CPU PA. CPU VA translation remains a
separate step. A captured BCM2711 low-peripheral configuration has:

| Child bus range | ARM physical range | Size |
|---|---|---|
| `[0x7c000000, 0x7e000000)` | `[0xfc000000, 0xfe000000)` | 32 MiB |
| `[0x7e000000, 0x7f800000)` | `[0xfe000000, 0xff800000)` | 24 MiB |
| `[0x40000000, 0x40800000)` | `[0xff800000, 0x100000000)` | 8 MiB |

For example, mini-UART bus address `0x7e215040` maps to CPU PA `0xfe215040`.
This is a decoded example, not permission to hard-code an offset. MMIO uses
Device memory attributes. DMA/VideoCore bus addressing and coherency are separate
contracts; a CPU PA is not automatically a valid DMA descriptor address.

The platform policy infers the BCM2711 firmware carve-out from the end of the
zero-based low RAM bank through `0x40000000`, accounting for it as reserved RAM.
Other permanent and dynamic reservations follow the [memory policy](memory.md).
The current allocator supports only a 4 GiB aperture even if a different board
reports more RAM.

## Secondary firmware contract

The [named backend](../../tvisor_util/secondary_backend.rs) verifies both
`raspberrypi,4-model-b` and `brcm,bcm2711`, Cortex-A72 implementer/part, compatible
enabled DT CPU nodes and membership in the same supported cluster. It rejects
PSCI-described firmware interfaces. Every enabled secondary needs a unique,
8-byte-aligned DT release cell, including uniqueness against disabled nodes.

Firmware must reserve all live holding code/data and cells before allocation.
Tvisor requires a fully reserved RAM page around each cell, validates mapping
and ownership, and checks zero again immediately before release. Discovery of
`enable-method = "spin-table"` alone does not establish the following entry state:

| Property at `__secondary_entry` | Requirement |
|---|---|
| Execution level/state | Non-secure AArch64 EL2 |
| Translation | `SCTLR_EL2.M = 0` |
| Data cache | `SCTLR_EL2.C = 0` |
| Data endianness | `SCTLR_EL2.EE = 0` |
| Coherency | Firmware-established participation in the supported CPU domain |
| Maintenance | Identified A72 with `CPUECTLR_EL1.SMPEN = 1` |
| Entry address | Published physical address read from the target's release cell |
| Stack and argument registers | No usable firmware stack/arguments are assumed |

The trampoline masks DAIF before proceeding and checks EL2 and M/C/EE before
slot reads or stack use. Cortex-A72 identity precedes the implementation-defined
CPUECTLR read (`S3_1_C15_C2_1`). It reads this control defensively and does not
repair firmware state or write implementation-defined coherency controls.
Shared objects must have consistent Normal cacheable Inner Shareable mappings,
no conflicting aliases, and completed publication before release.

SMPEN enables reception of broadcast instruction-cache/TLB maintenance; it does
not itself establish or prove CPU coherency. The supported A72 cluster supplies
the CPU-to-CPU coherency basis. This claim does not extend to DMA, VideoCore,
another cluster or other coherent masters.

MMU/data-cache-off bits do not prove clean cache history. Firmware must leave
no conflicting dirty secondary data-cache lines and must ensure safe instruction
fetch. The trampoline does not require I-cache off; if enabled, it must not
contain stale entry instructions. Local invalidation after entry cannot repair
unsafe fetching before that point. Publication and [stackless entry checks](cpu.md#stackless-secondary-entry)
do not authenticate firmware.

## Firmware reference and bring-up evidence

The reviewed firmware reference is Raspberry Pi tools commit
`fc0e73c13865450e95edd046200e42a6e52d8256`,
[`armstubs/armstub8.S`](https://github.com/raspberrypi/tools/blob/fc0e73c13865450e95edd046200e42a6e52d8256/armstubs/armstub8.S).
Its documented setup uses SCR_EL3=`0x5b1`, ACTLR_EL3=`0x73`, SMPEN=1,
SCTLR_EL2=`0x30c50830` and SPSR_EL3=`0x3c9`, followed by an EL2h stackless
WFE/read/branch holding loop. Firmware binary matching is not a runtime dependency;
these values explain the supported trust profile rather than proving arbitrary
installed firmware complies with it.

On firmware changes, capture fresh board/DT reservations, CPU identities and
revisions, entry SCTLR/SMPEN, acquired checkpoints, source/artifact hashes and
publication/parking results. Review revision-specific constraints against the
CPU manual. Unsupported observations require rejection or a separately reviewed
adapter, not relaxed checks or speculative fallback. Test success, bounded
failure and the unchanged Linux guest using [hardware acceptance](../testing/hardware-tests.md).

## CPU revision and errata review

Record the actual MIDR variant/revision and review applicable errata whenever
the execution regime, MMIO layout or mapping-update path changes. The relevant
[Cortex-A72 errata notice, EPM-012079 v11.0](https://documentation-service.arm.com/static/5fa29f9fb209f547eebd3610)
includes 859971, 1319367 and 1387635:

- Keep read-sensitive Device pages separate from executable Normal pages;
  executable text/vectors are followed by Normal storage, not MMIO. Do not infer
  a need for an implementation-defined CPUACTLR write from a generic test pass.
- Review address-translation instructions together with the executing EL and
  lower-regime context changes. The current host mapping checks include
  `AT S1E2R` and `AT S1E2W`; a host-entry review is not a complete audit of guest
  or firmware context-switch behavior.
- Retain descriptor publication, DSB/TLBI/DSB/ISB and synchronization before new
  mappings are used. Ordinary post-release mappings stay frozen; the narrow
  scratch exception has its own [all-owner update protocol](memory.md#synchronized-el2-mapping-updates).

These are scoped implementation review constraints, not certification that
firmware or all guest execution has every required workaround. Hardware
applicability depends on the recorded CPU revision and regime.

## References

- [BCM2711 peripheral manual](https://datasheets.raspberrypi.com/bcm2711/bcm2711-peripherals.pdf)
- [Cortex-A72 TRM](https://documentation-service.arm.com/static/60368ce38f952d2e4134dc2e), especially CPUECTLR and SMPEN sections 4.3.67 and 7.7.2
- [Devicetree specification](https://www.devicetree.org/specifications/)
- [Linux AArch64 boot protocol](https://docs.kernel.org/arch/arm64/booting.html)

The generic boot protocol corroborates coherency and reserved spin-table
requirements; it does not establish the specific firmware EL2 profile above.
