# U-Boot memory layout on Raspberry Pi 4

This note describes the memory layout relevant when U-Boot loads tvisor on a
Raspberry Pi 4 (BCM2711).

There are two different layouts to consider:

1. The BCM2711 physical address map, which is defined by the hardware and
   firmware configuration.
2. U-Boot's live RAM layout, which depends on the installed RAM, firmware,
   device tree, U-Boot build, and images loaded during this boot.

U-Boot relocates itself at runtime. Therefore, there is no single fixed address
range occupied by U-Boot on every Raspberry Pi 4.

## BCM2711 ARM physical address map

The Raspberry Pi firmware normally enables the BCM2711 "Low Peripheral" mode.
In that mode, the ARM physical address space includes the following regions:

| Physical address range | Purpose |
| --- | --- |
| `0x0000_0000` upward | ARM RAM reported by firmware/device tree |
| `0xFC00_0000`-`0xFF7F_FFFF` | Main peripherals |
| `0xFF80_0000`-`0xFFFF_FFFF` | ARM-local peripherals |
| `0x1_0000_0000` upward | Additional RAM, when present above 4 GiB |

Do not assume that every address below the installed RAM size is usable RAM.
The device tree may split RAM into multiple banks and reserve regions for the
firmware, GPU, DMA, or other purposes.

Some useful ARM physical peripheral addresses are:

| Address | Device |
| --- | --- |
| `0xFE00_0000` | Common main-peripheral base |
| `0xFE00_3000` | System timer |
| `0xFE20_0000` | GPIO |
| `0xFE20_1000` | PL011 UART0 |
| `0xFE21_5000` | Auxiliary block, including the mini UART |
| `0xFF80_0000` | ARM-local peripheral base |
| `0xFF84_0000` | GIC-400 base |
| `0xFF84_1000` | GIC distributor (`GICD`) |
| `0xFF84_2000` | GIC CPU interface (`GICC`) |

### ARM physical addresses versus legacy bus addresses

The BCM2711 documentation frequently identifies main peripherals with legacy
bus addresses. In Low Peripheral mode, an address written as `0x7Enn_nnnn` in
the legacy address space is visible to the ARM at `0xFEnn_nnnn`.

For example:

```text
Legacy/bus address:   0x7E20_0000
ARM physical address: 0xFE20_0000
```

Code running on the ARM CPU should use the ARM physical address when creating
its stage-1 or stage-2 mappings. A DMA engine may use a different bus address;
do not blindly give an ARM physical address to DMA hardware.

## Inspecting the live U-Boot layout

Run the following at the U-Boot prompt:

```text
=> bdinfo
```

The important fields are:

| Field | Meaning |
| --- | --- |
| `DRAM bank` / `start` / `size` | RAM banks U-Boot obtained from the platform |
| `relocaddr` | Address to which U-Boot relocated itself |
| `reloc off` | Difference between the relocation address and link address |
| `fdt_blob` | U-Boot's control device tree |
| `new_fdt` | Relocated device tree, when shown |
| `lmb_dump_all` | Available and reserved Logical Memory Block regions |

The exact output varies with the U-Boot version and build configuration. In
particular, the LMB information is present only when LMB support is enabled.

Inspect the standard image-loading variables as well:

```text
=> printenv kernel_addr_r
=> printenv fdt_addr
=> printenv fdt_addr_r
=> printenv ramdisk_addr_r
=> printenv loadaddr
=> printenv bootm_low
=> printenv bootm_size
```

These variables are proposed load addresses. They do not prove that an entire
region is permanently reserved or that an arbitrarily large image will fit.

## Layout observed on the tvisor development board

The following `bdinfo` snapshot was captured from the Raspberry Pi 4 used to
develop tvisor:

| Region or object | Address or range |
| --- | --- |
| DRAM bank 0 | `0x0000_0000`-`0x37FF_FFFF` (896 MiB) |
| DRAM bank 1 | `0x4000_0000`-`0xFBFF_FFFF` (3008 MiB) |
| Boot parameters | `0x0000_0100` |
| U-Boot runtime LMB reservation | `0x36B2_B000`-`0x37FF_FFFF` |
| Stack start / IRQ stack | `0x37B3_AC90` |
| U-Boot control DTB | `0x37B3_ACA0` |
| Relocated U-Boot address | `0x37F4_D000` |
| U-Boot translation tables | `0x37FF_0000` |
| Framebuffer base | `0x3E3C_D000` |

The range `0x36B2_B000`-`0x37FF_FFFF` is the U-Boot reserved runtime arena,
not the exact extent of its executable. It includes U-Boot and supporting
allocations such as its stack, DTB, translation tables, and heap.

The complete LMB reservation list was:

| Reserved range | Size | Observation |
| --- | --- | --- |
| `0x0000_0000`-`0x0000_0FFF` | 4 KiB | Low page containing boot data |
| `0x36B2_B000`-`0x37FF_FFFF` | `0x014D_5000` | U-Boot runtime arena |
| `0x3EF6_6280`-`0x3EF6_666F` | `0x3F0` | Firmware/device-tree reservation |
| `0x4000_0000`-`0xFBFF_FFFF` | `0xBC00_0000` | Entire second DRAM bank is LMB-reserved |

