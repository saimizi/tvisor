# Peripheral address translation on Raspberry Pi 4

## 1. Why one peripheral has multiple addresses

BCM2711 components do not all use one universal address space. The ARM CPUs,
VideoCore, DMA engines, and internal buses can use different address values for
the same hardware register.

For example, the mini-UART transmit register can be described by both of these
addresses:

```text
Legacy peripheral-bus address: 0x7E21_5040
ARM physical address:          0xFE21_5040
```

Both identify the same register, but from different address spaces. A device
tree node below `/soc` uses the `/soc` bus address:

```text
/soc/serial@7e215040
```

Code executing on the Cortex-A72 uses the translated ARM physical address for
MMIO. For tvisor's mini-UART driver, that address is `0xFE21_5040`.

## 2. Device Tree address hierarchy

A Device Tree is a hierarchy of buses and devices. A device's `reg` property
and the address in its node name are expressed in the address space of its
immediate parent bus—not necessarily in the root ARM physical address space.

A bus node can provide a `ranges` property to translate its child addresses
into its parent's address space. Translation continues through parent buses
until the root physical address space is reached:

```text
device reg address
       │
       ▼
immediate parent bus address
       │  parent ranges
       ▼
next parent address
       │
       ▼
root ARM physical address
```

Three properties define the encoding:

- The child bus's `#address-cells` gives the number of 32-bit cells used for a
  child address.
- The parent bus's `#address-cells` gives the number of 32-bit cells used for
  the translated parent address.
- The child bus's `#size-cells` gives the number of 32-bit cells used for the
  range size.

Each cell is a 32-bit big-endian value in the flattened device tree. U-Boot
prints the decoded cells as hexadecimal words.

## 3. BCM2711 `/soc/ranges`

The captured Raspberry Pi 4 device tree reports:

```text
ranges = <
    0x7e000000 0x00000000 0xfe000000 0x01800000
    0x7c000000 0x00000000 0xfc000000 0x02000000
    0x40000000 0x00000000 0xff800000 0x00800000
>;
```

For this tree, `/soc` uses one address cell and one size cell, while its parent
uses two address cells:

```text
/soc #address-cells:  1
root #address-cells:  2
/soc #size-cells:     1
```

One mapping therefore occupies four cells:

```text
<child-address parent-address-high parent-address-low size>
```

The property contains 12 cells, so it contains three mappings.

## 4. Decoding the mappings

### 4.1 Main peripherals starting at child `0x7E00_0000`

The first four cells are:

```text
child base:       0x7E00_0000
parent high:      0x0000_0000
parent low:       0xFE00_0000
parent base:      0x0000_0000_FE00_0000
size:             0x0180_0000
```

This mapping is:

```text
/soc child address                 ARM physical address
0x7E00_0000–0x7F7F_FFFF     →     0xFE00_0000–0xFF7F_FFFF
```

### 4.2 Main peripherals starting at child `0x7C00_0000`

The second four cells are:

```text
child base:       0x7C00_0000
parent high:      0x0000_0000
parent low:       0xFC00_0000
parent base:      0x0000_0000_FC00_0000
size:             0x0200_0000
```

This mapping is:

```text
/soc child address                 ARM physical address
0x7C00_0000–0x7DFF_FFFF     →     0xFC00_0000–0xFDFF_FFFF
```

Together, the first two mappings form the BCM2711 main-peripheral window in the
ARM physical address space:

```text
0xFC00_0000–0xFF7F_FFFF
```

### 4.3 ARM-local peripherals

The final four cells are:

```text
child base:       0x4000_0000
parent high:      0x0000_0000
parent low:       0xFF80_0000
parent base:      0x0000_0000_FF80_0000
size:             0x0080_0000
```

This mapping is:

```text
/soc child address                 ARM physical address
0x4000_0000–0x407F_FFFF     →     0xFF80_0000–0xFFFF_FFFF
```

It covers the ARM-local peripheral window, including the ARM-local interrupt
registers and GIC region.

### 4.4 Summary

| `/soc` child range | ARM physical range | Size |
| --- | --- | ---: |
| `0x7C00_0000–0x7DFF_FFFF` | `0xFC00_0000–0xFDFF_FFFF` | 32 MiB |
| `0x7E00_0000–0x7F7F_FFFF` | `0xFE00_0000–0xFF7F_FFFF` | 24 MiB |
| `0x4000_0000–0x407F_FFFF` | `0xFF80_0000–0xFFFF_FFFF` | 8 MiB |

## 5. Translation formula

First find a range for which the child address satisfies:

```text
child_base <= child_address < child_base + size
```

Then preserve the offset within that range:

```text
offset         = child_address - child_base
parent_address = parent_base + offset
```

