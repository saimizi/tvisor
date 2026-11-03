# Architecture and boot sequence

Tvisor runs at Non-secure AArch64 EL2 without VHE on the Raspberry Pi 4 Model B.
It replaces U-Boot's execution environment and never returns to it. Linux runs
at EL1/EL0 on CPU0 with one vCPU and 512 MiB of guest RAM. CPU1–CPU3 run host
workers; physical CPU support does not yet expose multiple CPUs to Linux.

## Boot sequence

1. U-Boot loads the tvisor ELF at its linked address and a separate Linux
   `Image`, then supplies the host DTB and exact Image source/file length.
   Placement must be safe before tvisor executes: runtime validation cannot
   undo an earlier overlapping download.
2. The assembly entry masks asynchronous exceptions and clears BSS. Early Rust
   uses the inherited stack to discover the DT-selected console and validate
   EL2/little-endian operation. Failure before console initialization is silent.
3. CPU topology and firmware startup-memory reservations are captured before
   allocation. The boot CPU gets a stable context, a private 64 KiB stack,
   exception vectors and linker-owned EL2 tables. The shared host controls are
   installed and a stackless assembly routine replaces the inherited EL2
   translation regime.
4. On the private environment, CPU0 constructs and publishes the final memory
   map, including the Image source reservation, and initializes the physical
   allocator. Secondary stacks are allocated, their guard holes are excluded
   from broad RAM mappings, usable stack bytes are zeroed, and contexts are
   published. The fixed heap is initialized before releasing workers.
5. CPU0 initializes host GIC management support. It selects the supported
   firmware backend, validates all secondary resources, freezes ordinary table
   mutation, publishes code/data/tables and releases each enabled secondary.
   Each entrant validates its local state before reporting Parked. Any failed
   boot operation blocks guest execution permanently for that boot.
6. Management interfaces are activated and checked in both directions. The
   normal path parks and pings all workers, verifying actual retaining CPU
   membership. Parked workers still service management SGIs. Optional acceptance
   images also run their explicitly selected stress/failure workloads.
7. VM management allocates guest RAM and stage-2 tables, copies/publishes the
   Linux Image and generated guest DTB, maps the virtual GIC CPU interface,
   and enters the vCPU. EL2 handles exits, trapped devices, physical IRQs and
   guest resumption. A terminal failure halts rather than returning to U-Boot.

## Ownership and source map

| Responsibility | Source | Ownership boundary |
|---|---|---|
| Entry and handoff | [main.rs](../../src/main.rs), [boot.rs](../../src/boot.rs) | CPU0 owns takeover and the guest boot decision |
| Discovery | [fdt.rs](../../tvisor_util/fdt.rs), [platform.rs](../../tvisor_util/platform.rs), [smp.rs](../../src/smp.rs) | Owned topology/contracts and finalized immutable physical map |
| Physical CPUs | [pcpu.rs](../../src/pcpu.rs), [utility layout](../../tvisor_util/pcpu.rs) | Stable contexts and guarded stacks independent of VM ownership |
| Secondary startup | [secondary.rs](../../src/secondary.rs), utility `secondary_*` and `startup_*` modules | Firmware handoff, publication, lifecycle and aggregate boot policy |
| Shared memory | [mm.rs](../../src/mm.rs), [heap.rs](../../src/heap.rs) | Physical allocation, EL2 tables and heap have separate synchronization |
| Host management | [management.rs](../../src/management.rs), [channels](../../tvisor_util/management.rs), [tlb.rs](../../src/tlb.rs) | Per-CPU requests and serialized retaining membership |
| Guest execution | [vmctl.rs](../../src/vmctl.rs), [vcpu.rs](../../src/vcpu.rs), [guest.rs](../../src/guest.rs) | VM allocations, per-vCPU state and CPU0's execution loop |
| Interrupts/devices | [exception.rs](../../src/exception.rs), [gicv2.rs](../../tvisor_util/gicv2.rs), [mmio.rs](../../tvisor_util/mmio.rs) | Physical completion and virtual delivery are separate |

The utility library is `no_std` and hosts allocation-free models and testable
protocols as well as hardware definitions. Binary modules connect these to
linker symbols, register access and the actual boot path. Hardware acceptance
workloads are feature-gated separately from ordinary execution.

## Read next

- [CPU storage, startup and lifecycle](cpu.md)
- [Memory, locks and translation updates](memory.md)
- [Physical memory layout and boundary sources](physical-memory-map.md)
- [IRQ routing and completion](interrupts.md)
- [Guest execution and address spaces](virtualization.md)
- [Platform and firmware requirements](platform.md)

The [development plan](../development_plan.md) records capability status and
future work. Architecture pages describe the current implementation and name
remaining gaps explicitly; they are not implementation-progress reports.
