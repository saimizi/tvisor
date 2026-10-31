# Raspberry Pi 4 secondary startup interface

Phase 11.0 validates the DT-described boot interface and memory ownership.
Firmware identification and disassembly are diagnostic evidence only. Tvisor
does not embed a firmware binary, inspect firmware instructions, or require a
specific firmware build to boot.

## Runtime checks

`cpu_topology.rs` discovers CPU affinities, status, per-CPU `enable-method`,
`cpu-release-addr`, and any PSCI binding/conduit. Logical CPU0 is selected by
matching the boot CPU's complete MPIDR affinity. Capacity remains four CPU nodes
for the supported Pi 4 target; neither physical affinity zero nor fixed release
addresses are required by startup-memory validation.

For enabled spin-table CPUs, `cpu_startup.rs` requires each release cell to be
an aligned 64-bit location, entirely within enabled DT-described RAM and a
valid DT reservation-block range. Shared release cells are accepted and read
once. Before reading a cell, `src/smp.rs` checks the inherited EL2 translation
provides an identity read mapping; then it requires the initial value to be
zero. No null Rust pointer is dereferenced, including for physical address zero.
Only advertised cells are read; surrounding firmware code is not inspected.

Before allocator initialization, the finalized memory map must exclude **every
DT reservation-block range** from usable RAM, as well as every captured release
cell. This preserves firmware holding code in reservations separate from the
cells. Existing platform memory discovery also preserves `/reserved-memory`
regions. No code-footprint estimate or hard-coded first-page reservation is
substituted for the firmware's advertised reservations.

The [AArch64 spin-table boot protocol](https://docs.kernel.org/arch/arm64/booting.html)
requires firmware to describe and reserve its holding area and release cells.
Tvisor trusts this interface: checking reservations does not independently prove
that firmware has accurately described every live dependency. A missing cell
reservation, invalid RAM range, nonzero/unreadable cell, or allocation overlap
stops boot. A change to firmware instructions alone does not.

PSCI descriptions remain separate from callable firmware interfaces. A PSCI
node does not trigger speculative SMC/HVC instructions, `PSCI_VERSION` probes,
or an automatic backend fallback. An EL2-callable PSCI adapter and entry-state
contract must be established before a future PSCI activation path is enabled.

## Phase 11.0 boundary and Phase 11.3 release

Phase 11.3 now implements and validates one-secondary release on lab-rpi4; see
the [completion handoff](phase11_3_status.md). The Phase 11.0 boundary and
normative entry/publication requirements below remain applicable.

Phase 11.0 does not release a CPU or infer its live registers from firmware code.
In 11.3 the assembly entry must validate CurrentEL, MPIDR and the documented
MMU/cache/coherency entry conditions before it accesses ordinary shared Rust
objects. It must establish a private stack, TPIDR, vectors, trap/timer/FP state
and matching shared EL2 mappings, then complete the startup-ready synchronization
specified by the SMP design. Boot-CPU register values do not establish another
CPU's execution state.

The release path must publish initialized trampoline code, startup records,
stack/table data and mailbox contents to the required coherency point before
publishing an entry address. It must store the physical entry as an aligned
little-endian 64-bit value, complete the required cache maintenance and barriers,
and send `SEV`. Shared release cells require valid storage and routing for all
CPUs they can release. Use generation-tagged acknowledgements and bounded
startup deadlines; a timeout never permits reusing a possibly live CPU's memory.
Exact maintenance instructions are reviewed with the 11.3 trampoline and its
actual entry regime. See the [SMP design](tvisor_smp.md).

## Diagnostic observations from lab-rpi4

The inspected board advertised four enabled affinities 0..3, per-CPU spin-table
cells at `0xd8`, `0xe0`, `0xe8`, `0xf0`, and a DT reservation `[0x0, 0x1000)`.
Its boot MPIDR was `0x80000000`; no PSCI interface was advertised. These are
observations, not required address constants in the implementation.

The earlier investigation byte-compared installed `start4.elf` with the official
[Raspberry Pi firmware revision effea745e592ec8a97fc54093a5673a7e6e515c9](https://github.com/raspberrypi/firmware/tree/effea745e592ec8a97fc54093a5673a7e6e515c9).
The observed holding loop matched the armstub built from
[Raspberry Pi tools revision fc0e73c13865450e95edd046200e42a6e52d8256](https://github.com/raspberrypi/tools/blob/fc0e73c13865450e95edd046200e42a6e52d8256/armstubs/armstub8.S).
This explains that board's parked-core behavior and provides useful bring-up
evidence. It is not a runtime allowlist, an authenticated-boot mechanism, or
proof of a future firmware build's actual secondary entry state.

Generated evidence remains ignored under `docs/evidence/`; the revised runtime
validation evidence is in `docs/evidence/pr28_spin_table_interface/`.

Issue [#29](https://github.com/saimizi/tvisor/issues/29) remains deferred. Allocator
reservation checks do not replace a general protection policy for explicit
guest stage-2 mappings; current guest mappings exclude the firmware low page.

## Phase 11.3 entry contract

The [lab-rpi4 backend contract](phase11_3_backend_contract.md) records the
supported firmware/CPU profile, source evidence, coherency reasoning, permitted
implementation-defined read, runtime prerequisites and pending hardware checks.
