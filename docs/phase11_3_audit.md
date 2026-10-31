# Phase 11.3 implementation audit

Current result, Task 15: implementation and scoped acceptance are complete and
ready for user review. See the [47-row closure map and evidence handoff](phase11_3_status.md).
The Task 1 findings below remain a historical snapshot; dated follow-ups record
their resolution.

Audit date: 2026-10-31. Baseline HEAD:
`de73e049e604cf310fe44fabb625042560dfa19b`, plus the current uncommitted
Phase 11.3 draft. Requirements come from [the design](phase11_3_design.md).
This is the Task 1 deliverable; it does not approve release or complete 11.3.
No implementation changes or hardware operations were performed for this audit.

Status meanings:

- **Implemented**: the scoped code requirement is present; hardware correctness
  is not implied.
- **Partial**: a draft or helper exists, but integration, validation or coverage
  is incomplete.
- **Missing**: no completing implementation or acceptance evidence was found.

## Task 1 overall finding (historical)

**Phase 11.3 is incomplete and inactive.** `src/main.rs` includes the secondary
module and the linker retains its trampoline, but
`src/boot.rs::post_switch_page_tables` proceeds from heap initialization directly
into `run_guest`. It never calls `secondary::start_one` or waits for a parked
secondary. No Phase 11.3 hardware success, deliberate failure or parked-secondary
regression evidence has been established in this session.

The draft now embeds the production `SecondaryMailbox` in `SecondaryRecord`;
the design's references to duplicated record/mailbox handoff words and 127 tests
are stale. However, `pcpu::CpuStartupState` still has a separate preparation
lifecycle and unused request/generation/acknowledgement fields. Their relationship
to the new protocol needs an explicit ownership decision.

## Task 1 requirement checklist (historical)

### Design sections 1–2: scope and backend

| ID | Requirement | Status | Evidence and remaining work |
|---|---|---|---|
| B1 | Exactly one enabled non-boot target, lowest logical ID | Implemented | `tvisor_util/secondary_entry.rs::select_secondary` selects by logical ID. `start_one` uses it; it is not reached from boot. |
| B2 | Match full MPIDR affinity | Implemented | Topology discovery assigns logical IDs by full affinity; trampoline and `secondary_main` mask with `MPIDR_AFFINITY_MASK`. Hardware gate remains open. |
| B3 | Unique aligned cell, including disabled CPU descriptions | Implemented | `validate_spin_table_target` checks alignment and compares all other descriptions, including disabled CPUs. Tests cover shared cells. |
| B4 | Firmware ownership and finalized reservations | Implemented | `cpu_startup::StartupMemoryContract` validates reserved RAM/cells; boot verifies finalized reservations before allocator initialization. This establishes memory ownership, not entry state. |
| B5 | Evidence-backed EL2/MMU/cache/endianness contract | Missing | `EntryContract` is a set of caller-supplied booleans. No production backend establishes them. `rpi4_secondary_startup.md` and ignored firmware observations are investigation inputs. |
| B6 | Evidence-backed coherency and permitted implementation-defined register access | Missing | MIDR/SMPEN checks exist in assembly, but do not establish the complete platform contract. |
| B7 | Reject unsupported entry before stack/shared Rust | Partial | Source rejects non-EL2, MMU-on, cache-on, big-endian, unsupported MIDR and SMPEN-off before SP installation. Linked disassembly and architecture review remain required. |
| B8 | No PSCI fallback or shared-cell activation | Implemented | Target validation rejects unsupported methods/shared cells; release helper only writes one cell. |

### Design section 3: ABI, ownership and lifetime

