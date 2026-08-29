# UART support design for Raspberry Pi 4

## 1. Purpose

This document designs the first UART milestone for tvisor on Raspberry Pi 4.
It is intentionally a diagnostic milestone rather than the final hypervisor
handoff.

For this milestone, tvisor is loaded with U-Boot `bootelf`, executes at EL2,
writes a short diagnostic through the UART already configured by U-Boot, and
returns to U-Boot. It must not take ownership of architectural state that
U-Boot still needs.

The design has four goals:

1. Verify the architectural state inherited from U-Boot instead of assuming
   that the MMU or caches are enabled or disabled.
2. Reuse the working serial connection without changing its clock, baud rate,
   GPIO routing, FIFO configuration, or interrupt configuration.
3. Provide polling transmit support with a bounded failure path.
4. Preserve a clear upgrade path to a non-returning hypervisor that owns its
   stack, exception vectors, MMU, page tables, and UART.

Receive support, interrupt-driven UART operation, PL011 support, UART
initialization, and permanent EL2 takeover are outside this milestone.

## 2. Boot and ownership contract

The current development flow is:

```text
U-Boot> tftpboot 0x02000000 <server>:tvisor
U-Boot> bootelf 0x02000000
```

The ELF container is staged at `0x0200_0000`; `bootelf` loads its segments at
the linked address `0x0400_0000` and calls its entry point.

Because tvisor returns from this diagnostic, the following state remains owned
by U-Boot:

- the current stack and stack contents;
- EL2 stage-1 translation tables and MMU configuration;
- cache configuration and cache contents;
- `VBAR_EL2` and U-Boot's exception handlers;
- interrupt masks and interrupt-controller configuration;
- GPIO14/GPIO15 pin multiplexing;
- UART enable, clock, baud rate, framing, FIFO, and interrupt configuration.

The diagnostic may read this state but must not replace or reconfigure it. In
particular, discovering `SCTLR_EL2.M == 1` is not permission to disable the MMU.
Doing so safely requires cache maintenance, barriers, identity-mapped
transition code, and a one-way ownership contract.

## 3. UART selection

BCM2711 contains one mini UART (`UART1`) and five PL011 UARTs (`UART0` and
`UART2` through `UART5`). On a normal Raspberry Pi 4 configuration with
Bluetooth enabled, the primary serial interface on GPIO14/GPIO15 is the mini
UART and PL011 UART0 is assigned to Bluetooth. UART2 through UART5 are disabled
by default.

The first tvisor UART backend therefore targets the mini UART:

| Property | Selection |
| --- | --- |
| Controller | BCM2711 AUX mini UART (`UART1`) |
| TX pin | GPIO14, header pin 8 |
| RX pin | GPIO15, header pin 10 |
| Electrical level | 3.3 V only |
| Expected framing | 115200 baud, 8 data bits, no parity, 1 stop bit |
| Operation | Polling transmit only |
| Initialization owner | Raspberry Pi firmware and U-Boot |

The firmware configuration must contain `enable_uart=1`. This both enables the
serial interface and provides the stable VPU core clock needed by the mini
UART's baud-rate generator.

Before relying on this selection, confirm the active console in U-Boot:

```text
U-Boot> printenv stdout
U-Boot> fdt addr ${fdtcontroladdr}
U-Boot> fdt print /chosen stdout-path
U-Boot> fdt print /aliases
```

If the board routes PL011 to GPIO14/GPIO15 using an overlay such as
`disable-bt`, this mini-UART design does not apply without changing the backend.

## 4. BCM2711 mini-UART register interface

The BCM2711 datasheet describes the AUX block at legacy bus address
`0x7E21_5000`. With the normal Low Peripheral mapping, the ARM sees it at
physical address `0xFE21_5000`:

```text
Legacy AUX base:       0x7E21_5000
ARM physical AUX base: 0xFE21_5000
```

The AUX register map relevant to the mini UART is:

