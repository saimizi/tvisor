# Phase 11.3 boot integration

Task 10, 2026-10-31. **Supported-board builds now attempt one secondary release
before entering Linux.** This is an implementation milestone, not a hardware
acceptance PASS. No board was operated for this task. Tasks 11–15 remain.

## Boot order and backend selection

`post_switch_page_tables` retains its existing once-only sequence: private EL2
and vectors/console, topology/startup-memory discovery, finalized memory-map
publication and reservation checks, physical allocator, guarded secondary stacks
and complete RAM mappings, then heap initialization. Only after those steps does
it reconstruct the shared translation policy from the expected takeover values
and call `secondary::start_one`.

The new production `LabRpi4Backend::select` requires both root compatibility
strings `raspberrypi,4-model-b` and `brcm,bcm2711`, a primary MIDR identifying Arm
Cortex-A72, the matching logical boot affinity, and enabled CPU affinities inside
one cluster with Aff0 below four. Enabled DT CPU nodes must identify Cortex-A72
and agree in count with the owned topology. A PSCI description rejects this
armstub-only profile; no conduit is probed. Target selection and unique-cell
validation remain the production helpers from earlier tasks.

Only successful selection can produce the named backend object and its entry/
coherency contract. This is explicit selection of the supported firmware trust
profile in [the backend contract](phase11_3_backend_contract.md), not firmware
authentication or a measurement of another core's live registers. There is no
runtime firmware hash allowlist or fixed release-address requirement. Unsupported
platforms, CPU descriptions, clusters, interfaces and unavailable targets fail
before any release write. Initial logs identify the selected profile, primary
MIDR/MPIDR, target logical ID/full affinity and DT cell.

## Validated physical deadline

`StartupDeadline` validates nonzero 32-bit CNTFRQ, nonzero duration, checked
frequency × seconds and an unambiguous duration below half the 56-bit range.
The production duration is two seconds. Before mapping/publication/release,
a finite 100,000-read probe must observe counter progress; a stalled or backwards
counter fails preparation rather than permitting an unbounded wait.

The system-counter width is not assumed to be 64 bits. The hardware wait uses
`(now - start) & ((1 << 56) - 1)` and passes elapsed ticks to the production
mailbox waiter. This handles the low-56-bit wrap on both 56-bit and wider counters.
For the architecture's minimum width rationale, see the primary
[Linux architectural timer source](https://linux.googlesource.com/linux/kernel/git/torvalds/linux/+/1200d84f4c0a929a0780180d25063d93773be79c/drivers/clocksource/arm_arch_timer.c).
The wait still has a finite 100-million-poll fallback and distinguishes poll
exhaustion from measured deadline expiry.

The release-store primitive now executes ISB, samples CNTPCT into x2, stores
the entry with STR, and returns the sample to CPU 0. Consequently the deadline
origin is immediately before the irreversible write, not the later start of
polling. It includes cell cleaning, completion and SEV. An actual timestamp of
zero is valid because the hardware adapter stores it as `Some(0)`. Frequency
conversion is not repeated or redefined after release.

## Terminal boot state and guest permit

`secondary_boot::SecondaryBoot` owns one atomic attempt state for the whole boot:

| State | Meaning |
|---|---|
| Idle | No secondary startup attempt has begun |
| Preparing | Backend/target/resource/publication work in progress |
| ReleaseAttempted | The consuming release operation is being attempted |
| Success | Matching validated Parked evidence permits guest entry |
| PreparationFailed | An error returned before a release attempt |
| ReleaseFailed | Release operation returned an error; it may already be irreversible |
| WaitFailed | Waiting or final checkpoint/fatal checks failed |

A compare-exchange grants exactly one attempt. Its RAII guard makes early returns
terminal: dropping Preparing becomes PreparationFailed; dropping ReleaseAttempted
becomes ReleaseFailed. Explicit waiting/checkpoint failure becomes WaitFailed.
Successful completion leaves Success untouched. No retry/reset API exists.
Initialization errors before this attempt still follow the existing fatal boot
branch and do not reach the guest.

The mailbox gains terminal outcome 6, BootAborted, for a published attempt that
fails before obtaining a normal wait outcome. The outer caller tries to publish
that result on error; its CAS preserves any earlier timeout/failure/success
checkpoint result. The persistent boot state is authoritative for guest gating,
including a fatal error discovered after an otherwise valid startup checkpoint.
Late worker evidence cannot revive either terminal result or reclaim resources.

After the wait succeeds, completion rechecks generation, validated arrival,
Parked state, successful mailbox outcome, zero validation failure bits and the
acquired global fatal flag. Only that path creates a private, non-Clone
`GuestPermit`. Boot consumes it to invoke `run_guest` once. All preparation,
publication, release, wait, stale-checkpoint and fatal failures print an explicit
CPU 0 diagnostic and branch to the existing permanent holding loop. No failure
branch calls the guest or frees the secondary's record/context/stack/tables.
Reset/power-cycle remains the recovery boundary. Warm re-entry or reloading
another image while a secondary is retained is unsupported.

All publication and waiting remain outside table/allocator/UART locks. The
secondary stays masked and parked; CPU 0 alone owns the one-vCPU guest and its
subsequent interrupt behavior. The guest's separate stage-2 tables do not thaw
the now-frozen host stage-1 mappings.

## Local verification

- 166 hosted tests pass. Six new tests cover supported/rejected backend profiles,
  one-use guest permits after validated success, terminal preparation/release/
  wait errors, missing/stale/failed/fatal/bad checkpoints, aborted boot with late
  arrival, deadline conversion and counter progress/wrap.
- Debug and optimized bare-metal builds pass **without warnings**; the formerly
  unused startup path is now connected.
- Both linked-entry/publication checks pass. The checker resolves relaxed ADR
  and unrelaxed ADRP/ADD address formation. The larger debug image uses the latter
  for the record; the checked address still matches the actual static symbol.
- The store primitive's checked instruction sequence now includes the pre-store
  timestamp, STR, result move and RET. The other publication primitives retain
  their exact checked operations.
- Formatting and whitespace checks pass. No hardware success, deliberate failure
  or parked-secondary guest regression result is claimed by these local checks.

Task 11 must assess the final connected binary and remaining architecture/build
requirements. Tasks 12–14 must demonstrate physical arrival/parking, bounded
failure and guest regression with the secondary parked; Task 15 assembles review
evidence. Phase 11.2 remains the last approved hardware implementation gate.