| ID | Requirement | Status | Evidence and remaining work |
|---|---|---|---|
| A1 | One documented production startup ABI | Partial | `SecondaryRecord` embeds the tested mailbox; assembly acquires record+64. Document the whole ABI and reconcile `CpuStartupState` preparation/unused handoff fields. |
| A2 | Required immutable identity, stack, context, translation and generation fields | Implemented | `StartupRecord`/`SecondaryRecord` contain these fields; `publish_record` claims initialization once and rejects some malformed inputs. |
| A3 | Publication without whole-record mutable aliasing | Partial | Publisher writes ordinary fields through raw field pointers; waiter acquires the mailbox before borrowing the record. Still requires complete record lifecycle/concurrency tests and review. |
| A4 | Compile-time assembly offset, size and alignment checks | Implemented | Record offsets 0–64, 256-byte size and 64-byte alignment are asserted; mailbox offsets/size are asserted. |
| A5 | Entry/arrival/park and early-failure evidence | Partial | Ready/arrived/parked generations and terminal outcome exist. No separate entered status; Rust failure is one numeric flag; safe early assembly failures have no recorded status. |
| A6 | Complete local checkpoint, including cache/coherency observations | Partial | Checkpoint contains EL, MPIDR, SP, TPIDR, VBAR, MAIR/TCR/root/SCTLR, DAIF/HCR/CPTR. Entry SCTLR/MIDR/SMPEN observations and several programmed control/timer readbacks are absent. |
| A7 | Stable storage through timeout and guest execution | Partial | Record/context/tables are static; allocated stacks are retained. Success/failure/late-arrival lifetimes have not been exercised end to end. |
| A8 | Unmapped guards and valid addresses across MMU transition | Partial | Existing stack mapper validates guards; draft checks identity reads over publication ranges. Complete permission/root/entry/context validation and linked literal-pool/MMU-transition review remain. |

### Design section 4: boot CPU sequence

| ID | Requirement | Status | Evidence and remaining work |
|---|---|---|---|
| C1 | Finish once-only initialization before release | Partial | Boot prepares topology/map/allocator/stacks/heap/vectors/console. Place startup after these prerequisites and enforce the boot call order. |
| C2 | Validate every prerequisite before irreversible write | Partial | `start_one` checks two entry booleans and structural target validity. It bypasses `validate_spin_table_release`, ignores the writable/published contract fields, and lacks a single authoritative final preflight. |
| C3 | Host-only full-page RW/NX Normal identity cell mapping | Partial | `mm::map_firmware_release_cell` checks captured cell membership, creates/validates a page mapping and performs local TLB maintenance. It is unused in actual boot; acceptance must confirm reservation ownership and no guest mapping effect. |
| C4 | Final resource checks and permanent mapping freeze | Partial | `freeze_tables` shares the table lock with writers; `with_tables` rejects mutations after freezing. Draft `start_one` invokes it, but complete resource validation and freeze rejection tests are absent. |
| C5 | Populate one-shot nonzero-generation record | Partial | `start_one` publishes generation 1; `RECORD_CLAIMED` prevents reuse. Full record validation tests and integration remain. |
| C6 | Complete cache-line-aware code/data/table publication | Partial | Draft cleans mapped text through writable image end, table arena and selected stack using CTR-derived lines, then DSB SY. Prove these ranges cover every dependency, shared cache line and supported cache regime. |
| C7 | Zero recheck, aligned LE cell store, clean, barrier, SEV | Partial | `smp::release_spin_table_cell` implements the sequence with assembly, including address zero without a Rust reference. Its unsafe prerequisites and entry mapping require final caller validation. |
| C8 | Physical-counter deadline and finite fallback bound | Partial | Wait uses two frequency-derived seconds, wrapping subtraction and 100 million polls. Frequency is checked before release and again in the waiter. Deadline begins after release; polling/observation race semantics, timing bounds and terminal reporting need review. |
| C9 | Acquired checkpoint then guest only after success | Missing | Waiter prints a success checkpoint, but boot never invokes startup or branches on its result. Guest currently boots without a secondary. |
| C10 | Wait outside allocator/table/UART locks | Partial | Draft wait follows mapping/freeze lock release and precedes printing; verify the final integrated call path. |

### Design sections 5–6: secondary, errors and terminal state

