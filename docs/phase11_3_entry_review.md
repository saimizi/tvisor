# Phase 11.3 secondary entry review

Task 6, 2026-10-31. This review covers the linked stackless trampoline and its
Rust boundary, not release authorization or hardware acceptance. The code is in
[`src/secondary.rs`](../src/secondary.rs); the repeatable linked check is
[`scripts/checks/secondary_entry.py`](../scripts/checks/secondary_entry.py).

## Changes made

- Replaced the full-affinity mask's literal-pool load with MOVZ/MOVK construction
  of `0x000000ff00ffffff`. No startup literal pool remains.
- Before installing SP, validate nonzero in-range logical ID, stack top alignment,
  page-aligned nonzero bottom, unsigned bottom < top and exactly 64 KiB of usable
  stack, nonzero 64-byte-aligned context, and nonzero page-aligned root.
- Synchronize SPSel before selecting SP_EL2; install TPIDR and VBAR, then run
  the common host register initializer. SP is a 16-byte-aligned exclusive top;
  the first Rust prologue grows down into mapped usable stack bytes.
- Add ISB after TTBR0_EL2 installation before local translation maintenance.
- Preserve original SCTLR, MIDR, CPUECTLR and MPIDR in x21–x24, then pass them
  in x1–x4 at the Rust call. Rust claims entry before storing these observations
  in the production record. The ABI specification records their added fields.
- Export stack-ready, coherent-Rust-boundary and reject labels for binary review.

## Control flow and allowed memory accesses

The entry masks DAIF, checks CurrentEL, entry SCTLR, MIDR implementer/part and
SMPEN before loading the record. Identification precedes the implementation-
defined CPUECTLR read. The expected regime is the supported firmware profile in
[the backend contract](phase11_3_backend_contract.md); these checks do not replace
that contract. Entry with MMU/data cache enabled or big-endian data is rejected.
An enabled I-cache is compatible only when firmware has ensured no stale entry
instructions; local invalidation cannot repair stale instructions already run.

A nonzero generation and complete masked affinity are checked before installing
any private pointer. All loads before the coherent boundary address bounded
fields of `SECONDARY_RECORD` through x19. There are no stack memory accesses,
stores, MMIO operations, BSS clearing, argument parsing, allocation, locks or
calls before the single `bl secondary_main`. Registers keep entry observations
without touching shared diagnostic storage in the MMU-off regime.

Every conditional rejection branches to one WFE/back-branch loop, including
wrong affinity. There are no polling loops or retries before the Rust boundary.
Rejection never acquires a lock, allocates or prints. Before safe coherent memory
access, CPU 0's bounded timeout is the failure evidence; the trampoline does not
attempt noncacheable diagnostic writes that might conflict with CPU 0's cached
record. Detailed Rust failure reporting is Task 7.

The common controls establish HCR/CPTR/HSTR/VTTBR and timer policy and execute ISB.
The trampoline installs MAIR/TCR/TTBR, synchronizes context, executes DSB SY,
local `TLBI ALLE2`, local `IC IALLU`, DSB SY and ISB, then installs SCTLR and
executes ISB. Only after that does LDAR acquire ready and compare it to the
expected generation. A mismatch rejects without calling Rust. The first Rust
call passes the record and the four original entry observations.

The instruction choices are reviewed against the supported
[Arm Cortex-A72 TRM](https://documentation-service.arm.com/static/60368ce38f952d2e4134dc2e),
particularly CPUECTLR/SMPEN, and the
[AArch64 boot requirements](https://docs.kernel.org/arch/arm64/booting.html)
for entry/cache publication. SMPEN is checked before maintenance/MMU enablement.
The clean initial data-cache history and coherency prerequisites remain firmware
contract obligations; observing C=0 does not prove them.

## Mapping and lifetime review

| Object | Before MMU enablement | After MMU enablement | Remaining runtime obligation |
|---|---|---|---|
| Entry, instructions around SCTLR/ISB and Rust code | Linked physical code | Identity RX Normal `.text` mapping | Validate final live mapping and code publication before release |
| Vector table | Linker address installed in VBAR | Identity RX Normal `.vectors` mapping | Validate VBAR mapping, alignment and publication |
| Startup record/context | Record bounded physical reads; context not dereferenced by entry | Static writable image data, RW/NX Normal | Validate selected resources and complete publication |
| Private stack | SP installed, no memory accesses before Rust | Identity RW/NX usable stack; unmapped guard | Validate prepared stack and preserve allocation through timeout |
| Shared translation tables | Physical hardware table walks at enablement | Identity RW/NX table arena | Validate root/table coverage, publication and freeze |

`src/mm.rs` constructs the section permissions above. The linked checker verifies
entry/vector/record symbols lie in the corresponding linker regions. Final
runtime mapping checks belong to Task 8. The register-only mask removes the
previous literal-pool mapping dependency. No code crosses from one virtual
address to a different physical address when SCTLR.M is set: identity mapping is
a precondition on the boot release path, not something the assembly can prove.

## Reproducible local validation

```sh
cargo build
python3 scripts/checks/secondary_entry.py target/aarch64-unknown-none/debug/tvisor
cargo build --release
python3 scripts/checks/secondary_entry.py target/aarch64-unknown-none/release/tvisor
cargo test --lib --target aarch64-unknown-linux-gnu
cargo fmt --check
```

Both debug and optimized linked-entry checks PASS. The checker examines decoded
instructions, call sites, memory operands, conditional rejection destinations,
full-affinity mask construction, symbol regions/alignment and ordering around
SP, translation setup and LDAR. It rejects undecoded bytes/gaps and forbidden
pre-boundary instructions. It is structural verification, not emulation or a
proof of instruction semantics, live mapping permissions or cache behavior.

The source also compiles all record/mailbox/entry-field layout assertions.
All 144 host tests pass. Builds retain inactive-startup warnings because boot
integration is Task 10. Review logs, ELF/source hashes and source snapshots are
retained under ignored `docs/evidence/phase11_3_task6/`.

Task 6 is complete for the supported assembly entry and local binary review.
Task 7 owes complete local validation/reporting; Tasks 8/9 owe live resource
validation and boot-side cache publication; Task 11 rechecks the final integrated
binary. Actual MIDR/revision, applicable errata, cache/translation observations
and successful execution must be recorded during hardware bring-up. No hardware
was operated and the secondary release gate remains inactive.
