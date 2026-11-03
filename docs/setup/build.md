# Build and configuration

Run commands from the repository root. The project is a Rust 2024 `no_std`
binary and utility library, with locked dependencies and a vendored dtoolkit.
Install a Rust toolchain supporting this edition and the bare-metal target:

```sh
rustup target add aarch64-unknown-none
cargo build --locked
cargo build --locked --release
```

[Cargo configuration](../../.cargo/config.toml) defaults to
`aarch64-unknown-none` and passes [the linker script](../../scripts/rpi.ld).
The build outputs are:

```text
target/aarch64-unknown-none/debug/tvisor
target/aarch64-unknown-none/release/tvisor
```

Use `--offline` with `--locked` when all locked dependencies are cached.
LLVM tools (`llvm-nm`, `llvm-readelf`, `llvm-objdump`) and Python 3 are required
for linked-image checks. Native unit tests need an explicit Linux host target;
see [host tests](../testing/host-tests.md).

## Build features

| Feature | Behavior |
|---|---|
| Default `baremetal` | Normal hypervisor; concise guest-entry message, active failure reports |
| `diagnostics` | Detailed CPU/resource/register/management checkpoints; no scratch workload |
| `secondary-startup-timeout-test` | Single-secondary delayed arrival; guest deliberately blocked |
| `all-core-timeout-test` | CPU2 delayed startup and partial-group preservation; guest blocked |
| `management-irq-test` | Masked EL1/EL2 IRQ, state preservation and HCR transition acceptance |
| `all-core-stress-test` | Concurrent allocator/heap/logging/management workload, also alongside Linux |
| `tlb-update-test` | Reserved scratch mappings, 32 Online and 16 Parked remaps |
| `tlb-update-timeout-test` | Delayed CPU2 quiescence acknowledgement; terminal failure and guest block |

Examples:

```sh
cargo build --locked --features diagnostics
cargo build --locked --release --features tlb-update-test
cargo build --locked --features tlb-update-timeout-test
```

Every hardware acceptance feature enables diagnostics automatically. Release
optimization alone enables neither verbose diagnostics nor scratch remaps.
Ordinary images reserve no scratch backing; boot validation, failure checkpoints,
warnings and panic reports remain enabled. Unit-test fixtures use `cfg(test)`
and are absent from bare-metal images.

Build acceptance features individually. `all-core-stress-test` and
`management-irq-test` are explicitly incompatible; `--all-features` is not a
supported configuration. Hardware tests require a matching runner mode and
fixture, as described in [hardware tests](../testing/hardware-tests.md).

## ELF placement

Tvisor is linked at physical `0x04000000`. Its entry address changes with the
build; derive it from symbols rather than copying an old serial log:

```sh
llvm-nm -n target/aarch64-unknown-none/debug/tvisor | rg ' main$|__image_(start|end)$|__el2_stage1_tables_(start|end)$'
llvm-readelf -l target/aarch64-unknown-none/debug/tvisor
```

Both downloaded file extent and complete runtime extent must fit the validated
board load window. The latter includes BSS, boot stack/guard and the 64 KiB
NOLOAD table arena. File size does not bound runtime writes. Do not assume the
ELF entry point equals the linked base or a previous build's entry point.
See [deployment checks](raspberry-pi4.md#validate-the-live-layout).

## Linux guest

[guest/Makefile](../../guest/Makefile) builds Linux 6.18.2 with a statically linked
BusyBox 1.38.0 initramfs. Prerequisites include curl, tar, make, Clang/LLVM/LLD,
an ARM64 GNU compiler/libc sysroot, fakeroot, cpio and gzip, plus kernel build
dependencies. The validated lab uses versioned LLVM tools selected by `LLVM=-22`.

```sh
make -C guest all LLVM=-22 JOBS=4
```

For unversioned LLVM tools use `LLVM=1`. The resulting kernel is:

```text
guest/build/arm64/arch/arm64/boot/Image
```

The initramfs is embedded in that Image. The normal guest contains a BusyBox
shell and virtual PL011 console; no SD-card root filesystem is needed.
Although kernel configuration enables SMP infrastructure, the generated DTB
exposes exactly one vCPU. A separate diagnostic configuration used by hardware
regression enables the probe and inspection facilities.

During hypervisor regression, reuse a verified frozen Linux fixture. Rebuilding
the kernel or changing its probe/configuration changes the comparison baseline.
The [hardware guide](../testing/hardware-tests.md#prepare-a-fixture) explains
initial fixture preparation and later reuse. Building tvisor alone neither
builds nor downloads Linux.