| ID | Requirement | Status | Evidence and remaining work |
|---|---|---|---|
| S1 | Stackless entry, private SP/TPIDR/VBAR, acquire before Rust | Partial | Source implements this sequence; binary review and hardware proof are outstanding. |
| S2 | Shared named primary/secondary register policy | Missing | Secondary hard-codes HCR/CPTR/HSTR/VTTBR/timer values. Primary translation policy is built separately; no common policy builder covers both paths. |
| S3 | Configure trap/FP/timer/translation state in reviewed order | Partial | Assembly writes the relevant draft controls and disables local timers. RES0/RES1, implementation support and ordering review remain. |
| S4 | Complete local validation before arrival | Partial | Rust checks identity/context, stack, translation, VBAR, DAIF/HCR/CPTR and published-map availability. It does not validate all HSTR/VTTBR/timer state or preserve entry-state observations. |
| S5 | No VM/vCPU/emulation access | Partial | Secondary never dereferences an active vCPU or emulation object. Context validation reads `active_vcpu` only to require null; document this metadata check explicitly. |
| S6 | Separate Parked after valid arrival; masked permanent WFE | Implemented | `acknowledge` precedes `park`, then WFE loops without allocation/logging. No Online publication occurs. Hardware execution is unverified. |
| F1 | Preflight failure reported by CPU 0 without cell write | Partial | Helpers return errors before the store; production caller/reporting is missing. |
| F2 | Rust failure published and consumed | Partial | Secondary release-stores `checkpoint.failure = 1`, then parks. Waiter never consumes that flag, so validation failure appears as a generic timeout. |
| F3 | Timeout records last safe evidence and retains resources | Partial | Mailbox can finalize failure and resources remain allocated/static. Waiter returns a generic string without a complete failure snapshot or reason distinction. |
| F4 | Late arrival cannot replace terminal failure | Partial | Atomic `finish` preserves the first terminal outcome; tested late arrival cannot change it. Worker arrival/park words may still advance as diagnostic evidence. Final boot behavior and retention need integration tests. |
| F5 | Reject stale/zero/duplicate protocol transitions | Implemented | Mailbox uses generation checks and compare-exchange; helper tests cover these cases. Full record/assembly interaction remains untested. |
| F6 | No retry, clear, reclamation or CPU_OFF after release failure | Partial | Draft has no rollback/retry/reclamation path. Production terminal halt/reset behavior is not connected to boot. |

### Design section 7: acceptance

| ID | Requirement | Status | Evidence and remaining work |
|---|---|---|---|
| V1 | Target, backend and missing-contract host tests | Partial | Helper tests cover selection, shared/disabled cells, unsupported method and each false contract field. They do not cover the actual `start_one` preflight, which bypasses the full validator. |
| V2 | ABI and startup-record validation tests | Partial | Compile-time ABI assertions exist. Host tests do not instantiate/validate the production `SecondaryRecord` or its publication lifecycle in `src/secondary.rs`. |
| V3 | Visibility, generations, bounded waits and terminal tests | Partial | Production mailbox tests cover these cases, including wrapping/stalled counters. Checkpoint visibility, park-specific publication and complete startup failure paths need coverage. |
| V4 | Bare-metal build and linked-disassembly review | Partial | Build passes with 12 unused-startup warnings. No completed Phase 11.3 linked-disassembly acceptance review is recorded. |
| V5 | Baseline hashes, fresh U-Boot layout and DT topology | Partial | Earlier baseline/firmware evidence exists; the session inspected U-Boot RAM layout and verified power off. No complete Phase 11.3 run bundle ties fresh topology, hashes and source to a release. |
| V6 | One physical secondary arrives/parks; other CPUs stay in firmware | Missing | No Phase 11.3 release/arrival/park evidence. |
| V7 | Deliberate bounded hardware failure with terminal retention | Missing | No documented test mechanism or completed run. |
| V8 | Full quick regression with secondary parked | Missing | Prior Phase 11.2 results do not satisfy this gate. |
| V9 | Final logs, provenance, hashes, thresholds and independent power-off evidence | Missing | Must be produced for the actual Phase 11.3 success/failure/regression runs. |

## Verification performed for this audit

- `cargo fmt --check`: PASS.
- `cargo build`: PASS, with **12 dead-code warnings** for inactive startup paths.
- `cargo test --lib --target aarch64-unknown-linux-gnu`: **130 passed**, zero
  failed. The host is AArch64; the repository's x86-64 `test-host` alias is not
  the command used here.
- No hardware was powered on or released for Task 1. No new disassembly
  acceptance review was performed. Source inspection is not a hardware PASS.

## Mapping to the remaining small tasks