| Offset | ARM physical address | Register | Purpose |
| --- | --- | --- | --- |
| `0x00` | `0xFE21_5000` | `AUX_IRQ` | AUX interrupt status |
| `0x04` | `0xFE21_5004` | `AUX_ENABLES` | Enables mini UART with bit 0 |
| `0x40` | `0xFE21_5040` | `AUX_MU_IO_REG` | Receive/transmit data |
| `0x44` | `0xFE21_5044` | `AUX_MU_IER_REG` | Interrupt enables |
| `0x48` | `0xFE21_5048` | `AUX_MU_IIR_REG` | Interrupt status and FIFO control |
| `0x4C` | `0xFE21_504C` | `AUX_MU_LCR_REG` | Data size and line control |
| `0x50` | `0xFE21_5050` | `AUX_MU_MCR_REG` | Modem control |
| `0x54` | `0xFE21_5054` | `AUX_MU_LSR_REG` | Line status |
| `0x58` | `0xFE21_5058` | `AUX_MU_MSR_REG` | Modem status |
| `0x5C` | `0xFE21_505C` | `AUX_MU_SCRATCH` | Scratch register |
| `0x60` | `0xFE21_5060` | `AUX_MU_CNTL_REG` | Receiver/transmitter control |
| `0x64` | `0xFE21_5064` | `AUX_MU_STAT_REG` | Extended status |
| `0x68` | `0xFE21_5068` | `AUX_MU_BAUD_REG` | Baud-rate divisor |

Only two registers are needed by the returnable transmit path:

- `AUX_MU_LSR_REG` bit 5: when set, the transmitter can accept at least one
  byte.
- `AUX_MU_LSR_REG` bit 6: when set, the transmitter is idle and all queued
  transmission has completed.
- `AUX_MU_IO_REG` bits `[7:0]`: the byte placed into the transmit FIFO.

All MMIO accesses must be volatile. A normal Rust reference is not appropriate
for device registers because the compiler may combine, cache, remove, or reorder
ordinary memory operations.

### 4.1 Transmit algorithm

For each output byte:

1. Poll `AUX_MU_LSR_REG` bit 5.
2. If the bit becomes one, write the byte to `AUX_MU_IO_REG` and return
   success.
3. If the bit is still zero after `1_000_000` polls, return a timeout error.

For text output, emit carriage return followed by line feed for each `\n`:

```text
'\n' -> '\r', '\n'
```

Before returning control to U-Boot, poll `AUX_MU_LSR_REG` bit 6 with the same
`1_000_000`-poll limit. This prevents the final diagnostic bytes from remaining
in flight when U-Boot resumes use of the controller.

This milestone must not write `AUX_ENABLES`, `AUX_MU_IER_REG`,
`AUX_MU_IIR_REG`, `AUX_MU_LCR_REG`, `AUX_MU_CNTL_REG`, or
`AUX_MU_BAUD_REG`. Reinitializing a borrowed controller could change or destroy
U-Boot's console configuration.

## 5. Inherited AArch64 state

`CurrentEL == 2` proves only that tvisor is executing at EL2. It says nothing
about whether EL2 stage-1 translation, data caching, or instruction caching is
enabled. Those controls must be read separately.

The following register reference supports incremental diagnostics as tvisor
grows. The first UART milestone reads `CurrentEL`; later diagnostics can print
the other values through UART. The field descriptions intentionally cover the
Cortex-A72/Armv8-A fields relevant to tvisor bring-up, rather than every
extension added in later versions of the architecture.

### 5.1 `CurrentEL` — Current Exception Level

`CurrentEL` is a read-only status register.

| Bits | Field | Meaning |
| --- | --- | --- |
| `[3:2]` | `EL` | `00`: EL0, `01`: EL1, `10`: EL2, `11`: EL3 |
| Other bits | Reserved | Read as zero |

It is read with `MRS Xt, CurrentEL`. Software shifts the value right by two and
masks two bits. tvisor requires the decoded value to be 2.

### 5.2 `SCTLR_EL2` — System Control Register at EL2

`SCTLR_EL2` controls EL2 stage-1 translation and several EL2 execution
properties.

| Bit | Field | Relevant meaning |
| --- | --- | --- |
| 0 | `M` | Enables EL2 stage-1 address translation |
| 1 | `A` | Enables alignment-fault checking |
| 2 | `C` | Enables data and unified caches for EL2 accesses |
| 3 | `SA` | Enables stack-alignment checking at EL2 |
| 12 | `I` | Enables instruction caching at EL2 |
| 19 | `WXN` | Treats writable mappings as execute-never |
| 25 | `EE` | Selects EL2 data endianness; zero is little-endian |

`M`, `C`, and `I` are independent. For example, observing `M == 0` does not by
itself prove that both caches are disabled. When EL2 stage-1 translation is
disabled, addresses are flat-mapped for that translation stage, so the input
address is also the output physical address.

For this milestone, tvisor records `SCTLR_EL2` but never writes it.

### 5.3 `HCR_EL2` — Hypervisor Configuration Register

`HCR_EL2` controls virtualization behavior for execution below EL2. It is
different from `SCTLR_EL2`: its `VM` bit controls stage-2 translation for
EL1/EL0, not EL2's own stage-1 translation.

