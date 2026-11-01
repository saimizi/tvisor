# Phase 11.3 CPU lifecycle and waiting policy

2026-11-01: implementation, local checks, physical timeout/late-parking acceptance
and the repeated full quick regression passed. Ready for implementation review. This supersedes the
mailbox outcome/worker protocol in the earlier Phase 11.3 reports. The per-slot
storage architecture remains in place.

## Three separate responsibilities

`SecondaryMailbox` no longer contains `outcome`, and `Outcome`/`WorkerState` are
removed. It contains a publication generation, one atomic `CpuState`, and a
private atomic Boolean guarding the unique diagnostic writer. None stores a
waiting-operation result. The entry claim is not an additional lifecycle state:
it prevents two entrants from writing the same checkpoint while Starting.

| Lifecycle | Meaning and transition owner |
|---|---|
| Offline | Context not initialized |
| Prepared | CPU0 initialized immutable context fields and release-published preparation |
| Starting | CPU0 reserved the one-shot startup request; firmware release completion is not implied |
| Parked | The claimed secondary validated its local environment and published its checkpoint before holding |
| Online | Reserved for future management/IRQ readiness; no transition API in Phase 11.3 |
| Failed | The claimed secondary reported an actual initialization failure and diagnostic checkpoint |

The implemented transitions are Offline → Prepared → Starting → Parked/Failed.
There is no retry/reset/CPU_OFF transition. Only the private, non-cloneable
`EntryClaim` returned by `claim_entry` may publish Parked or Failed. Duplicate
claims and stale generations fail without changing diagnostics. Dropping a claim
leaves Starting; a waiting CPU cannot infer successful or failed initialization.

The old prepared Boolean in `PcpuSlot` is removed. Its lifecycle publication now
gates immutable context snapshots; the remaining preparation-claim Boolean
prevents repeated initialization. CPU0's slot uses this for context preparation;
the secondary-start protocol does not manage the running boot CPU's execution.

## Publication and one-shot release

Preparation and the nonzero generation are separate publications: Prepared makes
context identity/stack fields readable, while ready makes the selected startup
resources available. CPU0 still publishes shared EL2 configuration, validates and
freezes host mappings, and cleans every required dependency before release.
Only logical CPU1's ready generation is published by production boot.

The consuming `PublishedStartup` handle retains the matching acquired slot view.
Its release operation changes Prepared → Starting exactly once, then executes
DSB SY before the first firmware-cell read. The sequence continues with the zero
recheck, pre-store physical timestamp, LE64 store, cell-line clean, DSB and SEV.
An already Starting/Parked/Failed slot cannot issue a second firmware operation.

If a callback fails before or after the store, the CPU may remain Starting:
neither Starting nor an I/O error proves whether firmware entered the CPU. Boot
policy fails separately and retains resources. No error clears the cell or
rewrites lifecycle to pretend a CPU failed. Host callback tests cover every
release stage, including failure before any cell store and after a possible store.

## Pure waiting and terminal boot policy

`wait_until_parked` returns `Result<(), WaitError>` with Timeout, CpuFailed,
PollLimit, InvalidDeadline or InvalidGeneration. It only reads publication and
lifecycle state; a timeout, invalid argument or exhausted poll budget changes
neither lifecycle nor entry ownership. A fresh wait can succeed after an earlier
timeout. Multiple waiters can independently return different results.

The physical deadline, counter-wrap handling, checked frequency conversion,
finite poll fallback and clock bracketing of the acquired Parked observation
are preserved. Checkpoint fields are read only after acquiring Parked or Failed,
not from partially written diagnostics while Starting.

Phase 11.3's `SecondaryBoot` caller chooses fail-stop policy. Its
`Attempt::wait_for_parked` wraps the pure wait: errors permanently set WaitFailed;
success records a private receipt for the exact slot address and generation.
`complete` requires that receipt, matching Parked state, zero local failures and
no fatal record before minting the consuming guest permit. Another slot, a stale
generation, direct readiness without a successful boot wait, or a later successful
wait after boot failure cannot authorize guest entry.

The boot controller is independent of per-CPU lifecycle. The genuine sequence
Starting → wait Timeout → later Parked is valid; CPU0's boot policy remains
WaitFailed. A release-policy failure likewise stays terminal even if the CPU
later parks. Optional wait retries do not permit firmware release retries.

Release and readiness waiting are separate APIs, providing a foundation for
future asynchronous use. The current guest-entry gate stays in place; asynchronous
guest boot, Online, CPU2/CPU3 release and concurrent startup remain out of scope.

## ABI and ownership

The stackless full-affinity lookup and coherent Rust boundary are unchanged.
The trampoline reads and LDAR-acquires the publication word, never lifecycle as
a substitute generation. It still rejects unknown/duplicate affinity, a wrong
slot index, invalid bounds or an unsupported firmware entry environment.