Continue with Task 2 (backend contract: B5–B7), then Tasks 3–4 (ABI/lifecycle:
A1–A7, F2–F5, V1–V3), Tasks 5–7 (policy/entry/local checks: S1–S5), Tasks 8–10
(preparation/publication/boot integration: C1–C10, F1–F6), Task 11 (local and
binary validation: V1–V4), Tasks 12–14 (hardware gates: V5–V8), and Task 15
(review evidence: V9). Implemented rows still require the applicable integrated
validation before the milestone can pass.

## Task 2 follow-up (2026-10-31)

The [backend contract](phase11_3_backend_contract.md) now supplies the documented
basis missing from B5/B6. Those rows advance from Missing to Partial: backend
selection is not enforced by production code, and live secondary confirmation
remains pending. B7 remains Partial pending binary review and hardware checks.
The original checklist above is preserved as the Task 1 snapshot.

## Task 3 follow-up (2026-10-31)

The [startup ABI](phase11_3_startup_abi.md) now documents the production library
record/storage, one-shot publication/reference rules and preparation ownership.
A1's duplicate preparation handoff fields are removed; A3 has production-record
concurrency coverage; A4 uses asserted layouts and assembly-derived offsets.
V2 now includes host tests of the actual record storage. Mapping, full protocol,
checkpoint and hardware acceptance findings remain for their assigned tasks.

## Task 4 follow-up (2026-10-31)

