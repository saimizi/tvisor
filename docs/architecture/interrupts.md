# Interrupt routing and management

Tvisor owns the supported Non-secure physical IRQ path at EL2. Routing an IRQ
to EL2 does not assign its source to the currently running guest. Source
ownership, physical CPU targeting and virtual delivery are separate decisions.
Firmware retains EL3/Secure-interrupt obligations; FIQ and SError need their
own policy and are not enabled by the IRQ routing configuration.

## Explicit EL2 control profiles

[`el2_policy.rs`](../../tvisor_util/el2_policy.rs) defines the host baseline
and derives the guest profile from it. Primary/secondary startup use the shared
[assembly controls](../../src/host_el2_controls.S). Values are explicit full
register writes with synchronization, not inherited firmware state.

| Execution state | HCR_EL2 | Meaning |
|---|---|---|
| Host-only, including workers | `HOST_HCR = 0x80000010` | RW and IMO set |
| Guest stage-2 installed, including temporary exits | `GUEST_HCR = 0x80080013` | Host baseline plus VM, SWIO and TSC |
| Full guest deactivation | Restore `HOST_HCR` | Remove all guest-only controls; retain physical IRQ routing |

FMO, AMO, TGE, E2H, VI and VF remain clear in both profiles. Management SGIs do
not set virtual pending bits. Normal IRQ/emulation exits retain the guest
profile; changing HCR on every exit would not describe the installed context.
Full deactivation invalidates the guest's translations while its VMID remains
installed, synchronizes, writes the host profile and clears VTTBR. See
[guest translation activation](virtualization.md#stage-2-activation-and-deactivation).

Other shared startup controls are CPTR_EL2=`0x33ff` (required-one fields and no
FP/SIMD trap), HSTR_EL2=0, VTTBR_EL2=0, CNTVOFF_EL2=0, CNTHCTL_EL2=3 and disabled
host/physical/virtual local timer outputs. Workers have no guest timer binding.
Host translation controls are described in [memory](memory.md#host-el2-mappings).

## Implemented IRQ paths and ownership

[GIC discovery](platform.md) establishes usable mappings and Non-secure CPU
interface/group/priority configuration before IRQ unmasking. The current GICv2
profile uses split EOI mode: physical priority drop (EOIR) and deactivation
(DIR) are different operations.

| Source | Current dispatch | Virtual delivery | Completion |
|---|---|---|---|
| Management SGI 1 | Current-EL and lower-EL handlers, initialized host CPUs | None | EL2 EOIR then DIR with full raw token |
| Architectural virtual timer PPI 27 | Lower-EL path on the guest's CPU | Hardware-backed virtual PPI 27 in LR0 | EL2 drops physical priority; guest virtual completion deactivates linked physical PPI |
| DT-selected mini-UART SPI | Lower-EL path on CPU0 | RX bytes feed virtual PL011; software-originated virtual SPI 33 in LR1 | EL2 drains RX and completes physical IRQ independently of guest EOI |
| Other ordinary INTIDs | No general handler/assignment registry | No automatic forwarding | Current code completes token and takes an error/fatal path |
| Special/reserved IDs 1020–1023 | Treated as no ordinary acknowledgement | None | No ordinary EOIR/DIR completion |

PPI 27 belongs to the architectural virtual timer, even though its expiry
arrives through the physical GIC. It is not the physical timer PPI. An idle
worker receiving it has no authority to inject into CPU0's guest.

Read IAR once and preserve the complete acknowledgement token, including SGI
source-interface bits. INTID alone is insufficient for matching completion.
Do not apply the host EOIR+DIR helper to a successfully hardware-backed timer
LR: that would add a second physical deactivation owner.

Mini-UART input is a host console bridge, not device passthrough. Its physical
INTID/token are never forwarded. Virtual FIFO/mask state determines Linux's
PL011 notification; guest EOI cannot complete an already completed physical
UART IRQ. Timer LR duplication increments a counter and is reported after
handling; it does not overwrite the in-flight LR.

## Exception entry

The [EL2 vector table](../../src/exception.rs) has sixteen 128-byte slots,
aligned to at least 2048 bytes. Current-EL entry preserves x0–x30, vector,
ELR/SPSR/ESR/FAR, Q0–Q31 and FPCR/FPSR in an 816-byte, 16-byte-aligned frame.
SIMD begins at offset 288, FPCR at 800 and FPSR at 808. Rust/assembly assertions
and linked disassembly checks enforce this ABI.

Lower-EL AArch64 synchronous/IRQ entries save the vCPU through the
[world-switch path](virtualization.md#world-switch-state). Ownership must not
be inferred from the temporary `active_vcpu` pointer alone: it is cleared on
exit and is not a persistent device-assignment map.

## Management requests

[`Channel`](../../tvisor_util/management.rs) provides one bounded request slot
per sender/receiver pair. SGIs are doorbells and may coalesce. A monotonically
increasing sequence publishes operation/argument; receiver acquisition claims
each request once. Discovered GIC target masks route requests and decode source
interfaces; CPU logical IDs are not assumed to be GIC target bits.

The [dispatcher](../../src/management.rs) validates source, readiness, HCR and
request ownership. It performs bounded work, completes the physical token,
checks virtual-GIC/routing preservation, and only then release-publishes the
reply. Sender acquisition of the matching acknowledgement permits retirement
and storage reuse. Timeout or dropping a pending request does not cancel the
request or authorize overwriting its storage. Sequence exhaustion does not wrap.

Operations include ping, bounded worker-test admission, Park, FatalStop and
TLB quiescence admission. IRQ handlers must not allocate, format ordinary output,
edit translation tables or synchronously wait for another CPU. Deferred workers
do allocator/logging workloads and acknowledge their later protocol stages.
Park/FatalStop coordinate with the [mapping membership gate](memory.md#synchronized-el2-mapping-updates).
Counter deadlines and poll limits bound management waits outside handlers.

## Remaining work

The following are requirements for extending IRQ support, not claims that the
current dispatcher already implements them:

- Share one explicit ownership/locality classification policy between current-
  and lower-EL paths. Current-EL dispatch recognizes only management SGIs;
  lower-EL dispatch also recognizes timer and UART. Unmask IRQ only when the
  required handlers for that CPU/execution interval are ready.
- Define consistent unexpected-source diagnostics, source containment and
  fail-stop behavior. Completing an unknown level-sensitive token does not clear
  its device source. Never guess a guest target or Secure-interrupt policy.
- Account for LR-busy, FIFO-full and pending events with no running vCPU. The
  current UART drain ignores a failed FIFO enqueue; comprehensive backpressure
  and event retention have not been established. Timer duplication counters
  alone do not prove all pending/teardown cases safe.
- Before freeing/reassigning a vCPU or device, stop new events and resolve
  active physical tokens, timers, virtual GIC references and queued notifications.
  General teardown, reassignment and migration require explicit quiescence.

For future sources, retain exactly one physical completion owner and explicitly
define virtual INTID, VM/vCPU binding, device servicing, targeting and lifetime.
The present supported sources do not require a speculative assignment framework.

Validation must cover profile transitions, masked IRQ/WFI delivery, raw SGI
tokens, non-injection, complete interrupted-state preservation, timer progress,
UART pressure, unexpected ownership and pending teardown cases. Existing
[hardware tests](../testing/hardware-tests.md) cover current management/guest
behavior; they do not certify the remaining requirements above.

## References

- [Arm HCR_EL2 description](https://support.arm.com/documentation/ddi0595/2021-03/AArch64-Registers/HCR-EL2--Hypervisor-Configuration-Register)
- [GICv2 architecture, IHI 0048B](https://documentation-service.arm.com/static/5f8ff21df86e16515cdbfafe), especially sections 4.4.4–4.4.5, 4.4.14 and 5.2.1
