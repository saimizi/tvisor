# Phase 11.4: centralized HCR policy follow-up

2026-11-01: implementation, local checks, management hardware acceptance and the fresh
normal-image full quick regression repeat passed. Ready for user review. Validation
and the local evidence bundle captured the working tree before this review commit.
This addresses [PR #28's HCR policy comment](https://github.com/saimizi/tvisor/pull/28#issuecomment-6556805974).
The initial implementation/evidence is retained in [Phase 11.4 status](phase11_4_status.md).
The wider [IRQ ownership proposal](irq_handling.md) remains separate; its pending
shared-dispatch, containment and lifetime work is not implemented by this change.

## Explicit profiles and transitions

`tvisor_util/el2_policy.rs` owns every HCR bit definition used here and both
profiles. `HOST_HCR = HCR_RW | HCR_IMO` (`0x80000010`).
`GUEST_HCR = HOST_HCR | GUEST_HCR_ADDITIONS` (`0x80080013`), where the additions
are VM, SWIO and TSC. Existing `HCR_EL2_*` names in the stage-2 module are aliases;
`HCR_EL2_STAGE2_VALUE` refers to the centralized guest profile. Adding a shared
host control no longer depends on updating an independent guest bit list.

Guest activation validates its supplied HCR value, explicitly writes the complete
guest profile and checks readback. Full stage-2 deactivation retains the existing
VMID-aware TLBI/barrier sequence, explicitly writes the complete host profile,
clears VTTBR and checks HCR readback. It removes SWIO/TSC as well as VM. CPTR
retains its existing valid shared policy, including its required reserved bits.
No transition inherits arbitrary HCR bits with a read-modify-write operation.

Ordinary IRQ and emulated exits leave the guest context installed, including its
HCR profile; they are not full deactivation. Management dispatch accepts only the
two centralized profiles, and every actual guest entry requires the exact guest
profile. This replaces independent literal routing masks with the policy values.
FIQ/SError routing, TGE, VHE and VI/VF remain disabled under both profiles.

## Validation scope

180 native host tests pass in debug and release. Profile tests verify shared-bit
inclusion, the exact guest-only additions, removal of guest controls in the host
profile and prohibited controls. The stage-2 register-construction test checks the
whole guest profile and every required host-wide bit. Normal and management-test
images build warning-free in both profiles; all four pass the linked trampoline
checker, eleven negative mutations each, and current-EL SIMD/FP frame checks.

The existing `management-irq-test` feature now exercises production activation
and deactivation with the actual retained guest stage-2 root, before guest
execution. It checks guest HCR/VTTBR readback, host HCR restoration and VTTBR zero,
then services a worker-originated SGI at EL2 after deactivation. It repeats the
activation/deactivation cycle to check reuse, before the normal Linux activation.
Temporary UART SPI masking confines this current-EL acceptance window to the
management handler; its original enable state and CPU IRQ mask are restored.
This does not enable general current-EL UART dispatch or change device ownership.

The management pilot also repeats masked EL2 delivery, active/WFI masked EL1
register-pattern checks, worker-to-Linux requests during CPU load and idle,
non-injection checks, responsive parking and fatal-stop with continued Linux
progress. This pilot passed on its first attempt: two stage-2 activation/deactivation
cycles produced the exact profile/VTTBR readbacks, and one worker SGI was handled
under the restored host profile. All 1,604 test requests were acknowledged, with
1,605 non-injection checks and zero stray management IRQs. CPU0 recorded three
current-EL and 1,602 lower-EL IRQs (1,514 masked, 88 unmasked). Maximum observed
acknowledgement latency was 50,001 us, including the deliberately masked 50 ms
interval. Three uniquely tagged guest commands passed; Linux progressed after
worker parking/fatal-stop. Cleanup and an independent subsequent query verified
power-off.

The separate normal-image full quick regression covers four boots, smoke/soak/
post-soak probes and UART/timer progress. The first run passed the three initial
boots, smoke test and 600.451-second soak (8,132 memory passes, 2,136 wakes,
maximum lateness 7.576 ms, zero wakeups over 500 ms). Its final boot stopped in
U-Boot before tvisor ran: the diagnostic Image transfer reported `ARP Retry
count exceeded; starting again`. The suite correctly remains FAIL, with verified
power-off; none of its results is relabelled as a complete pass. Network-interface
and TFTP-service checks were healthy. A new harness readiness guard requires a
successful fresh server ping before every transfer and fails the run on failure,
without automatic transfer retries. The guard was checked for success/failure
behavior without changing addresses or filenames. A complete fresh run in a new
directory subsequently passed, using unchanged frozen binaries. The failed first
run and its verified cleanup remain retained as a transport failure before the
post-soak tvisor launch.

## Provenance and evidence

Local evidence is under ignored `docs/evidence/phase11_4_hcr_policy/`; the normal
regression repeat is `guest/build/phase114-hcr-quick-20261101-r2/`; the failed
first run remains at `guest/build/phase114-hcr-quick-20261101/`. Exact tested source,
ELFs, diffs, local checks, UART and result files are retained. The source base is
`f42a7ea`, with the current working-tree changes. Implementation source is frozen
before hardware testing; subsequent report edits do not change the tested code.

Both runs reuse the already frozen baseline/diagnostic Linux fixtures from
`guest/build/phase114-quick-20261101/`. No Linux code/configuration was changed or
rebuilt. Their original manifest and its digest, configuration files and probe
provenance are copied into the new bundles. Only tvisor is rebuilt, with new
unique TFTP names for every run. A profile guard verifies fresh board/DTB/RAM/
relocation evidence before each RAM-only boot. No saved U-Boot environment or
SD-card write is part of verification. Final power-off was independently queried
and identity-verified after the passing repeat. No new overnight run is claimed.

## Passing full quick repeat

`guest/build/phase114-hcr-quick-20261101-r2/assessment.json` records all four
successful boots, 18 uniquely tagged guest commands, ten console/timer checks,
four fresh layout checks and eight successful pre-transfer network probes.
Each boot validated the existing CPU1 Parked checkpoint, bidirectional management
preflight and exact guest HCR profile. Other release cells remained zero, and the
normal image did not enable acceptance-only traffic.

The smoke probe passed at 35.986 seconds (280 memory passes, maximum lateness
7.620 ms). The soak passed at 600.497 seconds with 2,136 wakes, 8,126 memory
passes and maximum lateness 28.010 ms. No wakeup was over 500 ms. Timer interrupt
samples increased from 20,831 to 155,898. The final diagnostic cold boot and
5.628-second short probe passed. Total runner duration was 13 minutes 52 seconds,
including boot/transport checks and verified cleanup.

One RPC `COMMAND_EXIT_NONZERO` notice occurred on the final boot's shell check.
Its fresh UART contains the expected shell/uname output and both the runner's
unique completion marker and the corresponding rpictl marker with status zero;
the independent runner therefore accepted the actual command result. All 18
runner markers are zero. This contradictory RPC notice, ordinary prompt timeouts,
and raw UART are retained, rather than discarded or treated as guest failures.
Historical UART ring-buffer preambles are excluded from both assessments.

| Artifact | SHA-256 |
|---|---|
| Normal debug ELF used for both quick attempts | `ef7249bd8cefbd6e45fbd9e14ea51905a695117edc36ac3541790d83e8b532ce` |
| Management acceptance debug ELF | `2e507d7616698323a746a318ce27177fdb4d07da174a803649a2aafd0229c368` |
| Unchanged baseline Linux Image | `3a72c904e211f56e5f05d601b41495dfa4ec9029b3d7b1ee3556a72f9bb33b15` |
| Reused diagnostic Image | `a3772b566c8789bacdf4b1f47658045d42ccc1f573a10e0b33571f15d94f0cee` |

The exact frozen source-manifest SHA-256 is
`40be9799b1cbe82db9af57303e6370f8d18e466bb1b4723cde07965f6e0b397e`.
It matches the current implementation at base
`f42a7eaba80cecaa897e302750d390df022537cd`. Only documentation and the separately
recorded transport-readiness harness changed after the implementation freeze.

The local review archive is
`docs/evidence/phase11_4_hcr_policy/review-bundle.tar.gz`, with adjacent
`bundle-manifest.json` and `bundle-contents.json`. It includes the IRQ-policy
proposal and HCR implementation, exact source, four checked ELFs, local validation,
the management pilot, both quick attempts, transport guards/checks, raw UART,
fixture provenance, assessments and independent power-state evidence. These files
are ignored by Git. The unrelated earlier Phase 11.4 bundles remain untouched.
