# Phase 11.4: GICv2 and host management interrupts

Implementation and scoped acceptance complete, 2026-11-01; ready for user review.
Phase 11.3 is accepted. The contract is
[the Phase 11.4 design](tvisor_smp.md#114-gicv2-and-management-interrupts).
Phase 11.4 acceptance by the user and merge remain separate from this implementation
and validation result. Validation and the local review bundle captured the working
tree before the review commit.

## Work breakdown

1. Audit physical IRQ routing, acknowledgement and guest world switches.
2. Separate boot-only distributor setup from banked per-CPU setup; discover
   actual target masks and validate Device mappings and Non-secure ownership.
3. Add sequence-numbered request/ack channels with bounded waiting and retained timeout state.
4. Preserve raw acknowledgement tokens and dispatch host SGIs through current-
   and lower-EL vectors with correct physical completion.
5. Activate CPU1 management only after the accepted Parked checkpoint and boot
   permit; keep CPU2/CPU3 in firmware and worker timers disabled.
6. Preserve physical IRQ routing and guest architectural/virtual GIC state at
   every guest entry and management exit.
7. Exercise masked EL2, masked EL1 active/WFI, worker-to-Linux delivery,
   responsive parking and fatal-stop handling.
8. Run linked checks, host tests, physical acceptance and full quick regression.
9. Record exact source/artifact provenance, outcomes and verified power-off.

All nine steps are complete. The final physical management pilot and the
normal-image full quick regression passed. Final power-off was independently
verified.

## Implemented protocol

SGI 1 is reserved for host management. Every CPU reads its banked target fields;
logical IDs are never shifted into a target mask. Nonzero target fields must
identify one interface, distinct from every initialized CPU. Banked words are
read with 32-bit accesses and their byte fields folded together; fields reading
zero do not establish a target. An empty or multi-interface mask is rejected.

CPU0 configures the host UART SPI once and routes it only to its discovered
interface. Each CPU checks the retained immutable Device mappings and active
write translation before accessing its GIC. Local setup establishes the management
priority, PMR, binary point and split EOI mode and verifies readback. With Security
Extensions, IGROUPR is Secure-only and its Non-secure RAZ view cannot prove
classification. The firmware must leave SGI 1 enabled in the Non-secure view;
Group-0 enable bits are RAZ/WI. Nonzero priority writes must read back through
the Non-secure view before use (Group-0 priority fields are RAZ/WI too). No
Non-secure write is treated as permission to reclassify a Secure interrupt.
Firmware remains responsible for ownership and EL3 routing. Unsupported contracts
fail before IRQ unmask. On workers, PPIs and GICH are disabled; all three physical/
virtual timer controls remain disabled from startup.

Each ordered CPU pair has one retained allocation-free channel. A sender
publishes operation and argument with a sequence, then orders that publication
before GICD_SGIR. The per-CPU `ready` publication protects immutable GIC metadata; it is not a
second CPU lifecycle or a waiting result. Online/Parked/Failed remain in the
existing mailbox. A receiver claims each sequence once, performs bounded work,
completes the full raw physical IAR token through EOIR and DIR, then acknowledges
its reply. Only the sender consuming that reply retires storage for reuse.
Timeout or a dropped receipt never cancels a request or reuses its storage.
SGI coalescing and duplicate doorbells cannot duplicate execution. Ping, bounded
worker computation, responsive park and fatal-stop are host operations. The
owner publishes Parked before acknowledging a park request and remains IRQ-
responsive. Fatal-stop completes the physical token and acknowledges before
publishing its IRQ-masked holding checkpoint. Actual local management setup
failures publish Failed and a fatal record; waiting errors never do.

CPU1 initially reaches the unchanged Phase 11.3 Parked checkpoint with IRQs
masked. CPU0 obtains its boot permit, then authorizes management activation.
CPU1 establishes its local handler state, publishes Online and unmasks IRQs.
CPU0's boot preflight checks both request directions and bounded worker work.
The worker's WFI loop masks IRQ while checking work and entering sleep, then
unmasks after wake, preventing a request arriving between the check and WFI
from being stranded. Parking retains IRQ responsiveness and all resources;
this is not CPU_OFF and permits no firmware restart or reclamation.

Current-EL vectors preserve all GPRs, Q registers, FPCR/FPSR and exception-return
state before calling Rust. Lower-EL management IRQs use the existing full vCPU
save/restore path. Both dispatch management before any guest-injection path.
Special acknowledgement IDs 1020–1023 are never completed. The physical SGI
source bits survive completion. The binary point is requested at zero and its
implementation-defined minimum (up to four in the Non-secure view) is accepted. Hardware timer LRs retain their separate guest-
owned deactivation path; host UART and SGIs complete physically at EL2.

The host baseline now sets HCR_EL2.IMO as well as RW. Guest stage-2 activation
already sets IMO, and deactivation preserves every control except VM. Guest
entries verify IMO=1, FMO=AMO=TGE=E2H=VI=VF=0. Management dispatch compares
GICH VMCR/LR state and routing controls before/after handling, and the lower-EL
caller additionally verifies its saved virtual GIC state is unchanged. Guest
queue helpers reject SGI IDs.

## Acceptance instrumentation

`management-irq-test` is an explicit acceptance feature. It enables bounded
masked-EL1 register-pattern probes before Linux and worker-originated requests
while Linux runs for 30 seconds, followed by responsive-park/ping and fatal-stop
checks with continued guest progress. Normal workers sleep without periodic management ticks or a
worker timer. Test-only output and its source provenance must be distinguished
from the normal image's regression results.

Current local checks: 179 tests pass in debug and release; normal and acceptance
images build warning-free in both profiles. Linked checks pass on all four
images, and all eleven deliberately corrupted variants per image are rejected. The host is AArch64; use
`cargo test --lib --target aarch64-unknown-linux-gnu`, since the old `test-host`
alias names an unavailable x86 target.

Hardware evidence is retained locally under ignored `docs/evidence/phase11_4/`.
Pilot 01 failed manifest verification before boot (missing runner hash) and
verified power-off. Pilot 02 failed closed at boot GIC target discovery before
secondary release; its UART and power-off evidence are retained. The discovery
implementation was corrected to fold banked register words, with a sparse-field
host test, before a fresh pilot. Pilot 03 then stopped at the incorrect IGROUPR
ownership test; it was corrected using the specified Non-secure access semantics.
Pilot 04 reached CPU-interface validation; review also corrected an overly strict
binary-point check to allow the architectural Non-secure minimum of up to four.
Every attempt uses new artifacts and retains its own logs and cleanup evidence.
Pilot 05 passed GIC initialization, bidirectional requests and both masked EL1
probes, then exposed test traffic started too early during masked guest resource
preparation. Arming now occurs at first actual guest entry, after draining the
previous test session. Pilot 06 passed the corrected sequence. Pilot 07 stopped
because its shell load command depended on unavailable baseline BusyBox sleep/
kill applets; its unfinished background job was cleaned up by the verified RAM-
only power-off. Pilot 08 used the same final hypervisor binary with a finite,
self-contained shell loop and passed all checks. No failed run is counted as
acceptance.

Architecture references used during implementation:

- [Arm GICv2 architecture specification](https://developer.arm.com/documentation/ihi0048/latest/).
- [Arm HCR_EL2 register description](https://support.arm.com/documentation/ddi0595/2021-03/AArch64-Registers/HCR-EL2--Hypervisor-Configuration-Register).
- [Linux GICv2 driver target discovery](https://github.com/torvalds/linux/blob/master/drivers/irqchip/irq-gic.c).
- [Arm Trusted Firmware GICv2 definitions](https://github.com/ARM-software/arm-trusted-firmware/blob/master/include/drivers/arm/gicv2.h).

All-core bring-up, allocator contention and TLB shootdowns retain their later
phase boundaries. This phase does not add guest SMP, vCPU migration or scheduling.

## Final management pilot

`docs/evidence/phase11_4/pilot-08/` records the passing final pilot. Its source and
artifact hashes match the frozen implementation. Fresh UART after the final
application-start marker shows:

- CPU0/CPU1 target masks `0x1`/`0x2`, CTLR `0x261`, PMR `0xf0`, BPR 3.
  `security/group=1` is a packed raw readback: Security Extensions present,
  IGROUPR reads zero through its Non-secure view; it is not an ownership assertion.
- Host HCR `0x80000010`; guest HCR `0x80080013`, with IMO set and FMO/AMO/TGE/
  E2H/VI/VF clear.
- Pending SGI service after a deliberate 50 ms masked EL2 interval, 50 masked
  active-EL1 IRQs and 51 masked EL1/WFI IRQs, with GPR/SIMD/PSTATE preserved.
- 1,603 test requests acknowledged; CPU0 recorded two current-EL and 1,602
  lower-EL dispatches, including 1,513 masked and 89 unmasked lower-EL IRQs.
  Host counters progressed during the finite Linux CPU load and subsequent idle.
- 1,604 checked non-injection observations, zero stray management IRQs, and raw
  source-bearing token `0x401`. Maximum acknowledgement latency was 50,001 us,
  including the deliberate masked interval, below the two-second bound.
- Responsive park, ping while Parked, then fatal-stop to an IRQ-masked retained
  holding state; a subsequent unique Linux command still completed.
- Three unique guest commands returned zero. Other firmware release cells stayed
  zero. Runner cleanup verified relay-off; a separate power-state query after the final
  regression independently verified off and the configured relay identity.

## Full quick regression and handoff

`guest/build/phase114-quick-20261101/` contains the separate normal-image full
quick regression. All four boots, 18 unique guest commands and ten independent
console/timer checks passed. Each fresh boot validated CPU1's original Parked
checkpoint and the new bidirectional management preflight, then entered Linux
with the required IRQ routing. CPU2/CPU3 stayed in firmware; their cells and the
boot cell remained zero. The normal image contains no acceptance-test traffic.

The smoke probe passed at 35.986 seconds with 280 memory passes and maximum
lateness 7.622 ms. The full soak passed at 600.469 seconds with 2,136 wakes,
8,126 memory passes and maximum lateness 11.581 ms. No wakeups exceeded 500 ms.
Timer interrupt samples increased from 20,788 to 155,817. The post-soak cold
boot and 5.628-second short probe passed. Total runner time, including boots and
power cleanup, was 13 minutes 36 seconds. Final relay-off was independently
queried and identity-verified. No new overnight run was performed.

The baseline Linux Image is unchanged; only the isolated diagnostic fixture
adds proc/sysfs and the existing stress probe. Reviewed Kconfig differences are
retained with the run. ELF/Image SHA-256 values are:

| Artifact | SHA-256 |
|---|---|
| Normal debug ELF used for full quick | `7ec556e03d4130c025248d78e2f75417e10deee465bc5f333806f69604f9764b` |
| Management acceptance debug ELF used for pilot 08 | `d295ef7e992826089cc11bf774b90d7b9d5c0e70a391ffb21132c08131223b9f` |
| Unchanged baseline Linux Image | `3a72c904e211f56e5f05d601b41495dfa4ec9029b3d7b1ee3556a72f9bb33b15` |
| Isolated diagnostic Image | `a3772b566c8789bacdf4b1f47658045d42ccc1f573a10e0b33571f15d94f0cee` |

Validation used the dirty working tree at base
`698bdefab1fc80719c2276ef806ce805f961cf13`. Exact frozen source hashes match the
current implementation; meaningful untracked modules are included explicitly.
The source-manifest SHA-256 is
`b180ed124df9f774bbe7abbcd83d7dd8aec39d52c528e48f30cc6ca5e8b12cfb`.
Only documentation changed after the tested source freeze.

All four final builds are warning-free. All 179 host tests pass per profile,
and all four images pass the linked trampoline check and eleven negative
mutations each. Additional linked-frame checks verify both current-EL IRQ vectors,
the 816-byte frame, all Q-register saves/restores, FPCR/FPSR and return sequence.
Formatting, whitespace, changed-document links and artifact hashes were checked.
Initial local compile/tool-target errors are preserved separately from final
passing logs in `local-build-history/`.

Raw UART logs can contain historical ring-buffer preambles. Both assessment
files use the suffix after the final application-start marker; stale success
lines are excluded. Expected rpictl prompt-observation timeouts are retained,
with fresh UART and unique command-result markers establishing completion.
No SD card writes or saved U-Boot environment changes were made.

The local, Git-ignored review bundle is
`docs/evidence/phase11_4/review-bundle.tar.gz`, with adjacent
`bundle-manifest.json` and `bundle-contents.json`. It contains exact source and
four checked ELFs, build/host/binary logs, every retained pilot, passing management
and quick assessments, raw UART, image/configuration hashes and review documents.
Kernel build intermediates are excluded. Failed attempts remain failed; only
pilot 08 and the separate full quick run certify the final implementation.