The [ABI/protocol specification](phase11_3_startup_abi.md#task-4-protocol-transitions)
now defines serialized worker transitions, explicit failure and typed terminal
outcomes. The waiter consumes Failed instead of reporting it as a generic
timeout (F2 protocol portion); detailed local validation reasons remain Task 7.
A5 now has coherent-Rust Entered evidence, but no raw physical-entry report.
F4/F5 and V3 have expanded production tests for late evidence, concurrent
terminal decisions and checkpoint publication. Actual boot failure gating,
early assembly reporting and hardware acceptance remain outstanding.

## Task 5 follow-up (2026-10-31)

The [shared EL2 policy](phase11_3_el2_policy.md) implements S2: primary and
secondary inline one host-control sequence and use one validated translation
builder. Guest RW/CPTR definitions reuse the same constants. Secondary HCR/CPTR
validation now references that policy. S3 remains Partial pending full entry
ordering/architecture review; hardware regression must cover the newly explicit
primary initialization. Local validation: 144 tests, build and formatting pass;
linked common instruction sequences match.

## Task 6 follow-up (2026-10-31)

The [entry review](phase11_3_entry_review.md) records the completed stackless
assembly path, defensive pre-SP resource checks, literal-free affinity matching,
entry-observation handoff and repeatable debug/release linked checks. B7 and S1
now have local binary evidence; their physical execution remains unvalidated.
A6 has retained original entry observations pending Task 7 checks/reporting.
V4 has a scoped automated entry check, while final integrated binary acceptance
remains Task 11. No secondary has been released.

## Task 7 follow-up (2026-10-31)

The [local validation specification](phase11_3_local_validation.md) records the
production validator, complete planned control/timer readbacks, entry observations,
28-bit named failure mask and CPU 0 success/failure reporting. A6, S4 and the
Rust reporting portion of F2 now have production host coverage. Context identity
checks validate the actual owner-only context predicate without accessing a
vCPU object. Runtime mapping validation, final boot gating and hardware evidence
remain assigned to subsequent tasks.

## Task 8 follow-up (2026-10-31)

The [resource preparation specification](phase11_3_resource_preparation.md)
records complete firmware-page ownership, exact mapping/permission/object checks,
prepared-stack validation and a production table gate whose validation/freeze
share one lock. C3/C4 and the resource-layout portion of A8 now have production
host coverage. Target/slot affinity and active translation policy are checked in
the draft caller. Final release-contract/publication consumption remains Task 9;
actual boot call order and failure gating remain Task 10. No CPU was released.

## Task 9 follow-up (2026-10-31)

The [publication specification](phase11_3_publication.md) records the implemented
consuming resource/publication/release handles, checked cache ranges and final
cell sequence. C2's unused resource booleans are removed; actual freeze and
cache-completion results gate release. C6/C7 now have production callback-order
and failure-path tests plus linked primitive checks. Backend selection and the
actual guest-boot terminal gate remain Task 10, with final integrated checks and
physical acceptance still pending. No secondary was released for Task 9.

## Task 10 follow-up (2026-10-31)

The [boot integration specification](phase11_3_boot_integration.md) records the
now-connected startup path, named backend selection, pre-store timestamp and
validated low-56-bit physical deadline, persistent terminal boot result and
consuming guest permit. C1/C8/C9/F1/F6 now have connected control flow and local
gate/deadline tests; release errors abort the guest path and retain resources.
Builds are warning-free. Physical acceptance and final integrated review remain
Tasks 11–15; no hardware was operated for Task 10.

## Task 11 follow-up (2026-10-31)

The [local acceptance review](phase11_3_local_acceptance.md) records passing
format/whitespace checks, 166 host tests in both profiles, warning-free debug/
release target builds, stronger linked checks and six rejected ELF mutations
per profile. It includes the scoped architecture/errata assessment and source/
binary review. V1–V4 have local evidence for the connected path; V5–V9 still
require physical acceptance and final run provenance. No board was operated.

## Task 12 follow-up (2026-10-31)

The [hardware arrival review](phase11_3_hardware_arrival.md) records one passing
cold boot on lab-rpi4: CPU1 generation 1 reached Parked with zero validation
failures, exact host policy/context/stack evidence and entry-state observations.
Added read-only CPU0 post-arrival cell verification before the guest permit;
only CPU1's cell contained the linked entry, while CPU2/CPU3 cells stayed zero.
The unchanged Linux fixture booted and completed a fresh console command.
V5/V6 now have hardware evidence, including image/source hashes, fresh U-Boot/DT
layout and independent verified power-off. All 166 host tests in both profiles,
both warning-free builds and linked positive/negative checks passed after the
diagnostic change. V7/V8 and the complete V9 review bundle remain Tasks 13–15.

## Task 13 follow-up (2026-10-31)

The [failure acceptance review](phase11_3_failure_acceptance.md) documents the
opt-in `secondary-startup-timeout-test` feature and one passing hardware run.
CPU1 delays acknowledgement after validation; the normal two-second wait timed
out at 108,000,022 ticks/54 MHz while Entered. Its late zero-failure Parked
checkpoint could not replace TimedOut/WaitFailed; late Success and a new boot
attempt were rejected. Retained record/context/stack/root and unchanged other
release cells were reported, guest entry stayed blocked, and a 23.223-second
quiet observation plus independent verified power-off completed the run.
V7 now has hardware evidence without mapping/coherency corruption. Normal and
test-feature builds passed linked checks in both profiles, and host protocol
tests passed. Test instrumentation is compiled out of normal builds. V8 and the
complete V9 bundle remain Tasks 14–15.

## Task 14 follow-up (2026-10-31)

The [quick regression review](phase11_3_quick_regression.md) records a full PASS
with the normal debug image, timeout-test feature disabled, and CPU1 parked on
all four boots. Two normal cold boots, two diagnostic boots, 18 console commands,
10 increasing timer IRQ checks, the 35-second smoke test, the full 600.453-second
soak and the final short probe passed. The soak completed 8,133 memory passes;
maximum wakeup lateness was 7.585 ms, with zero wakeups over 500 ms. Each fresh
boot had zero local validation failures and zero CPU2/CPU3 release cells. No
fatal/reset marker occurred. Frozen images, dirty-source snapshots, configuration
diff, full UART/results and independent verified power-off are retained. V8 now
has the required hardware evidence; Task 15 remains the final consolidated review
and V9 closure.

## Task 15 follow-up (2026-10-31)

The [completion handoff](phase11_3_status.md) closes all 47 scoped requirements,
indexes the implementation and original acceptance artifacts, records exact
limits and provides a local review archive. All 330 hashes from the four local/
hardware acceptance indexes verified, and the current implementation matches
the final quick-regression source snapshot. A fresh read-only relay query again
confirmed off with identity verified. Top-level status text now reflects the
implemented state; historical task reports are preserved. V9 is closed.
No runtime implementation changed, no new hardware test was run, and nothing
was committed or pushed. The completed milestone is ready for user review.