The upper bound is exclusive. An address equal to `child_base + size` is not
part of that mapping.

## 6. Worked mini-UART example

The device tree selects this UART for the serial console:

```text
stdout-path = "serial0:115200n8"
serial0     = "/soc/serial@7e215040"
uart1       = "/soc/serial@7e215040"
```

The child address `0x7E21_5040` belongs to the first mapping:

```text
child range = [0x7E00_0000, 0x7F80_0000)
```

Calculate its offset:

```text
0x7E21_5040 - 0x7E00_0000 = 0x0021_5040
```

Add that offset to the parent base:

```text
0xFE00_0000 + 0x0021_5040 = 0xFE21_5040
```

Therefore:

```text
/soc/serial@7e215040
           │
           │ /soc ranges translation
           ▼
ARM physical address 0xFE21_5040
```

This is the mini-UART I/O register used for transmit and receive data.

Other registers use the same translation:

| Register | `/soc` bus address | ARM physical address |
| --- | ---: | ---: |
| AUX base | `0x7E21_5000` | `0xFE21_5000` |
| Mini-UART I/O | `0x7E21_5040` | `0xFE21_5040` |
| Mini-UART line status | `0x7E21_5054` | `0xFE21_5054` |
| PL011 UART0 | `0x7E20_1000` | `0xFE20_1000` |
| GPIO base | `0x7E20_0000` | `0xFE20_0000` |

For addresses in this particular range, the result can also be recognized as:

```text
ARM physical address = /soc address + 0x8000_0000
```

That shortcut is not a general Device Tree rule. It works here only because of
the specific bases in this `/soc/ranges` entry. Code that parses a Device Tree
must use `ranges`, not a hard-coded addition.

## 7. Physical and virtual addresses

The `/soc/ranges` translation produces an ARM physical address. It does not
directly produce the virtual address used by a load or store instruction when
the MMU is enabled.

### MMU disabled

When EL2 stage-1 translation is disabled with `SCTLR_EL2.M == 0`, addresses are
flat-mapped for that stage:

```text
EL2 address 0xFE21_5040 → physical address 0xFE21_5040
```

### MMU enabled

When `SCTLR_EL2.M == 1`, the EL2 page tables translate the virtual address to a
physical address:

```text
EL2 virtual address
        │ TTBR0_EL2/TCR_EL2 translation tables
        ▼
ARM physical address 0xFE21_5040
        │ BCM2711 interconnect
        ▼
mini-UART register
```

U-Boot may identity-map the peripheral, in which case the virtual and physical
numbers are both `0xFE21_5040`, but tvisor must verify that mapping rather than
assume it. When tvisor installs its own tables, it may choose another virtual
address as long as the mapping ends at physical address `0xFE21_5040`.

UART MMIO must be mapped with a Device memory attribute, not as cacheable Normal
memory.

## 8. DMA addresses are a separate concern

A peripheral or DMA engine can have another view of RAM and peripherals. An
ARM physical address must not automatically be placed into a DMA descriptor.
DMA addressing can involve legacy aliases, paging registers, limited address
widths, and coherency rules specific to the DMA engine.

The translation in this document answers this question:

> Which ARM physical address corresponds to a device described below `/soc`?

It does not define a general ARM-to-DMA address conversion.

## 9. Inspecting the live Device Tree in U-Boot

Useful commands include:

```text
U-Boot> fdt addr ${fdtcontroladdr}
U-Boot> fdt print / '#address-cells'
U-Boot> fdt print /soc '#address-cells'
U-Boot> fdt print /soc '#size-cells'
U-Boot> fdt print /soc ranges
U-Boot> fdt print /chosen stdout-path
U-Boot> fdt print /aliases
```

When decoding another bus:

1. Read the child bus's `#address-cells` and `#size-cells`.
2. Read the parent bus's `#address-cells`.
3. Divide `ranges` into tuples using those cell counts.
4. Find the tuple containing the device's child address.
5. Preserve its offset and add that offset to the parent base.
6. Repeat at the next parent if it is not the root bus.
7. Treat the final result as a physical address, then separately determine the
   virtual mapping used by the current MMU configuration.

## 10. References

- [BCM2711 ARM Peripherals](https://datasheets.raspberrypi.com/bcm2711/bcm2711-peripherals.pdf)
- [Devicetree Specification](https://www.devicetree.org/specifications/)
- [Arm memory-management guide](https://developer.arm.com/-/media/Arm%20Developer%20Community/PDF/LearnTheArchitecture-MemoryManagement-101811_0100_00_en.pdf)
- [UART support design for Raspberry Pi 4](uart_rpi4_design.md)
- [U-Boot memory layout on Raspberry Pi 4](uboot_rpi4_memory.md)
