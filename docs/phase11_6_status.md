# Phase 11.6: EL2 mapping updates and the final physical SMP gate

Phase 11.5 is accepted. Phase 11.6 implements a serialized EL2 stage-1
scratch-page update protocol using the existing per-CPU contexts, management
channels and worker loops. The diagnostics/build-configuration cleanup passed
all local and hardware regression checks, including the full six-hour physical
SMP run, which completed on 2026-11-03 at 04:30 JST. Final power-off was verified.
These results satisfy the Phase 11.6 implementation/regression gate; user review
and acceptance remain separate.

## Runtime scope

In TLB acceptance images, before secondary release and table freeze, CPU0
reserves two physical pages and the complete table path for one Normal, inner-shareable, writable, non-executable
4 KiB mapping at VA `0x1_0000_0000`. The backing pages and table pages remain
allocated until reset. Ordinary table editing remains frozen. The only runtime
exception accepts this fixed scratch leaf, its exact old descriptor, and the
reserved backing pair; it cannot allocate tables, split blocks or alter code,
stacks, vectors, devices or guest memory.

CPU0 coordinates updates under the explicit host HCR profile, with the guest
deactivated. Guest-active calls are rejected before starting an epoch. Workers
remain host-only. Guest stage-2 changes and guest SMP remain Phase 12 work.

Normal images activate all eligible CPUs, park and ping every worker, and enter
Linux. They keep EL2 mappings frozen and do not reserve scratch backing or run
remapping self-tests. Parked workers retain their EL2 regime and remain
responsive to management SGIs. `src/tlb_test.rs` contains the opt-in scratch
driver; the shared update state and break-before-make table primitives remain
in the reusable utility modules.

`diagnostics` enables verbose startup/resource messages in either debug or
release builds. It is off by default. All hardware acceptance features enable
it automatically. `diagnostics` alone does not enable scratch allocation or
remapping. Fatal reports and boot validation remain active in quiet builds;
failed secondary startup still dumps its checkpoint. Successful remap records
are compiled only with `tlb-update-test` or `tlb-update-timeout-test`.

## Update protocol

`tlb_update::UpdateState` contains one persistent gate, an increasing epoch,
the member mask, a phase and per-owner admission/quiescence/synchronization
epochs. The gate excludes other updates and serializes Park/FatalStop lifecycle
commands against the membership snapshot. Membership is the validated enabled
topology, including CPU0 and all Online or Parked workers. An unavailable or
stopped member fails the update; it is never silently removed. No hotplug or
new CPU-specific startup path is introduced.

1. CPU0 acquires the update gate and validates actual retaining members. It
   initializes the alternate reserved page with the new epoch signature.
2. Sequence-numbered `TlbQuiesce` SGIs admit the epoch into existing per-CPU
   channels. The IRQ handler performs a bounded admission operation and returns;
   it does not wait, allocate, log or edit a translation table.
3. Each ordinary worker reads the old scratch alias, executes local DSB/ISB,
   publishes its quiescent epoch and stops accessing that alias. CPU0 also
   reaches this safe point. No allocator, console or table lock is held while
   waiting for another CPU.
4. After every member is quiescent, CPU0 locks the shared table and performs
   break-before-make. It stores an invalid descriptor, calls the linked
   maintenance helper, stores the replacement descriptor and calls the helper
   again. Each helper call executes:

   ```asm
   dc   cvac, x0       // descriptor's identity-mapped address
   dsb  ish           // complete descriptor publication and clean
   tlbi alle2is       // EL2 inner-shareable broadcast
   dsb  ish           // architectural invalidation completion
   isb
   ```

5. CPU0 releases the table lock and publishes the synchronization phase.
   Every member executes local synchronization, reads the new signature through
   the scratch VA and publishes its synchronization epoch. A worker performs
   no further scratch access after this acknowledgement, so it can immediately
   return to the holding loop without missing the next epoch.
6. Only after all synchronization epochs match does CPU0 complete the update
   and repurpose the old backing page. Acceptance deliberately poisons that
   page to expose stale translations during the next update. Neither backing
   page is returned to the allocator, and no table page is reclaimed.