An LMB reservation means U-Boot considers the region unavailable for normal
memory-placement decisions. It does not necessarily mean the address is not
backed by working RAM. A direct load may write there even when an LMB-aware
operation rejects it, so such writes should be avoided until the reason for the
reservation is understood.

This snapshot is not a permanent ABI. Re-run `bdinfo` after changing the
firmware, DTB, U-Boot build or configuration, or boot sequence. The most useful
snapshot is taken immediately before launching tvisor.

### Passing the live LMB list to tvisor

The working DTB does not contain U-Boot's live allocator state. To refine the
temporary pre-takeover safety map, the caller may copy reservations printed by
the same boot session's `bdinfo` into repeatable arguments of this form:

```text
lmb=<hex-start>:<hex-size>
```

For the captured layout above, a `go` launch is conceptually:

```text
=> go ${tvisor_entry} fdt=${fdt_addr} \
     bootmem=4000000:${filesize} \
     lmb=0:1000 \
     lmb=36b2b000:14d5000 \
     lmb=3ef66280:3f0 \
     lmb=40000000:bc000000
```

`bootelf` receives the same tagged arguments after its ELF address, with its
staging buffer described as `bootmem=2000000:${filesize}`. `go` describes the
downloaded container at its direct load address. Tvisor scans the bounded
argument array, so it does not depend on the different `argv[0]` conventions
of `go` and `bootelf`. Malformed region arguments are fatal, but absent
arguments do not prevent construction of the permanent post-takeover map.

These regions are printed as `HANDOFF` and excluded from `INITIAL`. They are
not printed as permanent `RESERVED`, and they do not reduce post-takeover
`USABLE` RAM. The transition code must use `INITIAL` until it has installed a
tvisor-owned stack, page tables, vectors, and copied state and has crossed an
explicit no-return boundary.

Do not copy these sample values permanently to a different board or U-Boot
configuration. The command must be updated whenever the immediately preceding
`bdinfo` list changes.

### Current tvisor placement at `0x0400_0000`

The current linker script places tvisor at `0x0400_0000`. The inspected debug
ELF has this layout:

```text
ELF base:       0x0400_0000
entry point:    0x0400_1010
loaded extent:  [0x0400_0000, 0x0400_2320)
```

This range is in DRAM bank 0 and does not overlap any LMB reservation in the
captured configuration. In particular, it is well below the U-Boot runtime
arena at `0x36B2_B000`-`0x37FF_FFFF`.

The ELF container is staged separately at `0x0200_0000`:

```text
ELF staging address:  0x0200_0000
tvisor link address:  0x0400_0000
```

Verify both addresses against the load variables and `bdinfo` from the exact
boot session. With `bootelf`, keep the ELF container and linked destination
separate:

```text
=> fatload mmc 0:1 0x02000000 tvisor
=> bootelf 0x02000000
```

This lets `bootelf` copy the loadable segments from the staging buffer to their
linked addresses without source/destination overlap.

## Finding the loaded tvisor range

For example, after loading tvisor from the first FAT partition:

```text
=> fatload mmc 0:1 ${kernel_addr_r} tvisor
=> echo load=${kernel_addr_r} size=${filesize}
```

The occupied half-open address range is:

```text
[kernel_addr_r, kernel_addr_r + filesize)
```

The first bytes can be inspected before execution:

```text
=> md.b ${kernel_addr_r} 40
```

Before loading another image, check that its complete range does not overlap
tvisor, the DTB, an initrd, U-Boot's relocated data, or an LMB reservation.

## Regions tvisor should exclude

When tvisor constructs its allocator and stage-2 mappings, derive usable memory
from the device tree `/memory` nodes and subtract at least:

- `/reserved-memory` entries and the device-tree memory reservation table
- tvisor's own image, stacks, page tables, heap, and other runtime storage
- U-Boot's relocated image and runtime allocations while they are still needed
- the DTB passed to tvisor
- loaded guest images and initrds
- firmware- and GPU-reserved memory
- MMIO ranges, including the BCM2711 peripheral windows
- U-Boot LMB reservations

Do not hard-code the installed RAM size or assume a single contiguous bank.
This is especially important on Raspberry Pi 4 variants with more than 4 GiB
of RAM.

## References

- [BCM2711 ARM Peripherals](https://datasheets.raspberrypi.com/bcm2711/bcm2711-peripherals.pdf)
- [U-Boot `bdinfo` command](https://docs.u-boot.org/en/latest/usage/cmd/bdinfo.html)
- [Upstream Raspberry Pi U-Boot board code](https://github.com/u-boot/u-boot/blob/master/board/raspberrypi/rpi/rpi.c)
- [Upstream Raspberry Pi 4 U-Boot configuration](https://github.com/u-boot/u-boot/blob/master/configs/rpi_4_defconfig)
