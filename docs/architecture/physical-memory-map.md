# Physical memory map

This page locates physical regions and their owners. [Memory management](memory.md)
defines allocation, mapping and synchronization policy; [the guest platform](virtualization.md#guest-address-space)
defines guest IPAs. All intervals below are half-open: `[start, end)`. Addresses
are CPU physical addresses unless explicitly marked as VA or IPA.

## Sources of boundaries

| Boundary class | Authority | What can change it |
|---|---|---|
| Hardware-defined | BCM2711 address decoding in the selected peripheral configuration | Platform or hardware configuration |
| DTB-derived | Current `/memory`, reservation block, `/reserved-memory`, CPU release properties and translated `/soc/ranges` | Firmware, RAM size, overlays or working DTB |
| Linker-defined | [rpi.ld](../../scripts/rpi.ld) and symbols in the exact ELF | Source, features, build profile or linker changes |
| Runtime allocation | Final memory map, allocator state and boot allocation order | Reservations, topology, workload and allocation history |
| Deployment placement | U-Boot download addresses and exact file lengths | Boot session and operator/runner configuration |

The tables use the supported low-peripheral BCM2711 configuration and examples
from the development board. They do not prescribe addresses for arbitrary Pi
firmware, RAM sizes or tvisor builds. Re-read the live DTB and derive symbols
for the ELF actually being loaded.

## Platform address space

The main/ARM-local peripheral windows below follow BCM2711's low-peripheral
address decoding; their actual discovered windows come from translated DT
`ranges`. RAM bank ends and firmware carve-outs are firmware/DT configuration,
not the installed RAM capacity interpreted as one continuous usable interval.

| Physical interval, board example | Region and owner | Source |
|---|---|---|
| `[0x00000000, 0x38000000)` | Low ARM RAM bank, with reservations and allocations described below | DT `/memory` |
| `[0x38000000, 0x40000000)` | Firmware/GPU carve-out; unavailable to tvisor's allocator | Platform policy: low zero-based bank end through `0x40000000` |
| `[0x40000000, 0xfc000000)` | Second ARM RAM bank; usable portions can supply host/guest allocations | DT `/memory` |
| `[0xfc000000, 0xfe000000)` | Peripheral MMIO | BCM2711 configuration and DT `/soc/ranges` |
| `[0xfe000000, 0xff800000)` | Main peripheral MMIO, including UART/GPIO | BCM2711 configuration and DT `/soc/ranges` |
| `[0xff800000, 0x100000000)` | ARM-local peripheral MMIO, including GIC | BCM2711 configuration and DT `/soc/ranges` |

Additional RAM can exist above `0x100000000` on other configurations. The
current physical allocator has a 4 GiB aperture; discovery does not make memory
above that limit allocatable. The platform's reserved low-memory carve-out is
accounted as reserved RAM, not guest backing or an MMIO mapping.

Within those broad intervals, subtract every permanent reservation before
considering allocation. Relevant low-bank regions include:

| Physical interval or boundary | Owner/use | How to obtain it |
|---|---|---|
| `[0x00000000, 0x00001000)` in the captured board configuration | Reserved firmware boot/holding storage and release-cell page | DT reservations; not an assumed universal firmware footprint |
| `[cell, cell + 8)`; secondary cell examples `0xe0`, `0xe8`, `0xf0` | Firmware spin-table release cells, written once by CPU0 during validated startup | Each CPU's DT `cpu-release-addr` |
| All other firmware holding code/data extents | Firmware-owned and retained, even if separate from cell pages | Complete DT reservation block and `/reserved-memory` contract |
| Each `[alloc_range.start, alloc_range.end)` for a supported dynamic reservation | Entire possible placement window excluded, not merely the requested allocation size | Current `/reserved-memory` `alloc-ranges`; see [reservation policy](memory.md#physical-memory-classification) |
| `[__image_start, __image_end)` | Hypervisor's permanent linked runtime footprint | Exact ELF symbols; link base currently `0x04000000` |
| `[floor_page(dtb), ceil_page(dtb + totalsize))` | Live host DTB, permanently reserved while referenced | `fdt=` address and validated DTB total size; lab address `0x2eff1f00` |
| `[floor_page(image_source), ceil_page(image_source + file_size))` | Staged Linux source, reserved in the host memory map | `image=` handoff; lab source starts at `0x30100000` |
| `[0x36b2b000, 0x38000000)` in the captured U-Boot layout | U-Boot runtime arena; must survive downloads and takeover, then eligible for reuse if otherwise usable | Fresh `bdinfo`/LMB; not a permanent post-takeover reservation |

`floor_page`/`ceil_page` use the 4 KiB page size. A release cell must lie in a
fully reserved RAM page before that page is mapped writable; cell addresses
alone cannot establish holding-code ownership. A firmware reservation such as
the observed `[0x3ef66280, 0x3ef66670)` is already inside the broad firmware
carve-out; nested reservations are not extra free RAM boundaries.

## Linked hypervisor footprint

The complete runtime interval includes file-backed sections, BSS, boot stack,
guard and table arena. Its end is not the downloaded ELF file end. The ELF may
also contain debug/non-loadable data beyond the runtime footprint, so validate
both staging `[load_address, load_address + file_size)` and runtime placement.

| Symbol interval | Contents and physical owner |
|---|---|
| `[__image_start, __text_start)` | ELF headers/alignment at the link base; hypervisor reservation |
| `[__text_start, __text_end)` | Executable code, including secondary entry and publication helpers |
| `[__vectors_start, __vectors_end)` | Page-aligned EL2 vector storage |
| `[__rodata_start, __rodata_end)` | Read-only constants and unwind data |
| `[__writable_start, __writable_end)` | Data/GOT/BSS and padding; allocator bitmaps, static map and CPU metadata live here |
| `[__boot_stack_guard_start, __boot_stack_bottom)` | Reserved 4 KiB boot guard; no EL2 mapping |
| `[__boot_stack_bottom, __boot_stack_top)` | CPU0's reserved 64 KiB usable stack |
| `[__el2_stage1_tables_start, __el2_stage1_tables_end)` | Reserved 64 KiB EL2 stage-1 table arena; initial root and subordinate tables |

For a concrete example, the ordinary debug ELF built from `ba6db30` has these
symbol-derived bounds. They illustrate the layout, not fixed placement promises
for subsequent builds:

| Region | Example physical interval |
|---|---|
| Headers/alignment | `[0x04000000, 0x04001000)` |
| Text | `[0x04001000, 0x04098000)` |
| Vectors | `[0x04098000, 0x04099000)` |
| Read-only data | `[0x04099000, 0x040d2000)` |
| Writable image including BSS/padding | `[0x040d2000, 0x04117000)` |
| Boot guard | `[0x04117000, 0x04118000)` |
| Boot stack | `[0x04118000, 0x04128000)` |
| EL2 table arena | `[0x04128000, 0x04138000)` |
| Complete runtime footprint | `[0x04000000, 0x04138000)` |

Regenerate from your own ELF, keeping its configuration/provenance with the
output. BSS and table storage must be initialized even though NOLOAD regions
have no downloaded contents:

```sh
elf=target/aarch64-unknown-none/debug/tvisor
llvm-nm -n "$elf" | rg '__image_(start|end)$|__text_(start|end)$|__vectors_(start|end)$|__rodata_(start|end)$|__writable_(start|end)$|__bss_(start|end)$|__boot_stack_(guard_start|bottom|top)$|__el2_stage1_tables_(start|end)$'
llvm-readelf -l "$elf"
```

## Runtime allocations

These objects occupy managed RAM after takeover. Their sizes/owners are defined,
but their physical bases are chosen by the allocator. Do not restore old heap
or worker-stack addresses as constants.

| Physical interval | Size and owner | Source of the actual boundary |
|---|---|---|
| `[stack.guard_start, stack.top)` per enabled secondary | 17 pages: 4 KiB guard plus 64 KiB usable stack; hypervisor-owned until reset | [Stack allocator](../../tvisor_util/pcpu.rs) and prepared per-CPU identity |
| `[heap.arena_start, heap.arena_end)` | Default 256 pages / 1 MiB; hypervisor heap owns the arena for its lifetime | [Heap initialization](../../src/heap.rs) and heap statistics |
| `[guest_ram_pa, guest_ram_pa + 0x20000000)` | Contiguous 512 MiB allocation owned/tracked by `VmCtl` on behalf of the VM | [VM allocation](../../src/vmctl.rs), not the guest IPA value |
| `[table_pa, table_pa + 0x1000)` per guest table page | VM-owned stage-2 root/subordinate tables; not guest RAM | `VmMemUsage::GuestStage2PageTable` records in `VmCtl` |
| `[scratch_pa, scratch_pa + 0x1000)` for each of two reserved pages | Hypervisor-owned TLB acceptance backing, retained until reset; absent in ordinary images | [Scratch preparation](../../src/mm.rs), only with TLB acceptance features |
| Remaining managed pages | Allocator-controlled free or in-use RAM, not implicitly VM-owned | Final usable ranges and live allocator bitmaps |

Secondary stacks are allocated before the heap, and both precede guest backing.
The complete guard-plus-stack allocation remains InUse even though its guard
is unmapped. Heap suballocation does not change physical page ownership.
Failed/late startup does not make a stack allocation available for another owner.

The 512 MiB guest IPA range `[0x40000000, 0x60000000)` maps to the allocated
physical backing; equal-looking numbers in the platform RAM-bank table do not
imply identity mapping. The loaded Image and generated DTB are placed within
this backing at offsets computed by `LinuxBootLayout`. Stage-2 tables stay
hypervisor-accessible and are not advertised as guest RAM. The original staged
Image remains a separate host reservation after its contents are copied.

## Ownership, mappings and inspection

Firmware reservations and the linked runtime footprint are unavailable to the
physical allocator. Heap/stacks and VM backing/tables are managed pages currently
held by explicit owners; free managed RAM has no live object owner. Peripheral
MMIO is neither allocatable RAM nor guest backing. A mapping permission does not
transfer ownership: the firmware cell page can be host RW/NX while firmware
storage remains reserved; a stack guard remains owned while unmapped; GICV can
be mapped into a guest while the device stays host-owned.

The lab's staging checks require both tvisor file/runtime ends below DTB page
start `0x2eff1000`, and the staged Linux file end below `0x36b2b000`. These are
validated-session bounds, not allocator boundaries. Revalidate them before any
download using [the deployment procedure](../setup/raspberry-pi4.md#validate-the-live-layout).

Use the working DTB and fresh U-Boot inspection for banks/reservations, exact
ELF symbols for static objects, and diagnostics for the prepared CPU, heap and
memory-map addresses. Guest backing and stage-2 page addresses come from the
live `VmCtl` allocation records; the current diagnostic Image message reports
source PA and destination IPAs, not all backing/table PAs. Ordinary images
suppress detailed diagnostics. Reconcile the final usable map and allocation
records rather than
inferring free RAM from gaps between a few log messages. A physical ownership
map also does not specify DMA bus addresses or device-assignment isolation;
those need the platform's [separate addressing contracts](platform.md#bus-addresses-and-cpu-addresses).

The platform address-decoding reference is the
[BCM2711 peripheral manual](https://datasheets.raspberrypi.com/bcm2711/bcm2711-peripherals.pdf).
Discoverable configuration and the checked linker/runtime sources above determine
which portions tvisor actually uses.
