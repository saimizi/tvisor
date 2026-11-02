# Phase 11.5: all-core bring-up and concurrent resource acceptance

Phase 11.4 is accepted. Phase 11.5 implements the three steps agreed for this
milestone: shared all-core startup, concurrent resource tests, and hardware
acceptance with the single-vCPU Linux guest on CPU0. User acceptance remains a
separate review decision. EL2 mappings stay frozen; broadcast TLB synchronization
and subsequent mapping changes belong to Phase 11.6.

## 11.5.1 — Shared all-core startup

The enabled topology determines a required-secondary mask. Every enabled CPU
must have one unique bounded logical slot, including CPU0; invalid topology
fails before release. On the validated four-core platform the mask is `0xe`.
Logical IDs select slots and channels; full MPIDR affinities route entry, and
banked GIC discovery supplies interrupt target masks.

`secondary::start_all` reuses the Phase 11.3 stackless trampoline, `PcpuSlot`,
shared host configuration, entry validation, publication and consuming release
handles. CPU0 prepares every required CPU and its generation before validating
the complete resource set and sealing the shared EL2 tables once. Every cache
publication finishes before the first release. CPU1–CPU3 then use the same
release loop and independent physical-counter deadlines. Release-cell readback
checks the cumulative released set; every other firmware cell must remain zero.

Each CPU has its own one-shot boot attempt and a validated Parked receipt bound
to its logical identity. The aggregate boot gate consumes exactly the required
receipts: a missing, additional or incorrectly placed receipt, or a recorded
fatal failure, prevents guest authorization. Early return finalizes the attempt;
resources and firmware release cells remain retained until reset. A late Parked
checkpoint cannot reverse a terminal boot decision.

Management activation iterates that same required set. Each owner initializes
its existing banked GIC interface, becomes Online and pings CPU0. CPU0 validates
bidirectional ping, bounded arithmetic and oversized-job rejection for each
worker, with IRQ enabled during waits and no resource lock held. All discovered
GIC targets must be distinct. CPU2 and CPU3 have no separate entry, stack,
mailbox, GIC or interrupt-dispatch implementation. Normal images leave all three
workers responsive in the existing IRQ-safe holding loop while Linux runs.

## 11.5.2 — Concurrent tests

The opt-in `all-core-stress-test` image performs two one-shot phases:

| Phase | Participants | Rounds per CPU | Activity |
|---|---|---|---|
| Before guest preparation | CPU0–CPU3 | 256 | simultaneous allocations, payload checks, logging and all-to-all pings |
| From first actual guest entry | CPU1–CPU3 | 640, paced over 64 seconds | same workload plus worker-to-CPU0 pings while Linux executes |

Each round holds eight physical pages and four heap objects of different sizes,
aligned to 64 bytes. Workers publish their live ranges and rendezvous before
any payload write. Independent validation rejects null, overflowing, misaligned,
duplicate, nested or partially overlapping ranges across all participants and
both allocators. After filling distinct per-CPU/per-object/per-round patterns,
workers rendezvous, verify every byte, rendezvous again, then release resources
in alternating orders. Published ranges remain stable through all peer reads.
Every fourth round sends a ping to each other participant, including CPU0 during
the guest phase. Every 32nd round exercises serialized formatted EL2 logging.

Management `RunTest` acknowledges admission into fixed one-shot storage. The
SGI handler does not execute the workload, allocate, log, wait for a barrier or
wait for another CPU. The ordinary worker loop consumes the admitted job with
IRQ enabled; a separate Release/Acquire completion state indicates that its
allocations have been freed and its final logging is complete. Admission cannot
reuse a completed or timed-out slot. Work and waits have fixed iteration limits
and physical-counter deadlines. Failure records a shared fatal condition,
retains ambiguous resources and halts; no timeout-driven reclamation occurs.

CPU0 samples allocator baselines only at quiescent boundaries. Guest-phase
baselines are sampled after VM construction, at first guest entry, rather than
during allocation-heavy guest preparation. The final check requires exactly the
original physical in-use count, heap used bytes, live allocation count and failed
allocation count. Normal images contain no recurring acceptance workload.

Totals on four CPUs: 23,552 physical allocations, 11,776 heap allocations and
2,208 workload pings, in addition to startup and management smoke checks. This
checks simultaneous test allocations and payload ownership; it is bounded
acceptance coverage, not an exhaustive proof of arbitrary allocator histories.

## 11.5.3 — Hardware acceptance and regression

