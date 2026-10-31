# Phase 11.3 completion and review handoff

2026-10-31: **implementation and scoped acceptance complete; ready for user
review.** Tasks 1–15 are complete. This records the implemented state and evidence,
not user approval or a merge. Validation captured an uncommitted working tree at base
`de73e049e604cf310fe44fabb625042560dfa19b`.

On the supported lab-rpi4 profile, CPU0 now publishes one startup record,
releases the lowest enabled non-boot logical CPU, and requires its validated,
generation-matched Parked checkpoint before entering the one-vCPU Linux guest.
Other CPUs remain in firmware. A failed attempt is terminal and retains its
record, stack, context and frozen host tables until reset.

## Validation outcome

| Gate | Evidence and result |
|---|---|
| Local protocol, resource and binary review | [Task 11](phase11_3_local_acceptance.md): 166 host tests in each profile, warning-free builds, linked checks and six rejected corruptions per image |
| Physical arrival and park | [Task 12](phase11_3_hardware_arrival.md): CPU1 generation 1, Success/Parked, zero failures, other cells zero, Linux console response |
| Bounded failure and late arrival | [Task 13](phase11_3_failure_acceptance.md): timeout at about 2.0000004 s; late Parked cannot replace TimedOut/WaitFailed; retry rejected, guest blocked, resources retained |
| Full quick regression with CPU1 parked | [Task 14](phase11_3_quick_regression.md): four boots, 18 commands, 10 increasing timer samples, 600.453 s soak, 8,133 memory passes, maximum soak lateness 7.585 ms, zero wakeups over 500 ms |
| Integrity and cleanup | Task 15: all 330 hashes in the four acceptance indexes verified; current implementation matches the final hardware run; fresh relay query reports off with identity verified |

The same 166 host tests ran in multiple configurations; these are not distinct
test inventories to add together. Local checks were repeated after the Task 12
diagnostic and Task 13 test-feature changes. Normal and injected debug/release
images passed linked checks. The full quick regression used the normal debug
image with timeout injection compiled out. Task 15 changed documentation and
packaging only, so hardware evidence still matches the implementation.

## Review map

| Concern | Main implementation | Supporting review |
|---|---|---|
| Boot ordering and guest gate | `src/boot.rs`, `src/secondary.rs`, `tvisor_util/secondary_boot.rs` | [Boot integration](phase11_3_boot_integration.md) |
| Platform/firmware contract | `tvisor_util/secondary_backend.rs`, `secondary_entry.rs`, `src/smp.rs` | [Backend contract](phase11_3_backend_contract.md) |
| Context/record ownership | `src/pcpu.rs`, `tvisor_util/pcpu.rs`, `secondary_record.rs`, `secondary_mailbox.rs` | [ABI/lifetime](phase11_3_startup_abi.md) |
| Stackless entry/shared policy | `src/secondary.rs`, `src/host_el2_controls.S`, `tvisor_util/el2_policy.rs` | [Entry](phase11_3_entry_review.md), [policy](phase11_3_el2_policy.md) |
| Checkpoint/validation | `tvisor_util/secondary_validation.rs`, `src/secondary.rs` | [Local validation](phase11_3_local_validation.md) |
| Mapping validation/freeze | `src/mm.rs`, `tvisor_util/startup_resources.rs` | [Resources](phase11_3_resource_preparation.md) |
| Publication/release | `tvisor_util/startup_publication.rs`, `src/smp.rs`, `src/secondary_publication.S`, `scripts/rpi.ld` | [Publication](phase11_3_publication.md) |
| Failure injection/binary checks | `src/secondary_timeout_test.rs`, `Cargo.toml`, `scripts/checks/secondary_entry*.py` | [Failure test](phase11_3_failure_acceptance.md), [local acceptance](phase11_3_local_acceptance.md) |

The record ABI is 320 bytes aligned to 64; its embedded mailbox is 24 bytes at
offset 64. `RecordStorage` occupies 384 bytes including its separate claim flag.
Raw-field initialization precedes release-ready publication; subsequent record
references require acquire observation. Preparation metadata has no second
handoff/generation protocol. Primary and secondary share the translation builder
and host-control assembly sequence. Guest HCR/CPTR definitions reuse constants
without adding guest SMP or changing guest CPU count.

