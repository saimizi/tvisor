# Phase 11.1: per-CPU storage and guarded EL2 stacks

Status: **dynamic secondary-stack revision implemented and full quick regression
passed**, 2026-10-25. 110 host tests and a RAM-only disabled-CPU guest boot also
passed.
Revision base: `e4fd64d`. No secondary CPU is released in this milestone.

## CPU contexts and preparation stages

`src/pcpu.rs` keeps lifetime-stable, cache-line-aligned per-CPU contexts accessed
through `TPIDR_EL2`. The 64-byte context still has `host_sp` at offset 0 and
`active_vcpu` at offset 8, checked at compile time. All four world-switch assembly
lookups and the existing guest-entry/return invariants remain unchanged.
Owner-only mutable context is separate from atomic lifecycle/mailbox metadata.
Only `Discovered -> Prepared` is implemented; Prepared never means CPU online.

Preparation now has two stages:

1. `prepare_boot_cpu()` runs during early boot without allocation. It initializes
   only logical CPU0, using the single statically reserved guarded boot stack.
   The discovered topology is retained independently for later preparation.
2. `prepare_secondary_cpus()` runs on CPU0 after the physical-page allocator is
   initialized. It allocates one contiguous 17-page range (4 KiB guard plus
   64 KiB usable stack) per enabled secondary. Disabled CPUs receive no range.
   All mappings and guards are validated, stack bytes are zeroed, and only then
   are secondary bounds published as Prepared. The CPUs remain parked in firmware.

The linker reserves only one 68 KiB guarded boot stack. Its layout has no
secondary CPU count. Per-CPU metadata capacity remains four for the supported
board; secondary stack allocations follow the actual enabled topology.

## Mapping order and ownership

Physical allocator initialization touches only bitmap metadata already mapped
inside the tvisor image. Stack allocation also touches only bitmap metadata,
so it can safely precede broad usable-RAM mapping. No newly allocated stack
address is dereferenced during that allocation stage.

The first usable-RAM mapping excludes the dynamic guard pages as well as the
live DTB. This avoids creating a guard identity mapping at all; no live block
mapping is split or replaced. Existing descriptor-publication, local TLB
invalidation, completion barriers and ISB finish before the stack memory is
cleared or Prepared is published. Every stack page is RW/NX Normal and every
guard remains unmapped, verified by software table walks. Stack zeroing covers
only the usable 64 KiB, never its guard.

Guard pages remain allocator-owned InUse pages along with their stacks for the
run lifetime. The CPU context retains stack bounds, from which the guard and
full allocation extent are recoverable. These pages are not guest backing or
heap pages. Physical ownership and virtual guard permissions are separate.

A partial allocation failure rolls back prior ranges before mapping/publication.
A mapping failure stops boot and retains allocated ranges: partially installed
mappings cannot outlive recycled stack memory. No secondary is published or
released on that failure path. There is no hotplug/retry/free protocol yet.

## Validation

- **110 native ARM64 library tests passed**, including enabled-only allocation,
  boot-only topology with zero allocations, partial exhaustion rollback,
  disjoint stack ownership including guards, layout alignment/overflow,
  guard permissions and mapping-capacity failure.
- Bare-metal build, formatting and whitespace checks passed.
- Disassembly preserves TPIDR installation and the four context lookup sites.
  Linker symbols contain one boot stack and no `.pcpu_stacks` arena.
- Targeted hardware test: physical affinity 2 disabled in a RAM-only DTB copy;
  exactly two secondary ranges allocated for affinities 1 and 3, three guards
  absent, Linux guest boot and UART command completed with status zero.
- Full normal-topology quick regression **PASS**: four boots, 18 guest commands,
  ten independent UART/IRQ checks, and a 600.459-second soak with 2,136 wakeups,
  8,137 complete 32 MiB memory passes, maximum lateness 7.580 ms and zero wakeups
  over 500 ms late. The smoke and final reboot probes passed too.
- Every fresh normal boot shows three dynamic secondary allocations, four guards
  unmapped, 9/16 EL2 table pages used, and publication after allocator/mapping
  setup. Final runner cleanup and independent relay readback confirmed power off.
- The runtime image ends at `0x04110000`; the linker reserves one 68 KiB boot
  stack range, not four stack ranges. The secondary ranges are separate physical
  allocations, retained with their guard pages for the run lifetime.
- Tested tvisor ELF SHA-256:
  `a4dc042f1502b7e18161c2a41c1c576aec5ac390243518a591f238cba6f196de`.
  The current ELF matches the frozen test artifact.
- Results: `guest/build/phase111-dynamic-quick-20261025/`. Source diff, frozen
  images/configs, disassembly, disabled-CPU evidence and final results live there.
  This is a full quick pass, not an overnight or secondary-execution test.
- Normal guest Image is reused unchanged; no SD files or saved U-Boot environment
  are modified by these tests.

This work changes stack resource management, not secondary execution, allocator
concurrency (11.2), the release/entry protocol (11.3), or guest vCPU count.