| Object/slot offset | Current meaning |
|---|---|
| Slot 0–63 | `PcpuContext`; owner-only mutable fields at 0/8, immutable identity/bounds at 16–48 |
| Slot 64 | Full-width immutable ready generation (`AtomicU64`) |
| Slot 72 | Lifecycle (`AtomicU64`, `CpuState` repr u64) |
| Slot 80 | Unique entry claim (`AtomicBool`), followed by padding |
| Slot 88 / 248 / 280 | Local checkpoint / entry observations / failure mask |
| Slot 320 | Preparation claim; no separate prepared Boolean |
| Slot size/alignment | 384 / 64; complete four-slot array remains 1536 bytes |

The mailbox stays 24 bytes and per-slot diagnostics stay 256 bytes. Shared config
storage remains 128 bytes. Layout assertions and binary checks preserve context
offsets, stack addressing, generation synchronization and complete resource
coverage. Identity snapshots never borrow the owner's whole mutable context.
Resource validation/freeze, cache maintenance, lifetimes and masked permanent
holding loops remain unchanged apart from the added pre-release completion.

## Validation and evidence

170 host tests pass in each of debug/release. Four normal/injected builds are
warning-free. The linked checker passes on each image, and eleven deliberate
corruptions per image are rejected. The added corruption reads lifecycle at
offset 72 as generation instead of ready at offset 64, proving the two fields
cannot be silently interchanged in the trampoline.

Tests cover valid/duplicate lifecycle transitions, writer claims, stale and full-
width generations, late parking/failure, repeated/concurrent waits, counter wrap,
deadline boundaries, poll limits, checkpoint visibility and independent slots.
Boot-policy tests additionally reject guest permits without a matching successful
observation and prove late readiness cannot reopen wait/release-policy failure.

Physical failure acceptance passed: the first two-second wait returned Timeout
at 108,000,004 ticks against 108,000,000 ticks/54 MHz. CPU1 remained Starting with
its entry claimed, then reached Parked with zero local validation failures. A
fresh physical-counter-bounded wait succeeded, while boot policy stayed WaitFailed,
guest entry stayed blocked and the one-shot boot gate rejected retry. Retained
slot/context `0x407bfc0`, stack bounds and shared root matched; other release cells
remained zero. A quiet interval and independent relay-off query completed the run.

The normal-image full quick regression also passed all four cold boots (two
baseline, diagnostic soak and post-soak diagnostic). Each fresh boot observed
CPU1 Parked, generation 1, EL2, MPIDR `0x80000001` and zero local failures. Only
cell `0xe0` contained the linked secondary entry `0x04001000`; the other cells
remained zero. Normal-image context `0x40c7d40`, SP `0x30010a90` and VBAR
`0x408d000` differ from the injected build, as expected from separate links.

The smoke probe ran 35.987 seconds, with 280 memory passes and maximum wakeup
lateness 7.615 ms. The full soak ran 600.477 seconds, with 2,136 wakes, 8,135
memory passes and maximum lateness 11.578 ms. Neither reported a delay over
500 ms. All ten console checks passed with timer interrupt counts increasing
from 20,795 to 156,027. The final short probe passed; all 18 guest commands
returned zero. Total runner time was 13 minutes 47 seconds, including four boots
and verified power-off. A separate subsequent power query independently confirmed
off with the configured relay identity verified.

The actual quick-regression ELF SHA-256 is
`9a64d30393b624dd0f230d4fd0adc91065ca4781b93df7504146cb21f5d22f00`.
The unchanged baseline Linux Image SHA-256 is
`3a72c904e211f56e5f05d601b41495dfa4ec9029b3d7b1ee3556a72f9bb33b15`.
The isolated diagnostic Image SHA-256 is
`55a5046d2b891029d4fb784b74dc4f3c502ee0afc2e4bd17ce7b8d83b8c22749`.
Source manifests verify the exact tested implementation against the current
working tree at base `815594ddbf831c2b63d923e9f9e2382c123b097c`.

The current evidence is in ignored `docs/evidence/phase11_3_lifecycle_refactor/`.
The normal-image quick regression is in
`guest/build/phase113-lifecycle-refactor-20261101/`, including `status.json`,
`report.md`, `phase113-assessment.json`, artifact manifests and raw UART logs.
UART files include historical capture preambles; assessment uses only the suffix
after the final application-start marker for each new boot. Expected rpictl
prompt-observation timeouts are retained; fresh UART and unique command-result
markers establish completion. Earlier bundles are preserved as historical evidence
and are not reused to certify this change. One feature-only helper-name compile
error was fixed before hardware; its original log is retained separately from the
passing build logs.

The local review archive is `docs/evidence/phase11_3_lifecycle_refactor/review-bundle.tar.gz`.
Its adjacent `bundle-manifest.json` records the archive digest; `bundle-contents.json`
indexes individual files. It includes all four checked ELFs, exact source snapshots,
local checks, injected-failure evidence, full quick-regression evidence and this
review documentation. Kernel build intermediates are excluded. These artifacts
are local and ignored by Git. Validation and the archive captured the implementation
and documentation before the review commit.
