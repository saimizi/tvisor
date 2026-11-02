# IRQ ownership and handling in tvisor

Design proposal, 2026-11-01, following
[PR #28's HCR policy review](https://github.com/saimizi/tvisor/pull/28#issuecomment-6556805974).
This defines the intended policy for the current non-VHE EL2/GICv2 platform.
The initial implementation baseline is `f42a7ea`; gaps are listed below. The
[HCR policy follow-up](phase11_4_hcr_policy.md) implements the explicit profiles
and full deactivation policy; its validation status is recorded separately.
The wider IRQ-dispatch and lifetime rules remain proposals.

## 1. Routing, ownership and virtual delivery

Enabling `HCR_EL2.IMO` makes tvisor responsible for dispatching physical IRQs
routed to EL2, including IRQs unrelated to management SGIs. Routing to EL2 does
not assign the source to a guest and does not authorize automatic forwarding.
A virtual interrupt requires an explicit virtual-device or assignment decision.

This responsibility covers the supported Non-secure IRQ path. Firmware retains
its EL3 routing and Secure-interrupt obligations. `IMO` neither overrides EL3
routing nor defines FIQ/SError policy; those remain separate. Establish usable
GIC group ownership, mappings, targets, enables and priorities before unmasking.

Distinguish three questions for every source: who owns the device/event, which
CPU handles its physical interrupt, and which VM/vCPU, if any, consumes a virtual
event. The physical INTID and virtual INTID need not match. A source's ownership
must not change just because a different guest happens to be running.

## 2. Explicit HCR profiles

Keep one authoritative shared host baseline and derive the running-guest profile
from it. Use explicit full-register writes followed by the required synchronization,
not read-modify-write inheritance from firmware or a previous execution phase.

```rust
pub const HOST_HCR: u64 = HCR_RW | HCR_IMO;
pub const GUEST_HCR: u64 = HOST_HCR | HCR_VM | HCR_SWIO | HCR_TSC;
// Existing public name HCR_EL2_STAGE2_VALUE should denote GUEST_HCR.
```

| Execution/transition | Intended HCR | Policy |
|---|---|---|
| Initial private EL2, including workers | `HOST_HCR` (`0x80000010`) | RW=1, IMO=1; VM=SWIO=TSC=0 |
| Guest stage-2 active, including temporary EL2 exit handling | `GUEST_HCR` (`0x80080013`) | Shared host controls plus VM, SWIO and TSC |
| Resume the same guest after an IRQ or emulated exit | `GUEST_HCR` | Resume its existing translation and interrupt context |
| Full stage-2 deactivation and return to host-only operation | `HOST_HCR` | Remove guest-only controls while retaining physical IRQ routing |

Both profiles require FMO=AMO=TGE=E2H=VI=VF=0 for this platform. Management IRQs
must not set VI/VF or create guest pending state. Adding a new shared HCR control
requires reviewing its meaning in both profiles. A future host-only control
needs an explicit profile difference and tests; it must not silently become a
guest control through an unchecked baseline change.

A normal vCPU exit is **not** full stage-2 deactivation: the CPU executes EL2
handling code while the guest configuration remains installed. Restore the host
profile when actually deactivating the guest translation context, not on every
IRQ. Preserve the existing VMID-aware TLBI/barrier ordering before disabling
stage-2 and clearing VTTBR. Changing HCR alone does not complete guest teardown.

## 3. IRQ ownership table

The following completion rules assume the configured GICv2 split EOI mode.
Priority drop and physical deactivation are separate operations.

| Physical source | Owner and permitted target | EL2 action | Virtual delivery | Physical completion owner |
|---|---|---|---|---|
| Management SGI 1 | Hypervisor; initialized management CPUs | Validate source, handle bounded management request | Never forward or mark a guest interrupt pending | EL2: EOIR then DIR with the full acknowledgement token |
| Virtual timer PPI 27 | Timer state belongs to the bound vCPU; physical handling on its owning CPU | Validate binding, preserve timer state, arrange guest notification | Hardware-backed timer LR, currently virtual INTID 27 | EL2 drops priority; guest virtual deactivation completes the linked physical interrupt |
| Mini UART SPI, discovered from DT | Currently a host-owned console routed only to CPU0 | Service RX and bridge input only to its configured virtual-console consumer | Software-originated PL011 interrupt, currently virtual INTID 33, if virtual device state calls for it | EL2 completes the physical UART interrupt independently of guest EOI |
| Other explicitly registered host source | Hypervisor/device driver; registered CPU target | Run its bounded host handler or defined deferred-work path | None unless separately specified by a virtual-device policy | Registered host handler's completion policy |
| Explicitly assigned guest device source (future) | Named VM/vCPU under an assignment contract | Enforce assignment, source servicing and completion contract | Only the configured virtual INTID for that VM/vCPU | Explicitly choose EL2 completion or a hardware-backed guest completion path |
| Unregistered or ownership-inconsistent source | No usable ownership policy | Diagnose, contain and fail closed; never guess a guest target | None | EL2 handles its valid physical token under the unexpected-IRQ policy |

PPI 27 is the architectural **virtual timer's** PPI delivered through the physical
GIC; it is not the architectural physical timer's PPI. A worker without a vCPU
keeps its timer controls and unused PPIs disabled. Receiving PPI 27 there is a
configuration fault, not permission to inject it into CPU0's guest.

The Mini UART is not currently passed through to Linux. Physical UART servicing
and virtual PL011 behavior are distinct: Linux's RX enable/mask and FIFO state
control virtual notification. Neither the physical UART INTID nor its raw GIC
acknowledgement token is forwarded to Linux. Direct assignment would require
separate device/MMIO ownership, routing and completion support.

## 4. Entry and dispatch contract

Use one logical classification policy for both current-EL and lower-EL IRQs.
Entry mechanics differ; interrupt ownership does not.

1. Preserve interrupted state before calling Rust. Current-EL handling preserves
   GPRs, SIMD/FP state and exception-return state on the private EL2 stack.
   Lower-EL handling saves the vCPU before inspecting or changing its virtual state.
2. Read IAR once and retain the entire acknowledgement token. Dispatch by INTID;
   for SGIs, map its source interface through discovered per-CPU GIC targets.
   Special/reserved IDs 1020–1023 are never completed as ordinary interrupts;
   spurious/no-work observations require no device handler or guest injection.
3. Classify using registered ownership, CPU locality and readiness, including a
   stable VM/vCPU binding where applicable. Do not select an owner from INTID
   alone or assume the currently running vCPU owns an arbitrary physical IRQ.
4. Service the source using bounded work. Apply device-level acknowledge/clear
   rules as well as GIC rules, then complete or explicitly transfer responsibility
   for the physical token. Do not allocate, take allocator/table locks, format
   ordinary logs, or synchronously wait for another CPU inside the IRQ handler.
5. Record diagnostics/counters and any virtual pending event. Resume the host or
   guest only when the handler's policy permits, with its state preserved.

An EL2 execution interval without an active guest does not erase device ownership.
Known host devices may still require servicing. A bound guest source needs retained
pending state or containment until its owner can receive it; it must never be
redirected to another guest. In particular, the assembly `active_vcpu` pointer is
cleared on guest exit, so it is not by itself a persistent device-assignment map.
IRQ unmasking is allowed only where the handlers needed for that CPU and execution
phase are ready. Pending work in a bounded masked region must be serviced when
that policy next permits, including on guest resumption.

## 5. Completion and pending-event lifetime

Each acknowledged physical interrupt has one completion owner. Preserve the full
raw token for matching completion, particularly the source bits for SGIs. Do not
complete a token twice or send it through both host and guest completion paths.

For host SGIs and the current UART bridge, EL2 handles physical completion. A UART
handler must drain/clear or otherwise contain its level source before allowing
re-delivery. The virtual UART notification is independent of the already completed
physical interrupt; guest EOI cannot complete that physical token again.

For the timer's hardware-backed LR, EL2 drops physical priority and leaves physical
deactivation to the corresponding virtual completion. Guest EOI/deactivation
behavior depends on its virtual CPU-interface mode. Do not apply the generic host
EOIR+DIR helper after successfully handing physical completion to that LR.

If an LR or software event queue is busy/full, retain or explicitly account for
the pending event under that source's policy. Do not overwrite an in-flight LR,
lose the event silently, or invent a second physical-completion owner. Timer LR
backpressure and teardown must account for any still-active physical PPI.

Before freeing a vCPU or changing device ownership, stop new events and resolve
pending/active interrupts, timer state and virtual GIC references. A source must
not retain a reference to freed guest state. Reassignment and migration require
an explicit quiescence protocol; they are outside the current one-vCPU milestone.

## 6. Unexpected and unsupported IRQs

The default is no guest forwarding. Record the INTID, raw token, CPU, entry kind
and applicable ownership/readiness state using bounded diagnostics. For a source
whose Non-secure controller access permits it, contain re-delivery by disabling
or masking that source as appropriate. Do not guess how to acknowledge an unknown
device or attempt to reclassify firmware-owned Secure interrupts.

Complete a valid, EL2-owned physical token using the established GIC mode, then
enter the defined error/fail-stop path. Merely writing EOI/DIR does not clear an
unknown level-sensitive device. Do not resume normal execution into an interrupt
storm or silently treat the source as successfully handled. A claimed known source
on the wrong CPU or without a valid owner is also a policy fault.

## 7. Implementation baseline and required follow-up

At `f42a7ea`, physical SGIs already use full-token EOIR/DIR completion, timer delivery
uses a hardware-backed LR, and UART input uses an independent software LR. Management
handling checks non-injection, and the linked entry paths preserve interrupted state.
The Phase 11.4 evidence remains evidence for that implementation, not for this proposal.

The following table compares the initial baseline with the follow-up status;
remaining rows must be addressed before claiming full conformance:

| Area | Current implementation | Required follow-up |
|---|---|---|
| HCR composition | Guest HCR independently repeats shared bits | Implemented in the HCR follow-up: derive from `HOST_HCR`, validate exact profiles and test shared controls |
| Full stage-2 deactivation | Clears only VM, leaving `0x80080012` with SWIO/TSC set | Implemented in the HCR follow-up: explicitly restore `HOST_HCR` after guest-VMID invalidation; test readback and post-deactivation SGI handling |
| Dispatch coverage | Lower-EL path recognizes SGI, timer and UART; current-EL path recognizes only management SGI | Apply the same ownership policy in both paths, with explicit CPU/phase readiness restrictions |
| Unexpected physical IRQ | Completes the token; lower-EL path terminates the guest run, current-EL path becomes fatal | Define common diagnostics, source containment and fail-stop behavior; never auto-forward |
| Pending/teardown cases | Timer duplicate counter and bounded virtual queues exist | Review LR-busy, queue-full, no-running-vCPU and teardown cases against the lifetime rules above |

A small explicit classifier for the supported sources is sufficient initially;
a general device-assignment framework is not required by this document. Future
sources must be added with ownership and completion policy rather than a generic
forward-everything fallback.

## 8. Verification before policy acceptance

- Test the exact host and guest HCR profiles, inclusion of all required shared
  bits, prohibited routing/injection bits, and full deactivation back to host.
- Verify profile readback and IRQ delivery across activation, temporary exits,
  resumption and full deactivation. Exercise IRQs pending in masked EL2 regions,
  guest IRQ-masked execution and WFI, subject to the handler-readiness policy.
- Check SGI source-bearing tokens, exactly one completion owner, no management
  LR/pending-state changes, and unchanged guest architectural/virtual GIC state.
- Verify timer progress and the hardware-backed completion path; test LR-busy
  handling without lost events or duplicate physical deactivation.
- Exercise UART RX, virtual-device masks, FIFO pressure and Linux timer traffic
  concurrently with management requests. Check physical and virtual IRQs separately.
- Test unregistered IRQ classification, invalid ownership/locality, reserved IDs,
  and guest-source handling without a running vCPU. Validate containment without
  forwarding or unbounded IRQ-side work.
- Repeat the relevant hardware management tests and normal guest regression after
  implementation. Preserve exact source/artifact provenance and failed attempts;
  this documentation-only change does not itself require a new hardware run.

## References

The profile choices and source table above are tvisor policy. Architectural
routing is specified by [Arm's HCR_EL2 description](https://support.arm.com/documentation/ddi0595/2021-03/AArch64-Registers/HCR-EL2--Hypervisor-Configuration-Register).
GIC acknowledgement/completion and hardware-backed LRs are described by
[Arm IHI 0048B](https://documentation-service.arm.com/static/5f8ff21df86e16515cdbfafe),
especially sections 4.4.4–4.4.5, 4.4.14 and 5.2.1. This proposal complements the
[physical SMP design](tvisor_smp.md#114-gicv2-and-management-interrupts) and the
[Phase 11.4 implementation report](phase11_4_status.md).
