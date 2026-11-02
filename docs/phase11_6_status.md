# Phase 11.6: EL2 mapping updates and the final physical SMP gate

Phase 11.5 is accepted. Phase 11.6 implements a serialized EL2 stage-1
scratch-page update protocol using the existing per-CPU contexts, management
channels and worker loops. Local checks pass. Hardware acceptance and the
six-hour final regression are **pending**, following a controller failure before
tvisor was loaded. This is not a completed Phase 11.6 acceptance report.

## Runtime scope

Before secondary release and table freeze, CPU0 reserves two physical pages and
the complete table path for one Normal, inner-shareable, writable, non-executable
4 KiB mapping at VA `0x1_0000_0000`. The backing pages and table pages remain
allocated until reset. Ordinary table editing remains frozen. The only runtime
exception accepts this fixed scratch leaf, its exact old descriptor, and the
reserved backing pair; it cannot allocate tables, split blocks or alter code,
stacks, vectors, devices or guest memory.

CPU0 coordinates updates under the explicit host HCR profile, with the guest
deactivated. Guest-active calls are rejected before starting an epoch. Workers
remain host-only. Guest stage-2 changes and guest SMP remain Phase 12 work.

Normal images perform one update with Online workers, park and ping every
worker, then perform another update with Parked workers before entering Linux.
All eligible CPUs are activated through the existing startup path. Parked
workers retain their EL2 regime and remain responsive to management SGIs.

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
| Normal / `bringup` | Online update, responsive Parked workers, Parked update, single-vCPU Linux and console checks |
| `tlb-update-test` / `mapping` | 32 Online and 16 Parked remaps, exact epochs/signatures, old-page poisoning, unchanged allocator baseline, 35-second Linux timer/memory probe |
| `tlb-update-timeout-test` / `mapping-timeout` | CPU2 reaches quiescence late; failure stays terminal, late acknowledgement rejected, old mapping/backing retained and guest entry blocked |
| `all-core-stress-test` / `stress` | Both remaps plus the existing all-core allocator/heap/logging/IPI workload, including workers running alongside Linux's 120-second probe |
| Normal / `quick` | Four cold boots, 35-second smoke, full 600-second soak and post-soak probe |
| Normal / `overnight` | Eleven cold boots, 35-second smoke, full 21,600-second soak and five post-soak probes |

Each successful boot must contain the existing unique per-CPU resource and
bidirectional management checks, followed by an observed physical SMP gate.
`online_mask` counts initialized retaining CPUs, including Parked CPUs;
`parked_mask` is the actual Parked subset. Both are derived from per-CPU runtime
state under lifecycle serialization, rather than copied from topology masks.
On the four-core platform they must be `0xf` and `0xe`. Each parked worker must
also answer a fresh ping. The runner requires the exact remap epoch sequence
for the selected image. Guest DT CPU count remains one.

The fault image is opt-in and delays CPU2 for four seconds against a two-second
quiescence deadline. It observes the real late owner, verifies unchanged mapping
and allocator state, and deliberately blocks the guest. Production failure
handling remains fatal. Host tests also cover failed break/make callbacks,
missing synchronization acknowledgements, membership exclusion, stale epochs,
sequence exhaustion and repeated epochs with concurrent owners.

## Current verification

Native debug and optimized library suites pass all **200 tests**, including
128 mapping epochs with four concurrent modeled owners. These tests check the
software protocol; they do not validate physical TLB behavior. Normal,
mapping-acceptance, timeout and concurrent-stress bare-metal builds pass. The
linked helper checker verifies the exact maintenance sequence and rejects nine
negative mutations. The existing secondary-entry checker and eleven negative
mutations continue to pass.

The first hardware pilot is preserved in
`docs/evidence/phase11_6/mapping-02/`. It failed in `power_on_uboot`, before any
tvisor artifact was transferred or executed: relay confirmation completed after
the firmware countdown, so the controller missed the fresh countdown. The
runner verified final power-off. This is a **harness failure**, not evidence of
either success or failure of the new hypervisor protocol. The separate rpictl
daemon needs its countdown capture armed before applying power. No hardware
acceptance or overnight PASS is claimed.

A proposed controller fix was built in an isolated copy and tested with a
pseudo-terminal and simulated relay. With relay confirmation delayed beyond
the simulated countdown, the original controller timed out and the corrected
controller interrupted autoboot and verified power successfully. The live
daemon and its source remain unchanged, pending authorization to modify the
separate controller project. Fresh final bundles for mapping, timeout,
concurrency, quick and overnight checks are prepared but have not run; their
source and artifact hashes match the current implementation.

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
use optimized builds. Add `--mode overnight` with `--kind normal` for the final
six-hour run. Preparation verifies frozen Linux/config/probe hashes, builds
tvisor, checks linked entry and TLBI instructions, and preserves source hashes
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