| Bit | Field | Relevant meaning |
| --- | --- | --- |
| 0 | `VM` | Enables stage-2 translation for EL1/EL0 |
| 3 | `FMO` | Routes physical FIQ exceptions to EL2 |
| 4 | `IMO` | Routes physical IRQ exceptions to EL2 |
| 5 | `AMO` | Routes physical SError exceptions to EL2 |
| 13 | `TWI` | Traps EL1 `WFI` instructions to EL2 |
| 14 | `TWE` | Traps EL1 `WFE` instructions to EL2 |
| 27 | `TGE` | Routes general exceptions from EL1 to EL2 |
| 31 | `RW` | Selects AArch64 (`1`) or AArch32 (`0`) for EL1 |

Later, a functioning hypervisor will deliberately configure this register.
The returnable diagnostic only records the inherited value.

### 5.4 `TCR_EL2` — Translation Control Register at EL2

For the non-VHE EL2 translation regime used by Cortex-A72, `TCR_EL2` describes
the virtual-address size and the translation tables rooted at `TTBR0_EL2`.

| Bits | Field | Relevant meaning |
| --- | --- | --- |
| `[5:0]` | `T0SZ` | Input address size is `64 - T0SZ` bits |
| `[9:8]` | `IRGN0` | Inner cacheability of table walks |
| `[11:10]` | `ORGN0` | Outer cacheability of table walks |
| `[13:12]` | `SH0` | Shareability of table walks |
| `[15:14]` | `TG0` | Translation granule: 4 KiB, 64 KiB, or 16 KiB |
| `[18:16]` | `PS` | Physical address size used by the translation regime |
| 20 | `TBI` | Ignores the top byte during applicable address translation |

For `TG0`, encoding `00` selects 4 KiB, `01` selects 64 KiB, and `10` selects
16 KiB. Reserved encodings and unsupported physical sizes must never be
programmed. The implemented physical address range must ultimately be checked
through the processor feature registers, not inferred only from this inherited
value.

The fields affect translation-table interpretation when EL2 translation is
enabled. tvisor records them now and will choose its own values only during the
permanent handoff milestone.

### 5.5 `TTBR0_EL2` — Translation Table Base Register 0 at EL2

`TTBR0_EL2` identifies the root table for EL2 stage-1 translation. Its base
address must have the alignment required by the selected granule, virtual
address size, and starting table level. Low bits that are not part of the base
address are reserved or feature-dependent, such as `CnP` when that feature is
implemented.

A diagnostic should record the complete raw value. It must not assume that
masking with a fixed constant always produces a valid root-table address across
all Arm architecture extensions. For the Cortex-A72 configuration, decode the
base consistently with `TCR_EL2` and the implemented physical-address size.

`TTBR0_EL2` is not evidence that translation is active; `SCTLR_EL2.M` is the
enable control.

### 5.6 `MAIR_EL2` — Memory Attribute Indirection Register at EL2

`MAIR_EL2` contains eight 8-bit memory-attribute encodings:

```text
Attr0 = bits [7:0]
Attr1 = bits [15:8]
...
Attr7 = bits [63:56]
```

An EL2 stage-1 page-table descriptor contains an `AttrIndx` field selecting one
of these entries. Common encodings include:

| Encoding | Memory type |
| --- | --- |
| `0x00` | Device-nGnRnE |
| `0x04` | Device-nGnRE |
| `0x44` | Normal, outer and inner non-cacheable |
| `0xFF` | Normal, outer and inner write-back, read/write allocate |

UART MMIO must eventually be mapped as Device memory, while ordinary RAM should
normally be mapped as cacheable Normal memory. Mapping MMIO as Normal memory
can allow unsafe speculation, merging, or reordering.

For this returnable milestone, tvisor records `MAIR_EL2` and relies on U-Boot's
existing mapping.

### 5.7 `VBAR_EL2` — Vector Base Address Register at EL2

`VBAR_EL2` holds the base address of the EL2 exception vector table. An
AArch64 vector table occupies 2048 bytes and its base must be aligned to 2048
bytes, so the low 11 address bits are zero in the Cortex-A72 configuration.

Changing `VBAR_EL2` before a complete, correctly aligned vector table exists
would make synchronous exceptions, IRQs, FIQs, and SError exceptions unsafe.
The returnable diagnostic records the inherited U-Boot value and does not
replace it.

### 5.8 `DAIF` — Interrupt Mask Bits in PSTATE

`DAIF` exposes four exception-mask bits from `PSTATE`:

| Bit | Field | When set to 1 |
| --- | --- | --- |
| 9 | `D` | Debug exceptions are masked |
| 8 | `A` | SError exceptions are masked |
| 7 | `I` | IRQ exceptions are masked |
| 6 | `F` | FIQ exceptions are masked |