The optimized acceptance image passed both phases on physical hardware. All
four affinities, contexts, stacks and GIC targets were distinct; owner checkpoint
TPIDR/SP values matched their prepared resources and local register validation
reported zero failures. Both allocator baselines were restored. Linux's
120-second timer/32 MiB memory probe overlapped worker activity and passed:
428 wakeups, 1,962 memory passes, maximum scheduling lateness 7.624 ms and zero
wakeups more than 500 ms late. Subsequent console and timer-counter reads passed.
All 92 expected EL2 progress records were complete and each per-CPU round
sequence was exact. Linux emits individual console characters, so an EL2 record
can follow a partial guest line; whole guest lines are not an atomic logging
unit. Power-off was verified after the run.

The initial unoptimized stress pilot exceeded the harness's 60-second Linux
boot wait while all four CPUs were still progressing through boot stress.
That attempt is retained as FAIL/incomplete; it is not counted as acceptance.
The optimized run is a separate attempt with a separate source/artifact bundle.
Use the optimized acceptance build for this timed workload.

The middle-CPU timeout image passed. CPU1 remained Parked; CPU2 exceeded the
108,000,000-tick (two-second) production budget, then arrived Parked during the
bounded observer. CPU3 remained Prepared and unreleased. The aggregate policy
stayed WaitFailed, a pure late readiness wait succeeded without authorizing the
guest, retry was rejected, and the original slot/context/stack and release-cell
values remained intact. The released mask was `0x6`; the CPU3 cell stayed zero.
The board was verified off after this test.

The full normal-image quick regression passed all four cold boots, each with
fresh all-core resource checks, 18 tagged guest commands and 10 independent
console/timer checks. The 35-second smoke, full 600.457-second soak and final
5.628-second post-reboot probe passed. The soak recorded 2,136 wakeups, 8,132
32 MiB memory passes, maximum lateness 7.575 ms and zero wakeups over 500 ms
late. Sampled timer IRQ counts increased from 20,791 to 155,833. Fresh boot
segments contained no fatal markers. Final power-off was independently verified.
No overnight regression was run; these are the scoped Phase 11.5 acceptance and
full quick results, not a claim of Phase 11.6 or guest SMP completion.

## Reproduction and evidence

Build normal tvisor with `cargo build`. Build acceptance with
`cargo build --release --features all-core-stress-test`; build the middle-CPU
failure image with `cargo build --features all-core-timeout-test`. The latter
delays CPU2's Parked checkpoint for four seconds against the production two-second
startup deadline. The earlier `secondary-startup-timeout-test` retains its
CPU1-only fault profile. Run acceptance features separately; the older
`management-irq-test` instrumentation remains restricted to CPU1 while all
ordinary management workers are online.

Native tests use `cargo test --lib --target aarch64-unknown-linux-gnu` on the
AArch64 development host; optimized host tests and acceptance-feature host tests
are also run. The linked entry checker and all eleven negative mutations verify
that generalized orchestration preserves the assembly boundary. The existing
IRQ-frame audit checks Q0–Q31, FPCR/FPSR, the 816-byte frame and exception return.

Prepare a new bundle without rebuilding Linux:

```sh
python3 scripts/checks/phase115_prepare.py --fixture EXISTING_QUICK_BUNDLE \
  --output NEW_BUNDLE --kind normal --diagnostics
```

`--kind stress` selects the optimized stress feature; `--kind timeout` selects
the CPU2 fault feature. The helper verifies the frozen images, configurations,
probe and runner, builds the selected feature immediately before copying its
ELF, checks linker bounds, runs linked checks, and snapshots all participating
source including untracked implementation files. `--dry-run` validates inputs
and shows the build command without creating a bundle. Publication and hardware
execution remain separate actions.

`scripts/checks/phase115_acceptance.py DIRECTORY --mode MODE --authorize-hardware`
consumes a fresh quick-format stability manifest, already published under its
unique artifact names. Modes are `bringup`, `stress`, `timeout` and `quick`.
It uses the existing regression daemon IPC and cleanup, validates fresh board
layout and network readiness before transfers, and assesses only the current
unique image invocation rather than historical serial-buffer contents.
`quick` preserves the full standard four boots, 35-second smoke, 600-second soak
and final five-second probe, adding all-core resource checks after every boot.
Run through a bounded service with sufficient cleanup time as described in the
hardware regression skill. It never writes the SD card or saves firmware state.

Ignored local evidence lives under `docs/evidence/phase11_5/`: `bringup-01`,
`stress-02` (incomplete debug pilot), `stress-03` (optimized acceptance),
`timeout-01`, `quick-03` and `final-checks`. Prepared-only directories are not
hardware passes. The manifests retain frozen Phase 11.4 Linux fixtures and
probe hashes, exact tvisor ELF hashes, dirty source snapshots/patches and runner
provenance. Linux was not rebuilt. Full UART/event logs and separately verified
power-off evidence are retained for review. No commit or push is implied by
completion of this implementation.
