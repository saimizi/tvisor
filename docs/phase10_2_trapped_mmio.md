# Phase 10.2: trapped-MMIO PL011 earlycon

The virtual PL011 is at guest IPA `0x0900_0000..0x0900_1000`. It is deliberately
absent from the stage-2 RAM map, so guest accesses cause a lower-EL stage-2 Data
Abort at EL2; it is never mapped to the Raspberry Pi Mini UART.

`tvisor_util::mmio` first decodes `ESR_EL2` instruction-syndrome validity,
direction, access width, target register, and register width. `MmioDispatcher`
then dispatches the IPA to `VirtualPl011`.

Supported earlycon behavior:

- `UARTDR` (`0x00`) writes of 1, 2, or 4 bytes return the low byte for the EL2
  caller to forward to tvisor's Mini UART console;
- `UARTFR` (`0x18`) reads report TX-ready (`TXFF` clear);
- early initialization writes to `IBRD`, `FBRD`, `LCR_H`, `CR`, `IMSC`, and
  `ICR` are safely accepted without exposing host UART state.

Unsupported offsets, reads, writes, widths, sign-extending loads, and IPAs fail
explicitly. The EL2 caller invokes `VcpuContext::emulate_stage2_mmio()`; that
method writes read values into the named guest register and increments
`ELR_EL2` by four only after a successful device operation. A returned TX byte
is then forwarded to the host console by the guest-run loop.

Normal PL011 probe, identifier registers, RX, and virtual UART interrupts stay
deferred to Phase 10.5.