The consuming sequence is validated/frozen resources → cache publication plan →
published startup → one release. The physical deadline begins immediately before
the LE64 cell store, with checked conversion, low-56-bit wrap handling and finite
poll fallback. CPU0 also re-reads every advertised cell after arrival.

## Closure of the original 47 requirements

The [Task 1 audit](phase11_3_audit.md) retains its historical findings. Every row
below is closed within Phase 11.3 scope: the implementation and required
acceptance evidence exist. This does not mean every error branch was injected
on hardware or that the firmware trust boundary has been removed.

| ID | Closing implementation/evidence |
|---|---|
| B1 | Lowest eligible nonzero logical ID; target tests; physical CPU1 on acceptance boots |
| B2 | Full affinity in topology/record/assembly/local validator; linked mask check and physical MPIDR |
| B3 | Unique aligned cell checked against all CPU descriptions, including disabled CPUs; negative tests |
| B4 | DT startup contract and finalized map preserve firmware reservations before allocation; fresh boot evidence |
| B5 | Named backend and documented entry profile; physical EL/entry SCTLR/MIDR checkpoint |
| B6 | Documented firmware/coherency trust profile; CPU identification before SMPEN read; physical observations |
| B7 | Pre-stack EL/regime/identity checks and bounded rejection; linked positive/negative checks |
| B8 | PSCI/shared-cell rejection; one consuming release and one changed physical cell |
| A1 | One production record/mailbox ABI; duplicate preparation handoff removed |
| A2 | Validated immutable identity, stack, context, translation and nonzero generation |
| A3 | One-shot raw-field initialization, acquired references and concurrent storage tests |
| A4 | Compile-time offsets/sizes/alignments; assembly operands checked against final ABI |
| A5 | Ready/Entered/Arrived/Parked/Failed; safe Rust failures recorded; unsafe early rejection uses holding/timeout |
| A6 | Twenty local fields, four entry observations and 28 named validation bits; physical checkpoint |
| A7 | Static record/context/tables and retained allocated stacks; late arrival and guest regression |
| A8 | Real table/object/guard validation; literal-free linked MMU transition and private stack evidence |
| C1 | Startup follows topology/map/allocator/stacks/heap/vectors/console; connected hardware logs |
| C2 | Private resource/freeze/publication handles enforce prerequisites before the only firmware store |
| C3 | Whole reserved-page host RW/NX Normal identity mapping; PA-zero support; no new guest mapping |
| C4 | Validation/freeze share the table lock; mutation rejection tested |
| C5 | Validated generation-1 record published once; invalid/stale/reuse tests |
| C6 | Checked cache rounding and complete code/vector/rodata/writable/table/stack coverage; ordering/failure tests |
| C7 | Zero recheck, LE64 store, clean, DSB, SEV; linked width/order and physical cell readback |
| C8 | Pre-store timestamp, checked two-second deadline, finite polls; measured physical timeout |
| C9 | Acquired Parked checkpoint and consuming permit; physical failure blocks guest |
| C10 | Reviewed mapping/allocator/publication/wait/report scopes; no resource/UART lock held during wait |
| S1 | Stackless trampoline, private SP/TPIDR/VBAR and LDAR before Rust; linked checks and checkpoint |
| S2 | Shared named host translation/control policy; exact common instruction check |
| S3 | Explicit trap/FP/timer/translation setup; synchronized order and scoped architecture/errata review |
| S4 | Complete validator before arrival; failure visibility tests and zero-failure hardware checkpoint |
| S5 | Context identity/null-idle metadata checks only; no secondary VM/vCPU/emulation dereference |
| S6 | Arrived then Parked and permanent masked WFE; no Online/IRQ enablement |
| F1 | Preflight errors precede store and stop boot; callback failure tests and connected error path |
| F2 | Checkpoint then Failed; acquired CPU0 report and SecondaryFailed; visibility tests |
| F3 | Typed timeout, last safe worker state and retention; physical timeout/late checkpoint |
| F4 | First-outcome CAS and independent boot terminal; late Parked/Success cannot replace timeout |
| F5 | Zero/stale/duplicate/concurrent generation and transition tests on production protocol |
| F6 | One-shot boot gate; no clear/free/CPU_OFF/release retry; rejected hardware retry probe and reset cleanup |
| V1 | Backend/target/contract preflight tests, including missing/unsupported/shared cases |
| V2 | Production record validation/concurrency, ABI assertions and bounded assembly accesses |
| V3 | Acquire visibility, bounded waits, wrap/stall/frequency cases and terminal preservation tests |
| V4 | Warning-free builds; integrated source/architecture and linked positive/negative reviews |
| V5 | Actual image hashes, dirty-source snapshots and fresh board/DT/layout evidence |
| V6 | Task 12 physical arrival/park and other cells zero, repeated across four Task 14 boots |
| V7 | Task 13 documented delay, bounded timeout, rejected late success/retry and retained state |
| V8 | Task 14 complete quick profile, full-duration soak and post-soak boot/probe |
| V9 | Indexed logs, binaries, source/configuration provenance, thresholds and independent final power-off |