SGI admission is a software receipt. Quiescence is a later owner safe point.
Broadcast TLBI completion is supplied by the architectural maintenance sequence;
it is not inferred from SGI acknowledgements. Local ISB/read acknowledgements
then verify that all retaining owners can use the replacement translation.
The sequence follows Arm's description of descriptor publication,
break-before-make, broadcast domains and local synchronization in
[AArch64 memory management, sections 8.1–8.3](https://documentation-service.arm.com/static/68e680a2587fd957f5efd742).
The second broadcast is conservative and keeps publication identical after
both descriptor stores.

Production waits have a physical-counter deadline and a bounded poll count.
After an epoch starts, any missing member, failed acknowledgement, signature
mismatch, invalid descriptor or timeout is terminal: mark the protocol Failed,
record the shared fatal condition, notify peers and halt. There is no rollback,
retry, guest continuation or reclamation after a partially completed update.
Late completion cannot make a failed epoch reusable. Backing pages, tables and
per-CPU resources remain retained until reset.

## Acceptance coverage

| Image/runner mode | Required result |
|---|---|
| Normal / `quiet` | Full four-boot quick guest regression with acceptance messages absent; no UART-based SMP gate claimed |
| `diagnostics` / `bringup` | Responsive Parked workers, single-vCPU Linux and console checks; no scratch remaps |
| `tlb-update-test` / `mapping` | 32 Online and 16 Parked remaps, exact epochs/signatures, old-page poisoning, unchanged allocator baseline, 35-second Linux timer/memory probe |
| `tlb-update-timeout-test` / `mapping-timeout` | CPU2 reaches quiescence late; failure stays terminal, late acknowledgement rejected, old mapping/backing retained and guest entry blocked |
| `all-core-stress-test` / `stress` | Existing all-core allocator/heap/logging/IPI workload, including workers running alongside Linux's 120-second probe |
| `diagnostics` / `quick` | Four cold boots, 35-second smoke, full 600-second soak and post-soak probe |
| `diagnostics` / `overnight` | Eleven cold boots, 35-second smoke, full 21,600-second soak and five post-soak probes |

Each successful diagnostics-enabled SMP boot must contain the unique per-CPU resource and
bidirectional management checks, followed by an observed physical SMP gate.
`online_mask` counts initialized retaining CPUs, including Parked CPUs;
`parked_mask` is the actual Parked subset. Both are derived from per-CPU runtime
state under lifecycle serialization, rather than copied from topology masks.
On the four-core platform they must be `0xf` and `0xe`. Each parked worker must
also answer a fresh ping. The runner requires the exact remap epoch sequence
recorded by the manifest (zero for new normal/stress images, 48 for mapping
acceptance). Older frozen bundles retain their two-remap expectation. Guest DT
CPU count remains one.

The fault image is opt-in and delays CPU2 for four seconds against a two-second
quiescence deadline. It observes the real late owner, verifies unchanged mapping
and allocator state, and deliberately blocks the guest. Production failure
handling remains fatal. Host tests also cover failed break/make callbacks,
missing synchronization acknowledgements, membership exclusion, stale epochs,
sequence exhaustion and repeated epochs with concurrent owners.

## Current verification

The final suite used the staged cleanup on top of `bb294f9`, with immutable
source/artifact hashes, frozen host runners and the existing Linux 6.18.2
fixtures. Evidence is retained in
`docs/evidence/diagnostics_regression_20261102_01/`. No Linux image was rebuilt.
Only this completion report and documentation labels were updated afterward;
implementation and runner source still match the tested snapshots.

All 200 native tests pass in debug, optimized and diagnostics-enabled
configurations. Ten warning-free bare-metal variants pass linked checks:
ordinary debug/release, diagnostics debug/release, mapping, mapping timeout,
allocator stress, management IRQ, single-secondary timeout and all-core timeout.
Quiet ELF checks confirm that success messages and scratch acceptance helpers
are absent while failure diagnostics remain. Entry checks reject eleven
negative mutations; the TLBI helper check rejects nine. IRQ-frame checks cover
Q0–Q31, FPCR/FPSR, exception return and vector/frame layout.

| Hardware check | Final result |
|---|---|
| Scratch remapping | PASS: 48 exact epochs, 32 Online and 16 Parked; unchanged allocator baseline; Linux probe passed |
| CPU2 timeout | PASS: real late acknowledgement rejected, old mapping/backing retained, guest entry blocked |
| Concurrent Linux stress | PASS: both allocator baselines restored; 120.323-second guest probe, 428 wakeups and 1,969 memory passes |
| Quiet default-image quick | PASS: all four boots, acceptance output absent, 600.461-second soak, smoke and post-reboot probes |
| Diagnostics-enabled physical SMP overnight | PASS: all eleven boots and full 21,600.334-second soak; physical SMP gates passed on every boot |

The quiet soak recorded 2,136 wakeups, 8,135 memory passes and maximum lateness
7.573 ms. The overnight soak recorded 76,832 wakeups, 289,655 memory passes and
maximum lateness 50.007 ms. Neither had any wakeups over 500 ms late. Overnight
completed 379 tagged guest commands and 360 independent console/IRQ checks;
timer IRQ samples increased strictly from 20,800 to 5,405,184. All eleven fresh
UART boot segments contained the required physical SMP gate and no fatal
markers. The suite stopped successfully after verified power-off, independently
confirmed through the controller. Guest CPU count remained one.

Earlier manual boots confirmed the quiet default output. The first overnight
attempt was interrupted at the user's request for console inspection; it is
retained as incomplete and is not counted as the completed six-hour pass above.

The first hardware pilot is preserved in
`docs/evidence/phase11_6/mapping-02/`. It failed in `power_on_uboot`, before any
tvisor artifact was transferred or executed: relay confirmation completed after
the firmware countdown, so the controller missed the fresh countdown. The
runner verified final power-off. This is a **harness failure**, not evidence of
either success or failure of the new hypervisor protocol. The separate rpictl
daemon needed its countdown capture armed before applying power. That attempt
provided no hardware acceptance evidence.

The controller fix was subsequently reviewed, merged as controller revision
`b72a334`, installed and verified against the running executable. Its 30 tests
pass. On hardware, `hardware-mapping-02` passed all 48 remaps and a 35.987-second
Linux probe (128 wakeups, 285 memory passes, maximum lateness 7.625 ms, no wakeups
over 500 ms late). `hardware-timeout-01` passed late-acknowledgement rejection,
resource retention and blocked guest entry. `hardware-concurrency-01` passed
both allocator baselines and a concurrent 120.319-second Linux probe (428
wakeups, 1,968 memory passes, maximum lateness 7.625 ms, no wakeups over 500 ms).
Each completed verified power-off. The full quick run also passed; the six-hour
run was later interrupted for the requested default-console inspection. Those
initial results describe frozen revision `bb294f9`, before normal-image scratch
tests were removed by this cleanup. The final staged-cleanup results above
come from a separate complete suite.

The first updated-controller attempt, `hardware-mapping-01`, is retained as a
harness failure: relay queries timed out before any tvisor load. A bounded
read-only diagnostic and a subsequent controller query verified recovery and
power OFF. The fresh successful attempt uses a separate evidence directory.

The stability runner now distinguishes a known RAM-only guest from a board that
failed before guest launch. If the latter has booted the SD-card OS, cleanup
uses graceful shutdown; unknown state refuses a forced power cut. This prevents
the controller failure from turning into unsafe cleanup.

## Reproduction

Prepare each attempt in a fresh directory, reusing the immutable Linux fixture:

```sh
python3 scripts/checks/phase115_prepare.py --fixture EXISTING_QUICK_BUNDLE \
  --output NEW_BUNDLE --phase 11.6 --kind mapping
```

Kinds are `normal`, `mapping`, `mapping-timeout` and `stress`. Mapping and stress
use optimized builds. Normal bundles are quiet by default. Add `--diagnostics`
for physical SMP resource/park checks; these do not enable scratch tests. Add
`--mode overnight --kind normal --diagnostics` for the final six-hour SMP run.
Preparation verifies frozen Linux/config/probe hashes, builds
tvisor, checks linked entry instructions, diagnostic isolation and (for TLB
acceptance) TLBI instructions, and preserves source hashes
and copies including untracked implementation files. It does not rebuild Linux
or touch hardware. Publish only the three manifest artifacts to the existing
TFTP server for an authorized hardware run, then run:

```sh
python3 scripts/checks/phase116_acceptance.py NEW_BUNDLE \
  --mode mapping --authorize-hardware
```

Run mapping, timeout, concurrent stress and quick pilots before overnight.
Keep the final run in a bounded persistent service and require completed soak,
all post-soak boots, physical SMP checks and verified power-off before declaring
the Phase 11 final gate passed. Preserve failed attempts separately.
