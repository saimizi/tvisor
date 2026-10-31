# Phase 11.3 shared host EL2 policy

Task 5, 2026-10-31. The policy is defined in
[`tvisor_util/el2_policy.rs`](../tvisor_util/el2_policy.rs). Both primary and
secondary startup inline the same
[`host_el2_controls.S`](../src/host_el2_controls.S) sequence with constants from
that module. No Rust helper is called on the secondary before its acquire
boundary. The sequence uses x9 only and preserves the primary's translation
arguments and the secondary's record/generation registers.

## Supported register values

This is the non-VHE Armv8-A/A72 host baseline. References are Arm's
[Cortex-A72 TRM, ARM 100095_0003_06_en](https://documentation-service.arm.com/static/60368ce38f952d2e4134dc2e),
sections 4.3.34–4.3.36 (HCR/CPTR/HSTR), 4.3.42 (TCR), the architectural register
summary, and the [boot contract](phase11_3_backend_contract.md).

| Register | Policy | Purpose |
|---|---|---|
| HCR_EL2 | `HOST_HCR = 0x80000000` | AArch64 lower EL (RW); stage 2 off (VM=0), non-VHE (E2H=0), routing and optional traps clear |
| CPTR_EL2 | `HOST_CPTR = 0x33ff` | Preserve required-one fields; FP/SIMD trapping disabled; TTA/TCPAC clear |
| HSTR_EL2 | `HOST_HSTR = 0` | No additional AArch32 system-register traps |
| VTTBR_EL2 | `HOST_VTTBR = 0` | No active guest translation context |
| CNTVOFF_EL2 | `HOST_CNTVOFF = 0` | Explicit baseline virtual counter offset |
| CNTHCTL_EL2 | `HOST_CNTHCTL = 3` | Permit EL1 physical counter/timer access; other controls clear |
| CNTHP_CTL_EL2, CNTP_CTL_EL0, CNTV_CTL_EL0 | `DISABLED_TIMER_CTL = 0` | Disable all three unused local timer outputs |
| MAIR_EL2 | `HOST_MAIR = 0x4ff` | Existing Normal WB/WA and Device nGnRE attribute slots |
| TCR_EL2 | `HostTranslation::new` | Existing 39-bit VA, 4 KiB granule, WB/WA table walks, Inner Shareable, required-one bits and validated physical size |
| TTBR0_EL2 | `HostTranslation::root()` | Validated aligned root from owned table metadata |
| SCTLR_EL2 | `HOST_SCTLR = 0x30cd183d` | Required-one bits, MMU/data/I-cache enabled, stack alignment checking, WXN; LE |
| VBAR_EL2 | `__el2_vectors` | Same linker-owned vector symbol on both CPUs |
| TPIDR_EL2, SP_EL2 | Selected private context/stack | Per-CPU resources, not shared register constants |
| DAIF | Mask all D/A/I/F during startup | CPU 0 may later enable its supported interrupt path; parked secondary stays masked |

The policy does not change CNTFRQ, invent an inherited cache state or access EL3
or implementation-defined controls. Entry validation remains a separate step.
The reference applies to the supported CPU profile; extension-specific controls
on other architectures require their own policy review.

## Translation policy construction

`HostTranslation` has private fields. Its constructor accepts a root and PA-range
encoding from the owned `El2Stage1PageTable` metadata. It rejects a zero or
unaligned root, an address outside the selected physical range and PA encodings
above 48 bits, outside this profile. MAIR and SCTLR are fixed; TCR is built from
named fields. The primary uses this constructor before takeover.

`secondary::start_one` now accepts that same policy type instead of four
unconstrained raw register arguments. It populates the immutable startup record
using the policy accessors. Secondary assembly installs those published values;
it does not copy live registers from CPU 0. The remaining call-site integration
is Task 10.

## Initialization ordering and guest policy

Both paths establish masked DAIF, private SP/TPIDR and VBAR before executing the
common control sequence. That sequence sets HCR, CPTR, HSTR, VTTBR, counter offset,
counter access and disabled timers, then executes ISB before continuing to Rust
or secondary translation setup. Every MOV value fits its linked immediate
encoding; the shared sequence uses no stack, memory load, literal pool or call.

The primary now explicitly initializes this baseline before `private_el2_main`;
it previously inherited several of these controls from U-Boot. This is a real
primary startup behavior change and requires the later hardware regression gate.
The translation values for the previously observed board configuration remain
MAIR `0x4ff`, TCR `0x80843519` and SCTLR `0x30cd183d`.

Guest stage-2 activation retains its existing additional VM, interrupt-routing
and trap controls. Its RW and CPTR constants now refer to the shared definitions,
so FP/SIMD and required-one policy cannot drift. Guest world-switch timer state
and guest stage-2 teardown behavior remain separately owned by the guest path.
This host baseline is installed at startup, not blindly reapplied on every exit.

## Validation and remaining gates

- 144 hosted library tests pass, including two new policy tests covering the
  existing board translation values, key control invariants, every supported
  physical size, invalid roots and unsupported PA encodings.
- Existing guest stage-2 policy tests continue to pass with the shared constants.
- Bare-metal build, formatting and whitespace checks pass. Inactive startup
  functions still generate warnings pending boot integration.
- Linked disassembly of `__enter_private_el2` and `__secondary_entry` shows the
  same 17-instruction common control sequence, including immediate values and
  final ISB. This checks the shared sequence, not the entire trampoline.
- Full entry/MMU/cache-maintenance ordering and architecture/errata review
  remain Tasks 6/9/11. Full control-state checkpoint validation remains Task 7.
- No hardware was operated for Task 5; no secondary release gate was enabled.
