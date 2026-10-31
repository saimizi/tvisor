# Phase 11.3 local acceptance review

Task 11, 2026-10-31. **PASS for the local validation gate.** This is not a
Phase 11.3 hardware PASS. Supported-board builds retain Task 10's enabled release
path. No hardware was operated for this task.

## Checks and results

| Check | Result |
|---|---|
| `cargo fmt --check` | PASS |
| `git diff --check` | PASS |
| `cargo test --locked --lib --target aarch64-unknown-linux-gnu` | 166 passed, zero failed |
| Same host tests with `--release` | 166 passed, zero failed |
| `cargo build --locked` | PASS, no warnings |
| `cargo build --locked --release` | PASS, no warnings |
| Linked entry/publication checker on debug ELF | PASS |
| Linked entry/publication checker on release ELF | PASS |
| Six deliberate ELF corruptions, debug | All six rejected |
| Six deliberate ELF corruptions, release | All six rejected |

The host test counts describe separate executions of the same 166 tests, not
332 distinct tests. Compiler: rustc 1.94.0 (`4a4ef493e`, 2026-03-02). The host is
AArch64; the old x86-64 `cargo test-host` alias was not used. Cargo used the locked
dependencies and the repository's AArch64 linker configuration.

## Findings resolved in this task

The existing linked checker was too permissive in several places. It accepted
syntactically record-based loads without validating their precise offsets and
only partially checked the entry predicates and maintenance ordering. The checker
now verifies:

- First instruction masks all D/A/I/F bits; CurrentEL comparison requires EL2.
- MMU/data-cache/endianness rejection, A72 implementer/part extraction/comparison
  and SMPEN test are present before record use.
- Every pre-boundary load uses a specific bounded record operand; no literal,
  stack or MMIO load is accepted, and no pre-boundary store/call is accepted.
- Full-affinity mask construction, stack/context checks, SPSel synchronization,
  generation compare and LDAR address/order match the production ABI.
- The exact TTBR/ISB/DSB/TLBI/IC/DSB/ISB/SCTLR/ISB sequence precedes acquire.
- Rust receives record and saved entry observations in x0–x4, and only one Rust
  call exists after the coherent boundary.
- Primary and secondary contain the same explicit 17-instruction host-control
  sequence, including timer disablement and final ISB.
- Record/vector address materialization resolves to the actual linked symbols,
  using ADR or ADRP/ADD as appropriate; entry/vector/record regions and sizes fit.
- Publication primitives contain exactly their intended instructions, including
  the pre-store physical timestamp and full-width release store.

The new `secondary_entry_negative.py` patches copies of each linked ELF and
confirms rejection of a pre-stack store, an early Rust call, a corrupted affinity
mask, LDR replacing LDAR, NOP replacing DC CVAC, and a 32-bit release store. The
original ELF files are untouched. These checks test the checker against concrete
bad machine code; they are not execution/emulation of the target CPU.

No Rust implementation defect requiring a change was found in this review.
The code changes for Task 11 are the strengthened checker and its negative test.

## Integrated source and binary review

The review followed the production path from `post_switch_page_tables` through
backend selection, one-shot boot attempt, active-policy checks, prepared slots,
reserved-cell mapping, record publication, real resource validation/freeze,
consuming cache publication/release, measured wait, acquired checkpoint reporting
and consuming guest permit. Each early error bypasses guest entry and leaves
static/allocated secondary resources retained. Normal guest invocation has one
boot call site through `permit.run`.

The source/table-builder review agrees with the binary's identity-mapping
requirements: text and vectors are RX Normal, constants R/NX Normal, shared data
and tables RW/NX Normal, and usable stacks RW/NX with unmapped guards. Final
validation covers complete object extents, including crossing a page boundary,
and every prepared stack. Host stage-1 mutation is closed by the same locked
validation/freeze operation; guest stage-2 ownership remains separate.

The secondary only reads published startup storage before the acquire boundary.
It keeps original entry observations in registers and performs no Rust call,
allocation, lock acquisition or UART access before that boundary. Rust claims
Entered before diagnostic writes, validates all planned local controls, and
publishes Arrived/Parked or Failed. Checkpoint reads follow terminal worker-state
acquisition; late evidence cannot replace a terminal boot decision. No Online,
interrupt unmasking, scheduler, CPU_OFF, retry or mapping-update protocol is added.

The cache plan covers text, vectors, rodata, the complete writable image (including
record/context/topology/map), the full table arena and the selected usable stack.
The aligned page extents and accepted line sizes keep rounding inside validated
mappings, including shared lines. The release cell is handled separately with a
zero recheck, one LE 64-bit store, line clean, completion and event. The physical
counter sample is immediately before STR; the validated deadline uses low-56-bit
elapsed ticks and a finite poll fallback. No resource lock spans publication,
release, waiting or reporting.

The existing quick-regression runner's fatal-marker pattern was also inspected;
the normal `local validation failures=0x0` diagnostic does not match it. The
runner was not executed or changed for this task.

## Architecture and errata assessment

Reviewed [Arm's A72 errata notice, EPM-012079 v11.0](https://documentation-service.arm.com/static/5fa29f9fb209f547eebd3610),
particularly 859971, 1319367 and 1387635. The following are scoped source-review
inferences, not claims that firmware has installed every possible workaround:

- 859971: the relevant alternative is separation of read-sensitive Device pages
  from executable Normal pages. Host executable mappings are confined to text
  and vectors; their following page is Normal rodata, not MMIO. Conflicting device
  mappings are rejected. No CPUACTLR write is added.
- 1319367: the connected host binary contains only current-EL2 `AT S1E2R`
  operations. The parked secondary executes no AT operation or lower-regime
  context switch. This is not a general guest/firmware context-switch errata audit.
- 1387635: new host mappings are followed by DSB/TLBI/DSB/ISB before access;
  takeover/entry also synchronize before the new regime is used. Host mappings
  are frozen after release.

Actual MIDR/variant/revision and firmware configuration must still be recorded
on the board. New execution regimes, MMIO layouts, lower-EL context behavior or
post-release table changes require reassessment; generic tests do not establish
hardware applicability. Broader guest/security/firmware errata handling is not
certified by this entry-path review.

## Evidence and remaining acceptance

Logs, positive/negative disassembly checks, LOAD/section reports, actual linked
ELFs, tool versions, source snapshots and hashes are retained under ignored
`docs/evidence/phase11_3_task11/`. The manifest identifies the dirty checkout and
hashes of the artifacts/source used. This work was neither committed nor pushed.

Task 12 must cold-boot the connected image and prove one physical arrival/park
with the other secondaries retained in firmware. Task 13 must exercise deliberate
bounded failure; Task 14 must complete the full quick guest regression while the
secondary is parked. Task 15 must assemble the final review bundle, including
independent final power-off evidence. Those gates remain unfulfilled.
