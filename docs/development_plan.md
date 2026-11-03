# Development plan and current status

This is the maintained roadmap and capability summary. Update it when behavior,
validation coverage or priorities change. Architecture and procedures live in
the linked guides; detailed implementation discussions belong in issues/PRs,
and exact historical results belong in immutable evidence bundles.

## Current capabilities

| Area | Status and validation scope | Reference |
|---|---|---|
| Platform discovery and EL2 takeover | Implemented on Raspberry Pi 4 Model B; DT-selected console, RAM/reservations/GIC/CPUs; private stack, vectors and tables | [Platform](architecture/platform.md), [boot sequence](architecture/overview.md) |
| Memory ownership and shared allocation | Implemented; static published memory map, guarded stacks, physical allocator, fixed heap and serialized console | [Memory](architecture/memory.md) |
| Physical CPUs | All four cores enter owned EL2 contexts; startup validation, late-arrival handling and retained resources; initialized parked workers remain responsive | [CPU](architecture/cpu.md) |
| Host management interrupts | Implemented SGI requests/replies and explicit host/guest control profiles; hardware coverage includes masked delivery and non-injection | [Interrupts](architecture/interrupts.md) |
| EL2 mapping synchronization | Implemented narrow scratch update protocol; acceptance covers Online/Parked members and terminal timeout; ordinary mappings stay frozen | [Memory updates](architecture/memory.md#synchronized-el2-mapping-updates) |
| Linux guest | One VM/vCPU on CPU0, 512 MiB RAM, stage 2, generated DTB, PL011 console and virtual timer/GICV delivery; quick/overnight stability coverage | [Virtualization](architecture/virtualization.md) |

See [evidence interpretation](testing/hardware-tests.md#evidence-and-comparison)
for the provenance and limits required when assessing validation. A past pass
does not validate later source changes.

## Known gaps

- Guest execution exposes one vCPU. Guest PSCI CPU_ON/off, virtual SGIs/IPIs,
  vCPU scheduling/migration and cross-CPU guest stage-2 coordination are absent.
- Physical startup is limited to the reviewed BCM2711 Cortex-A72 spin-table
  profile. PSCI calls, warm restart/hotplug and other platforms are unsupported.
- The physical allocator covers a 4 GiB aperture and the heap is fixed-size.
  General live host remapping/table reclamation is not exposed.
- Device emulation is limited to the working Linux platform. Common current-EL/
  lower-EL dispatch, unexpected-source containment, FIFO/LR backpressure and
  full interrupt/device teardown need [further work](architecture/interrupts.md#remaining-work).
- The standalone VM integration harness and the hardcoded x86 host-test alias
  have [test execution limitations](testing/host-tests.md#test-execution-limitations).

## Roadmap

### Consolidate and simplify the current implementation

Reduce duplicated state and unnecessary abstractions before extending guest
execution to multiple CPUs. [Issue #31](https://github.com/saimizi/tvisor/issues/31)
tracks the scope; its issue/PR checklists track implementation progress.

Keep permanent architecture/setup/testing guidance and this roadmap. Review
CPU/startup, memory, VM/vCPU, interrupt and TLB ownership for redundant state,
unnecessary wrappers and fragmented modules. Preserve assembly layouts, memory
ownership, publication/acknowledgement ordering and terminal failure behavior.
Update diagnostics and their parsers together when renaming them.

Completion requires reviewed cohesive interfaces, no speculative guest features,
valid documentation links, resolved or explicitly accounted-for validation gaps,
supported builds/local checks and relevant fresh hardware regression. Track the
working checklist in the PR description rather than another status document.

### Prepare guest interrupt and execution ownership

Before running multiple guest CPUs, settle persistent VM/vCPU device binding,
current/lower-EL dispatch policy, timer ownership, pending-event/backpressure and
teardown semantics. Preserve the current one-vCPU Linux checkpoint while adding
focused protocol tests and hardware probes. Do not infer general ownership from
the transient `active_vcpu` world-switch pointer.

Completion requires explicit source/completion owners, safe handling without a
running vCPU, no lost/duplicated interrupt completion, and regression evidence
covering guest/host state preservation. Design the needed boundaries before
choosing a broader scheduling or assignment framework.

### Add guest multiprocessing

Expose a selected virtual CPU topology independently of physical CPU identity.
Define lifecycle/startup through a guest PSCI-compatible interface, complete
per-vCPU architectural/timer/virtual GIC state, virtual IPIs, an explicit execution
assignment policy and shared VM/stage-2 synchronization. Decide whether initial
execution uses fixed physical assignments; migration/scheduling needs its own
review and must not be assumed by the current refactoring.

Dependencies are validated physical startup/management, clear interrupt ownership
and preserved single-vCPU behavior. First use a controlled multi-vCPU payload to
exercise startup, independent timers and virtual IPIs. Only then allow a Linux
guest to discover and use the intended CPU count. Host tests must cover stale/
duplicate/failed startup, ownership transitions and shootdown targeting; linked
checks must preserve the complete guest context; hardware must demonstrate actual
multi-vCPU execution. Physical CPU acceptance alone cannot satisfy this gate.

Further platforms, device assignment, multiple VMs and generalized scheduling
remain later design work with no implementation commitment here.

## Review and validation policy

Keep changes reviewable and preserve a working checkpoint. Run relevant
[native/static checks](testing/host-tests.md), bare-metal builds and linked
instruction/layout checks when affected. For hardware-sensitive work, run
[acceptance and Linux regression](testing/hardware-tests.md) using frozen guest
fixtures, fresh uniquely named evidence and verified final power-off.

Distinguish implemented behavior, planned behavior and the validation actually
performed. Record hardware unavailability or incomplete runs explicitly. Keep
implementation progress and historical results in issues, PRs and evidence
bundles. Keep this roadmap focused on supported capabilities, limitations,
objectives, dependencies and acceptance criteria.
