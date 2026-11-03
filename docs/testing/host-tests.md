# Host tests and linked-image checks

Run commands from the repository root. Tests of parsers, layouts, allocation
and synchronization protocols run on the host; privileged CPU execution and
coherency require [hardware validation](hardware-tests.md).

## Native tests

The default Cargo target is bare metal and has no Rust test harness. Select
the installed compiler's native target explicitly:

```sh
host_target=$(rustc -vV | sed -n 's/^host: //p')
cargo fmt --all -- --check
cargo test --locked --lib --target "$host_target"
cargo test --locked --lib --target "$host_target" --release
cargo test --locked --lib --target "$host_target" --features diagnostics
```

`cargo test-host` is currently an x86_64 Linux alias. It works only with that
target and an appropriate execution environment. On AArch64 Linux use the
commands above; installing an x86 target alone does not make its executables
native. Hosted synchronization tests use mock/no-op interrupt control and
hardware fault injection, not EL2 privileged instructions.

Coverage includes DT/argument validation, memory normalization and ownership,
allocator/heap contention, guarded stack layouts, startup generations and entry
claims, pure waits and terminal boot policy, publication failures, management
channel ordering, EL2 update epochs, register policy, guest DTB/layout and MMIO.

## Builds and static analysis

```sh
cargo build --locked
cargo build --locked --release
cargo clippy --locked --lib --target "$host_target" -- -D warnings
cargo clippy --locked --bin tvisor -- -D warnings
git diff --check
```

Treat Clippy diagnostics as findings to assess and record for the tested
revision. A successful build or library test run does not imply a clean Clippy
run; do not suppress findings globally to report a pass. Keep exact counts,
toolchain versions and remediation tracking in the issue/PR or test evidence.

Also build diagnostics in debug/release and each acceptance feature individually;
the [feature table](../setup/build.md#build-features) describes the supported
matrix. Preserve the exact ELF immediately after each build: feature variants
share Cargo output filenames. A later feature build can otherwise invalidate
an earlier image's check or hardware provenance. Do not use `--all-features`.

## Linked-image checks

Python 3, `llvm-nm` and `llvm-objdump` are needed. For an ordinary debug ELF:

```sh
elf=target/aarch64-unknown-none/debug/tvisor
python3 scripts/checks/secondary_entry.py "$elf"
python3 scripts/checks/secondary_entry_negative.py "$elf"
python3 scripts/checks/diagnostics.py "$elf"
```

For a diagnostics-enabled ELF, add `--enabled` to `diagnostics.py`. For either
TLB acceptance ELF, add both `--enabled --scratch` and run:

```sh
python3 scripts/checks/tlb_sync.py "$elf"
```

Select `elf` to match the profile just built, including the release path when
appropriate. The secondary checker verifies stackless entry constraints,
full-affinity slot lookup, shared-config offsets, acquisition before Rust,
publication instructions, vectors and IRQ/world-switch preservation. Its
negative checker rejects eleven deliberately corrupted ELF copies. The TLB
checker checks the exact maintenance sequence and rejects nine mutations.
Diagnostic isolation checks ordinary images contain neither detailed success
markers nor scratch helpers while retaining actionable failure messages.

These are structural checks of linked instructions. They do not prove cache
history, firmware compliance, physical coherency or actual cross-CPU execution.

## Test execution limitations

The standalone [VM mapping harness](../../tests/vmctl_stage2.rs) includes
`vmctl.rs`, which imports `crate::guest` and `crate::vcpu`; the harness does not
provide those modules and therefore does not compile. Its separate invocation is:

```sh
cargo test --locked --no-default-features --target "$host_target" --test vmctl_stage2
```

Passing `--lib` runs do not cover that harness or certify its VM mapping tests.
Record the compile failure when assessing VM test coverage rather than treating
an excluded target as passing. Remove this limitation when the harness provides
the required module boundary and its tests run successfully.

The x86_64-only alias has the host-target restriction described in
[native tests](#native-tests). Use an executable native target before interpreting
target-not-installed or wrong-architecture errors as implementation failures.
Track remediation through [issue #31](https://github.com/saimizi/tvisor/issues/31)
and its PRs; this guide does not maintain checkpoint results.

## Investigating an abort

Preserve the exact ELF and complete fresh exception frame before rebuilding.
Determine whether the exception came from host EL2 or guest execution; ESR/FAR
meaning depends on the exception class. Decode ESR's EC and class-specific ISS,
and use FAR only when valid. For a guest stage-2 abort, correlate HPFAR's page
with FAR's offset rather than treating the Linux VA as a host PA.

```sh
llvm-addr2line -e "$elf" -f -C "$exception_pc"
llvm-objdump -d --demangle "$elf"
llvm-nm -n "$elf"
```

Set `elf` to the preserved build and `exception_pc` to its captured hexadecimal PC.
Find the faulting instruction and operand registers, then walk the applicable
translation regime and check ownership, memory type and permissions. A fault
near a device register can be caused by an earlier wrong mapping or stack
change. Check linked instructions and frame offsets before attributing a crash
to Rust source. Use an isolated, controlled experiment to test a hypothesis and
retain failed attempts. The deliberate boot breakpoint is a known diagnostic
path; it does not imply generic exception recovery exists.
