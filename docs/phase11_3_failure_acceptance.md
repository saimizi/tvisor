# Phase 11.3 bounded-failure hardware acceptance

## Documented test mechanism

The opt-in Cargo feature `secondary-startup-timeout-test` builds a deliberately
failing image. It is absent from default features and must not be enabled for
normal guest boots or quick regression. Build it in an isolated target directory:

```sh
cargo build --locked --offline --release \
  --features secondary-startup-timeout-test \
  --target-dir target/phase113-timeout-test
```

The normal entry assembly, private stack/context, host mappings, coherency checks,
cache publication, release cell and two-second startup deadline are unchanged.
Only after coherent Rust entry, local validation and checkpoint publication does
the secondary wait four physical-counter seconds before its normal Arrived/Parked
transitions. It keeps D/A/I/F masked and all local timers disabled. This creates
a late acknowledgement without corrupting mappings or simulating noncoherency.
Invalid frequency or a finite poll fallback prevents acknowledgement entirely.

CPU0 must terminate its normal wait with TimedOut while the worker is Entered.
The existing failure path records WaitFailed and attempts BootAborted, which
must preserve the first terminal outcome. Test-only CPU0 instrumentation then
observes for up to five physical-counter seconds (also bounded by polls),
acquire-observes the late Parked checkpoint and reads its diagnostics. It attempts
a late Success outcome and probes the boot gate with `begin`; both must reject.
It never calls the release path again or mints a guest permit.

The observer checks the retained record identity, prepared context and stack
bounds, local failure mask and firmware cells. It prints resource addresses and
a final test PASS/FAIL, then returns to the production startup-error/holding path.
Static/allocated resources and frozen tables remain retained; no reset, cell
clear, unmap or free operation is added. The context/stack check and late physical
execution support retention evidence; allocator/table lifetime also requires the
source ownership review, rather than inferring retention from a log message.

Hardware acceptance requires a new cold boot with fresh DT/layout/cell evidence,
the feature banner, a measured timeout at the unchanged two-second budget, a
late zero-failure Parked checkpoint, rejected late Success and retry, preserved
TimedOut/WaitFailed, unchanged other release cells, no guest-entry/boot marker,
and independent final power-off. Observe a further quiet interval after the
terminal report before reset. Record all images, source provenance and logs.

## Task 13 result

2026-10-31: **PASS on lab-rpi4 for deliberate bounded timeout and late arrival.**
One injected image was executed once; no failed test was retried. This is not a
full quick regression or a completed Phase 11.3 acceptance gate.

Fresh U-Boot evidence confirmed the expected MAC, DTB `0x2eff1f00`, RAM and
reservation layout, four A72 spin-table CPUs, the low-page firmware reservation
and all four cells initially zero. The test image's runtime extent was
`[0x04000000, 0x0409e000)`, clear of guest staging, DTB and U-Boot reservations.
The unchanged normal Linux fixture was loaded solely to keep the usual handoff
inputs; execution never reached guest preparation or entry. No U-Boot environment
was saved and no SD-card write was performed.

| Observation | Result |
|---|---|
| Counter / unchanged deadline | 54 MHz / 108,000,000 ticks (2 s) |
| Wait return sample | 108,000,022 ticks, about 2.0000004 s |
| First terminal state | `TimedOut`, worker `Entered`, boot `WaitFailed` |
| Late checkpoint | CPU1, generation 1, `Parked`, zero validation failures |
| Late Success transition | Rejected with `AlreadyFinished` |
| New boot attempt gate | Rejected with `AlreadyAttempted` |
| Final outcome / boot state | `TimedOut` / `WaitFailed` |
| Release cells | `0xe0=0x04001000`; `0xd8/0xe8/0xf0=0` |
| Guest entry / reset / fatal marker | None |
| Additional quiet observation | 23.223 s, no new UART bytes |
| Final power | Separate relay query: off, identity verified |

The late checkpoint reported EL2, MPIDR `0x80000001`, A72 MIDR `0x410fd083`,
SP `0x30010ec0` inside the original `[0x30001000, 0x30011000)` private stack,
TPIDR/context `0x407bf80`, vectors `0x4029000`, and root `0x408e000`. Translation,
trap and timer fields matched the host policy: DAIF `0x3c0`, HCR `0x80000000`,
CPTR `0x33ff`, CNTHCTL 3, and all three local timer controls zero. The retained
record remained at `0x4079040`; context identity validation still passed.
The CPU0 production error path printed terminal WaitFailed and resource retention
after the observer, then held permanently. Frozen host tables and allocated
stacks were not reset, unmapped or returned to the allocator.

## Local validation and provenance

Formatting and whitespace checks passed. The normal release host test suite and
feature-enabled debug host suite each passed all 166 tests, including existing
production-protocol timeout/late-arrival, first-outcome preservation and denied
guest-permit tests. Both normal and injected target builds passed without warnings
in debug and release. The linked entry/publication checker passed on all four
ELFs; all six deliberate corruptions were rejected for each. Normal ELF bytes
were checked for absence of the timeout-test instrumentation marker.

The frozen bundle is ignored under `docs/evidence/phase11_3_task13/`, including
the feature-enabled ELF, normal control ELF, unchanged guest fixture, manifest,
source snapshots/patch, pre-run mechanism specification, local logs, fresh layout,
transfer results, contiguous UART captures, combined `boot.uart.log`, verdict,
quiet-interval evidence and independent final power query. Manifest hashes cover
the retained evidence/source. The checkout is dirty at base revision
`de73e049e604cf310fe44fabb625042560dfa19b`; nothing was committed or pushed.

- Injected ELF SHA256:
  `1ad75b842e0c1b637feceaa48d84a40518fd4c1ad7b0ffbaeafd30c388bdbda6`.
- Normal control ELF SHA256:
  `c5e820f90766021a19aa0a5277591e532a2f2479db12158f69b85ec3db62e9a2`.
- Unchanged guest Image SHA256:
  `3a72c904e211f56e5f05d601b41495dfa4ec9029b3d7b1ee3556a72f9bb33b15`.

The boot RPC timed out waiting for a prompt, as expected for this holding path;
it was not treated as success evidence. Fresh contiguous, untruncated UART
captures with matching console generation establish the timeout and late
checkpoint. A separate relay-state query establishes power-off despite stale
serial-state classification. Task 14 must cold-boot a normal image without this
feature and complete the full quick regression; Task 15 remains the final review
bundle.