It can be read with `MRS Xt, DAIF`. `DAIFSet` and `DAIFClr` modify selected
masks, but the diagnostic does not use them because it must return with U-Boot's
masking state unchanged.

### 5.9 `SP` and `SPSel` — Current Stack Pointer Selection

`SP` is the current stack pointer, not a conventional system register read by
`MRS`. Its value is copied with an instruction such as `MOV Xt, SP`.

`SPSel` bit 0 determines which physical stack pointer the `SP` name selects:

| `SPSel.SP` | Current `SP` at EL2 |
| --- | --- |
| 0 | `SP_EL0` (EL2t execution) |
| 1 | `SP_EL2` (EL2h execution) |

The stack must be 16-byte aligned at an AArch64 public interface. Because this
milestone returns to `bootelf`, it continues using U-Boot's current stack and
does not change `SP`, `SP_EL0`, `SP_EL2`, or `SPSel`.

## 6. Error reporting

The original returnable diagnostic stored byte-sized error codes in a private
`.bss` stack for later inspection from U-Boot. Tvisor now performs an
unconditional no-return takeover, and no consumer reads that private state, so
the error stack has been removed.

After UART initialization, failures are reported directly through the serial
console and tvisor halts. Failures before UART discovery halt silently. A
future persistent crash record, if needed, should use an explicitly reserved
region with a documented format and lifetime.

## 7. Ownership by milestone

| State or device | Returnable UART diagnostic | Permanent hypervisor |
| --- | --- | --- |
| Current U-Boot stack | Use without replacement | Replace with tvisor stack |
| `SCTLR_EL2` | Read only | Configure with cache-safe transition |
| `HCR_EL2` | Read only | Configure virtualization and stage 2 |
| `TCR_EL2` / `TTBR0_EL2` / `MAIR_EL2` | Read only | Install tvisor EL2 stage-1 tables |
| `VBAR_EL2` | Read only | Install tvisor vector table |
| `DAIF` | Read only | Mask/unmask according to exception setup |
| UART and GPIO configuration | Reuse U-Boot setup | Initialize and own explicitly |
| UART operation | Polling TX | Initially polling, later interrupt driven |
| Return to U-Boot | Required | Never |

## 8. Validation

The returnable UART milestone is successful when all of the following hold:

1. U-Boot identifies the external console as the mini UART on GPIO14/GPIO15.
2. `bootelf 0x02000000` enters tvisor at EL2.
3. The diagnostic appears correctly at 115200 8N1, including CRLF newlines.
4. The printed EL is 2.
5. The DTB-selected console path resolves to the expected compatible device and CPU physical address.
6. The final byte drains before tvisor returns.
7. U-Boot reports a zero return status and its prompt and serial input/output
   continue to work.
8. A pre-UART discovery failure returns one of the documented early status codes without MMIO access.
9. An unavailable transmitter times out instead of polling forever.

## 9. Future permanent-handoff sequence

The later non-returning hypervisor milestone should proceed in this order:

1. Enter through a dedicated assembly entry point and preserve required boot
   arguments.
2. Mask exceptions for the transition and install a tvisor-owned aligned stack.
3. Clear `.bss` and establish known runtime invariants.
4. Install a complete EL2 vector table and set `VBAR_EL2`.
5. Perform the required cache cleaning, invalidation, barriers, and TLB
   maintenance before replacing inherited translation state.
6. Build EL2 stage-1 tables that map RAM as Normal cacheable memory and UART
   MMIO as Device memory.
7. Program `MAIR_EL2`, `TCR_EL2`, and `TTBR0_EL2`, then enable the intended
   `SCTLR_EL2` configuration using the required barriers.
8. Initialize GPIO and UART independently of U-Boot.
9. Configure `HCR_EL2`, guest stage-2 translation, interrupts, and the
   non-returning hypervisor main loop.

## 10. References

- [BCM2711 ARM Peripherals](https://datasheets.raspberrypi.com/bcm2711/bcm2711-peripherals.pdf)
- [Raspberry Pi UART configuration](https://www.raspberrypi.com/documentation/configuration/computers/raspberry-pi.html#configure-uarts)
- [Arm A-profile AArch64 register reference](https://developer.arm.com/documentation/ddi0601/latest)
- [Arm memory-management guide](https://developer.arm.com/-/media/Arm%20Developer%20Community/PDF/LearnTheArchitecture-MemoryManagement-101811_0100_00_en.pdf)
- [Arm exception-model guide](https://developer.arm.com/-/media/Arm%20Developer%20Community/PDF/Learn%20the%20Architecture/Exception%20model.pdf)