## Review bundle and reproduction

The local archive is `docs/evidence/phase11_3_task15/review-bundle.tar.gz`.
Its adjacent `manifest.json` records archive SHA256, base revision, dirty state,
source/document snapshots, acceptance-index hashes and the fresh final relay
query. Original acceptance artifacts keep their repository-relative paths.
Kernel build intermediates and unrelated/private files are omitted. Use this
review bundle with the recorded base checkout; it is not a standalone repository.

| Evidence | Retained directory |
|---|---|
| Task 11 local acceptance | `docs/evidence/phase11_3_task11/` |
| Task 12 preparation/U-Boot/power-on | `docs/evidence/phase11_3_task12/` |
| Task 12 executed arrival image | `docs/evidence/phase11_3_task12_readback/` |
| Task 13 executed failure image | `docs/evidence/phase11_3_task13/` |
| Task 14 full quick regression | `guest/build/phase113-task14-20261031/` |
| Task 15 snapshots/integrity/cleanup | `docs/evidence/phase11_3_task15/` |

Generated evidence is ignored by Git and must accompany the review separately.
The initial Task 12 image was superseded before execution for readback diagnostics;
it was not a failed hardware run. Task 13 deliberately fails startup with a
passing test verdict. Assessments exclude historical UART snapshots and resolve
prompt-wait notices using fresh output and unique successful command markers.

Normal-build local reproduction:

```sh
cargo fmt --check
cargo test --locked --offline --lib --target aarch64-unknown-linux-gnu
cargo build --locked --offline
cargo build --locked --offline --release
python3 scripts/checks/secondary_entry.py target/aarch64-unknown-none/debug/tvisor
python3 scripts/checks/secondary_entry_negative.py target/aarch64-unknown-none/debug/tvisor
```

Use the documented [failure feature](phase11_3_failure_acceptance.md) only for
its deliberate test. Full quick reproduction follows the tvisor-regression skill,
preserving the normal Image and using a new run directory/unique TFTP filenames.
Task 15 needed no new hardware run; its final relay query was read-only. Hardware
tests used temporary TFTP artifacts and RAM-only guests. No U-Boot environment
was saved and no SD-card write was performed.

## Remaining scope

The firmware/BCM2711/A72 profile remains a documented trust boundary, not
authenticated firmware or independent proof of global coherency. The observed
MIDR is A72 r0p3; see Task 11's scoped errata assessment. Other firmware/entry
regimes need revalidation or an adapter.

No Phase 11.3 task remains open. Phase 11.4 management SGIs/local GIC readiness,
11.5 all-core startup/concurrent allocator work, and 11.6 mapping changes/TLB
shootdowns remain future milestones. Guest SMP/migration and deferred issue #29
remain outside this claim. CPU1 is Parked, not Online; this validation does not
establish all-core operation, a global stop protocol or an overnight pass.
