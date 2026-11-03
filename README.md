# tvisor

Tvisor is an experimental, educational AArch64 hypervisor written in Rust for
the Raspberry Pi 4 Model B. U-Boot launches it at EL2; tvisor takes ownership
of the machine and runs a single-vCPU Linux guest with 512 MiB of RAM on CPU0.
All four physical CPUs enter tvisor-owned EL2 contexts. Secondary CPUs provide
host management services and remain responsive while parked.

The implementation includes Device Tree discovery, guarded per-CPU stacks,
shared physical and heap allocation, management SGIs, guest stage-2 translation,
a virtual PL011 console, GICv2 timer delivery, and synchronized EL2 scratch-page
updates in acceptance builds. Ordinary images keep EL2 mappings frozen and do
not run scratch-remapping workloads.

## Build and run

Install Rust with the bare-metal target, then build from the repository root:

```sh
rustup target add aarch64-unknown-none
cargo build --locked
```

The ELF is `target/aarch64-unknown-none/debug/tvisor`. Run native library tests
using the installed compiler's host target:

```sh
host_target=$(rustc -vV | sed -n 's/^host: //p')
cargo test --locked --lib --target "$host_target"
```

Use the [build guide](docs/setup/build.md) for release/diagnostic configurations
and the Linux guest build. Follow the [Raspberry Pi guide](docs/setup/raspberry-pi4.md)
to validate load addresses, load both the ELF and Linux `Image`, and launch with
the required `fdt=` and `image=<address>,<file-size>` arguments. Every run needs
a fresh U-Boot boot; there is no return path to its prompt.

## Documentation

- [Architecture and boot sequence](docs/architecture/overview.md)
- [Physical CPUs and secondary startup](docs/architecture/cpu.md)
- [Memory ownership, mappings and TLB synchronization](docs/architecture/memory.md)
- [Interrupt routing and management](docs/architecture/interrupts.md)
- [VMs, vCPUs and the Linux guest platform](docs/architecture/virtualization.md)
- [Platform discovery and firmware contracts](docs/architecture/platform.md)
- [Build and configuration](docs/setup/build.md)
- [Raspberry Pi setup and execution](docs/setup/raspberry-pi4.md)
- [Host tests and linked-image checks](docs/testing/host-tests.md)
- [Hardware acceptance and regression](docs/testing/hardware-tests.md)
- [Development plan and current status](docs/development_plan.md)

## Current limitations

The supported startup backend is the BCM2711 Cortex-A72 armstub spin-table
interface. Physical PSCI startup, hotplug and other boards are unsupported.
The guest has one vCPU: guest multiprocessing, guest PSCI CPU_ON, virtual IPIs,
vCPU scheduling/migration and shared stage-2 update coordination are future work.
Device emulation covers the current Linux console/timer platform, not a complete
GIC or PL011 implementation. Common IRQ dispatch, backpressure and teardown
handling still have [documented gaps](docs/architecture/interrupts.md#remaining-work).

## License

Tvisor is distributed under either the [MIT License](LICENSE-MIT) or the
[Apache License 2.0](LICENSE-APACHE), at your option. Components under
`third_party/` retain their respective licenses.
