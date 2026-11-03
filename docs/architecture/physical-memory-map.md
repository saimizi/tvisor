# Physical memory layout

The layout below shows where firmware, tvisor, stacks, the Linux source and
the heap sit in the Raspberry Pi 4's physical address space. Addresses increase
downward. Boxes are **not to scale**; every interval is `[start, end)`.

This is one captured board configuration, after secondary-stack and heap
initialization and before VM allocation. The static addresses come from the
matching diagnostics ELF; the allocated addresses come from that boot's UART
capture. They are examples, not fixed allocation addresses for every build or
boot. [Boundary sources](#boundary-sources-and-reproduction) explain what to
re-read when the configuration changes.

## Whole physical address space

```text
Physical address     Contents at this point in boot

0x00000000  +--------------------------------------------------------+
            | Firmware holding/release-cell page             4 KiB   |
0x00001000  +--------------------------------------------------------+
            | RAM excluded by the DT dynamic-reservation window     |
0x04000000  +--------------------------------------------------------+
            | TVISOR: code, vectors, data, CPU0 stack, EL2 tables     |-- A
0x0413b000  +--------------------------------------------------------+
            | RAM excluded by the DT dynamic-reservation window     |
0x2eff1000  +--------------------------------------------------------+
            | Live host DTB reservation                      60 KiB   |
0x2f000000  +--------------------------------------------------------+
            | RAM excluded by the DT dynamic-reservation window     |
0x30000000  +--------------------------------------------------------+
            | CPU1 / CPU2 / CPU3 guards and stacks           204 KiB  |-- B
0x30033000  +--------------------------------------------------------+
            | Free allocator-managed RAM                            |
0x30100000  +--------------------------------------------------------+
            | Staged Linux Image source, including page padding      |
0x30527000  +--------------------------------------------------------+
            | Rust heap arena                                1 MiB   |
0x30627000  +--------------------------------------------------------+
            | Free allocator-managed RAM                            |
            | Includes the former U-Boot arena after takeover        |
0x38000000  +========================================================+
            | Firmware / GPU carve-out                       128 MiB |
0x40000000  +========================================================+
            | Second RAM bank                               3008 MiB |
            | Free allocator-managed RAM before VM allocation       |
            | Guest backing and stage-2 tables use managed RAM      |-- C
0xfc000000  +========================================================+
            | Peripheral MMIO                                 32 MiB |
0xfe000000  +--------------------------------------------------------+
            | Main peripheral MMIO, including UART/GPIO        24 MiB |
0xff800000  +--------------------------------------------------------+
            | ARM-local MMIO, including GIC                     8 MiB |
0x100000000 +--------------------------------------------------------+
            End of the current allocator's 4 GiB aperture
```

The first RAM bank is `[0x00000000, 0x38000000)`. In this DTB, a dynamic
reservation's possible placement window is `[0x00000000, 0x30000000)`.
Tvisor conservatively excludes that entire window from allocation. Code and
the DTB inside it remain explicitly owned reservations; the apparent gaps around
them are **not free allocator RAM**. This is why secondary allocation begins at
`0x30000000`, rather than immediately after the linked image.

The staged Linux file ends at `0x30526808`; reservation rounds that up to
`0x30527000`. The free space between stacks and the staged Image is smaller
than the 1 MiB heap request, so the allocator places the heap after the Image.
These runtime placements can change when the Image, reservations or allocation
order changes.

The old U-Boot arena `[0x36b2b000, 0x38000000)` must be protected while U-Boot
is loading/launching tvisor. After the no-return takeover it is reusable RAM
unless another reservation excludes it. It is not a permanent firmware hole.
The firmware/GPU hole above it remains reserved, including the nested firmware
reservation `[0x3ef66280, 0x3ef66670)`.

## A — Inside the linked tvisor image

This expands `[0x04000000, 0x0413b000)` from the whole-machine layout, using
the **same captured diagnostics ELF**. A different build has different section
ends; derive them from its linker symbols.

```text
0x04000000  +----------------------------------------------+
            | ELF headers and alignment             4 KiB |
0x04001000  +----------------------------------------------+
            | Executable .text                    612 KiB |
            | Includes secondary entry/publication code   |
0x0409a000  +----------------------------------------------+
            | EL2 exception vectors                 4 KiB |
0x0409b000  +----------------------------------------------+
            | .rodata and unwind data             232 KiB |
0x040d5000  +----------------------------------------------+
            | .data / .got / .bss / alignment      276 KiB |
            | CPU slots, allocator bitmaps, static map    |
0x0411a000  +----------------------------------------------+
            | CPU0 stack guard: reserved, UNMAPPED   4 KiB |
0x0411b000  +----------------------------------------------+
            | CPU0 usable EL2 stack                64 KiB |
            | Grows toward lower addresses                |
0x0412b000  +----------------------------------------------+
            | EL2 stage-1 table arena              64 KiB |
            | Root and subordinate tables, 16 pages       |
0x0413b000  +----------------------------------------------+
            __image_end
```

The complete linker footprint is reserved, including guards and NOLOAD stack/
table storage. Downloaded ELF size is a separate staging extent and may include
non-loadable debug data beyond this footprint; check both before loading.

## B — Secondary stacks and heap

The three secondary stacks are physical allocator allocations, separate from
the linked image. Each holds a 4 KiB guard and a 64 KiB usable stack. The guard
page stays allocated even though its EL2 mapping is absent.

```text
0x30000000  +---------------------------------------+
            | CPU1 guard: UNMAPPED            4 KiB |
0x30001000  +---------------------------------------+
            | CPU1 stack                     64 KiB |
0x30011000  +---------------------------------------+
            | CPU2 guard: UNMAPPED            4 KiB |
0x30012000  +---------------------------------------+
            | CPU2 stack                     64 KiB |
0x30022000  +---------------------------------------+
            | CPU3 guard: UNMAPPED            4 KiB |
0x30023000  +---------------------------------------+
            | CPU3 stack                     64 KiB |
0x30033000  +---------------------------------------+

            Free RAM and staged Linux source
            (see the whole-machine layout)

0x30527000  +---------------------------------------+
            | Heap arena: 256 physical pages  1 MiB |
0x30627000  +---------------------------------------+
```

Contexts are in the linked `PCPU_SLOTS` array, not in these stack allocations.
Only enabled secondary CPUs receive a stack. Stack and heap bases are chosen
at runtime; the sizes come from [stack allocation](../../tvisor_util/pcpu.rs)
and [heap initialization](../../src/heap.rs). Their backing remains owned by
tvisor until reset; late startup does not permit stack reuse.

## C — Guest backing and stage-2 tables

The whole-machine diagram stops before VM allocation so it does not invent
physical addresses missing from the capture. When the VM is created,
[`VmCtl`](../../src/vmctl.rs) allocates a contiguous 512 MiB run from the managed
RAM, then separately allocates its stage-2 root/subordinate table pages. Denote
the chosen guest backing base by `guest_ram_pa`:

```text
Guest IPA space                         Host physical RAM allocation

0x40000000 +-----------------------+    guest_ram_pa +-----------------------+
           | Linux Image           |                 | Copied Linux Image    |
           | at header text_offset | -- stage 2 -->   | at the same offset    |
           | Generated guest DTB   |                 | Generated guest DTB   |
           | Remaining guest RAM   |                 | Remaining guest RAM   |
0x60000000 +-----------------------+                 +-----------------------+
           512 MiB                                  guest_ram_pa + 0x20000000

                                       Separate allocator-owned pages:
                                                    +-----------------------+
                                                    | Stage-2 root/table(s) |
                                                    +-----------------------+
                                                    Not exposed as guest RAM
```

The capture does not log `guest_ram_pa` or every stage-2 table PA; inspect live
`VmCtl` allocation records to obtain them. Do not substitute the guest IPA
`0x40000000` for a physical allocation base. The [guest layout](virtualization.md#linux-image-and-entry-abi)
places the Image and DTB within the allocated RAM. The source at `0x30100000`
is a separate host reservation and remains reserved after copying.

GICV is a host-owned MMIO device mapped into the guest's device IPA window,
not part of the 512 MiB RAM allocation. The trapped virtual UART/distributor
also do not turn the physical UART/GIC distributor into guest-owned devices.
TLB acceptance builds additionally allocate two 4 KiB scratch backing pages
and a host mapping at VA `0x100000000`; that VA is not a physical RAM address.
Ordinary images have no scratch allocation.

## Boundary sources and reproduction

| Region | Authority for its actual address |
|---|---|
| RAM banks, firmware reservations, dynamic placement windows, CPU release cells | Working DTB `/memory`, reservation block, `/reserved-memory`, `cpu-release-addr` |
| Peripheral windows | BCM2711 low-peripheral configuration and translated DT `/soc/ranges` |
| Linked image, CPU0 stack and EL2 table arena | [rpi.ld](../../scripts/rpi.ld) and the exact ELF's symbols |
| Live DTB and staged Linux source | `fdt=` / `image=` handoff and validated sizes, rounded to pages |
| Secondary stacks and heap | Prepared CPU identity and heap diagnostics from that boot |
| VM backing and stage-2 tables | Live `VmCtl` allocation records |
| Temporary U-Boot arena | Fresh `bdinfo`/LMB; relevant to safe deployment before takeover |

The example comes from the retained diagnostics boot using ELF SHA-256
`a87c7beea8f89cd41eea9490796ec1b7743fedd818d8282f55d41c34b7e668a2` and a
4,352,008-byte Linux Image. Local provenance, when retained, is in
`docs/evidence/diagnostics_regression_20261102_01/overnight/`: the manifest,
matching ELF and the fresh diagnostic invocation in `diagnostic-soak.uart.log`.
This is layout provenance, not a requirement to reproduce historical test results.

For the ELF you will actually load, regenerate the static boundaries:

```sh
elf=target/aarch64-unknown-none/debug/tvisor
llvm-nm -n "$elf" | rg '__image_(start|end)$|__text_(start|end)$|__vectors_(start|end)$|__rodata_(start|end)$|__writable_(start|end)$|__bss_(start|end)$|__boot_stack_(guard_start|bottom|top)$|__el2_stage1_tables_(start|end)$'
llvm-readelf -l "$elf"
```

Use a diagnostics build and its fresh UART segment to locate the runtime stacks,
heap and reservations. Revalidate the [deployment layout](../setup/raspberry-pi4.md#validate-the-live-layout)
before downloads; addresses and the size of the firmware carve-out can change.
The current allocator cannot manage RAM above its 4 GiB aperture.

Physical ownership and EL2 mapping permissions are separate: an unmapped guard
is still owned, a writable release-cell page remains firmware-reserved, and a
guest mapping does not transfer device ownership. See [memory policy](memory.md)
for those rules and [platform addressing](platform.md#bus-addresses-and-cpu-addresses)
for the distinction between CPU PA and DMA/bus addresses. The hardware reference
is the [BCM2711 peripheral manual](https://datasheets.raspberrypi.com/bcm2711/bcm2711-peripherals.pdf).
