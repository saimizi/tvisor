# tvisor

Tvisor is an experimental, educational AArch64 hypervisor for the Raspberry
Pi 4. It is written in Rust as a `no_std` bare-metal program and is currently
launched from U-Boot at EL2.

The project is building the execution environment needed to take control from
U-Boot and eventually run guest virtual machines. The current implementation:

- validates the architectural state inherited from U-Boot;
- discovers the Raspberry Pi platform from the working Device Tree Blob
  (DTB);
- initializes the DTB-selected mini UART for diagnostics;
- records RAM, firmware reservations, MMIO windows, CPUs, and console data;
- distinguishes permanent reservations from temporary U-Boot handoff memory;
- derives pre-takeover and post-takeover usable-RAM maps without a heap; and
- installs a private stack, exception vectors, and EL2 stage-1 page tables;
- reclaims eligible U-Boot RAM through a global physical-page allocator; and
- initializes a wrapped, fixed-size Rust heap after takeover.

Tvisor does not run a guest VM yet. After validating the handoff, it takes
ownership of EL2 and does not return to U-Boot.

## Requirements

The host development machine needs:

- Rust with the `aarch64-unknown-none` target;
- an AArch64-capable `nm` implementation;
- a TFTP server reachable by the Raspberry Pi; and
- a serial connection to the Raspberry Pi 4 U-Boot console.

Install the Rust target with:

```sh
rustup target add aarch64-unknown-none
```

The Raspberry Pi must boot through a U-Boot configuration that enters tvisor
at EL2 and provides a working DTB through the `fdt_addr` environment variable.

## Build and host tests

Run the host unit tests:

```sh
cargo test-host
```

Build the debug ELF for the bare-metal AArch64 target:

```sh
cargo build --target aarch64-unknown-none
```

The resulting ELF is:

```text
target/aarch64-unknown-none/debug/tvisor
```

The linker script [scripts/rpi.ld](scripts/rpi.ld) currently links tvisor at
physical address `0x0400_0000`. Source changes can move the `main` entry point,
so determine it from every new build:

```sh
nm -n target/aarch64-unknown-none/debug/tvisor | grep ' main$'
```

## Test on Raspberry Pi 4

The following procedure uses U-Boot's `go` command. Replace addresses and
network settings with values verified for the current board and boot session.

### 1. Publish the ELF through TFTP

For a TFTP root at `/srv/tftp`:

```sh
sudo cp target/aarch64-unknown-none/debug/tvisor /srv/tftp/tvisor
```

### 2. Enter a fresh U-Boot session

Reset the board before each tvisor run and interrupt the autoboot countdown.
Repeatedly running tvisor without resetting U-Boot first is not a supported
test sequence.

At the prompt, inspect the current platform and U-Boot allocation state:

```text
U-Boot> bdinfo
U-Boot> printenv fdt_addr
```

Do not assume that LMB, DTB, stack, or relocated U-Boot addresses remain the
same after changing firmware, U-Boot, the DTB, or the boot sequence.

### 3. Download tvisor

The direct `go` workflow downloads the ELF at its linked base:

```text
U-Boot> setenv autostart no
U-Boot> tftpboot 0x04000000 <tftp-server-ip>:tvisor
```

U-Boot sets `filesize` to the downloaded file size.

### 4. Run tvisor

Use the `main` address reported by `nm`:

```text
U-Boot> go <main-address> fdt=${fdt_addr}
```

For a more accurate pre-takeover safety map, also pass the download buffer and
every LMB reservation reported by the immediately preceding `bdinfo`:

```text
U-Boot> go <main-address> \
    fdt=${fdt_addr} \
    bootmem=4000000:${filesize} \
    lmb=<start0>:<size0> \
    lmb=<start1>:<size1>
```

The backslashes above show logical line wrapping; enter the command as one
line in U-Boot. Addresses and sizes are hexadecimal without a required `0x`
prefix.

`lmb=` and `bootmem=` describe temporary U-Boot ownership. They refine the
`INITIAL` map but never reduce the final post-takeover `USABLE` map.

Tvisor always installs its own EL2 stage-1 tables. An optional `fault=`
argument selects a post-switch test:

```text
fault=none       no deliberate fault (default)
fault=sync       execute BRK #0x600 and return through the EL2 handler
fault=guard      write the unmapped stack guard and halt in the handler
fault=unmapped   read an unmapped virtual address and halt in the handler
```

### 5. Check the result

A successful run should print:

- DTB version, size, and board model;
- discovered RAM, reservations, MMIO translations, CPUs, and console;
- normalized `RESERVED`, `HANDOFF`, `INITIAL`, and `USABLE` regions;
- inherited EL2 register state; and
- the private-environment checkpoints:

```text
Phase 7 checkpoint 2: tvisor EL2 page tables active
Phase 7 checkpoint 3: register, stack, and image validation passed
Phase 7 checkpoint complete; halting
```

The final halt is intentional. Reset or power-cycle the board before another
test; there is no return path to the U-Boot prompt.

## Documentation

- [Development plan](docs/development_plan.md)
- [System-information model](docs/system_information_model.md)
- [U-Boot Raspberry Pi 4 memory layout](docs/uboot_rpi4_memory.md)
- [Address translation](docs/address_translation.md)
- [Draft tvisor EL2 memory map](docs/tvisor_memory_map.md)
- [Private EL2 foundations](docs/private_el2_foundations.md)
- [EL2 page-table transition](docs/el2_page_table_transition.md)
- [U-Boot handoff-state diagnostics](docs/check_handoff_state.md)
- [Peripheral address translation](docs/peripheral_address_translation.md)

## License

Tvisor is distributed under the terms of either the
[MIT License](LICENSE-MIT) or the
[Apache License 2.0](LICENSE-APACHE), at your option.

Third-party components under `third_party/` retain their respective licenses.
