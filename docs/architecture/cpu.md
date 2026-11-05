# Physical CPUs and secondary startup

All enabled CPUs in the supported four-core BCM2711 cluster enter tvisor-owned
EL2 contexts. CPU0 runs Linux; secondary CPUs remain host-only. The
[platform contract](platform.md#secondary-firmware-contract) is a prerequisite
for every release, including coherency before shared Rust/atomic access.

## Topology and stable storage

[CPU discovery](../../tvisor_util/cpu_topology.rs) compares full MPIDR affinity
(`0xff00ffffff`), not just Aff0. The currently executing boot CPU becomes logical
CPU0; enabled secondary CPUs receive bounded logical slots. Disabled CPUs remain
in the description without an enabled logical slot. Duplicate affinities,
capacity overflow and malformed CPU descriptions fail discovery. PSCI metadata
can be described, but discovery never calls firmware or releases a CPU.

[PCPU_SLOTS](../../src/pcpu.rs) is a static array of four
[`PcpuSlot<Vcpu>`](../../tvisor_util/pcpu.rs) objects. Each owns:

- An `UnsafeCell<PcpuContext<Vcpu>>`, the canonical persistent identity and stack
  bounds. Only the owning CPU may modify `host_sp` and `active_vcpu`.
- `SecondaryStartup`: publication/lifecycle mailbox, local checkpoint, original
  entry observations and validation failure bits.
- A one-time initialization claim, preventing concurrent/repeated preparation.

`CpuIdentity` snapshots only immutable fields after preparation acquisition.
`StartupView` is a transient acquired view of that identity, startup metadata,
generation and shared EL2 configuration. Neither is persistent duplicate storage.
Shared readers must not borrow the whole context while its CPU can write the
world-switch fields. Physical contexts do not own VM allocations or vCPU lifetime.

The shared CPU utility validates secondary contexts against the caller's fixed
slot array. Both coherent entry and the boot CPU's acquired Parked check use
this same host-tested path: the selected slot must be prepared, its pointer and
immutable identity/stack bounds must match, and its world-switch fields must
remain idle. The check reads individual fields rather than borrowing the whole
context. It does not grant publication, release or guest-execution permission.

Each CPU has a 64 KiB downward-growing stack and a 4 KiB unmapped lower guard.
CPU0's stack is linker-owned. Enabled secondary stacks are exclusive physical
allocations, mapped Normal RW/NX and zeroed before context publication.
Allocation failure rolls back before mapping; mapping/preparation failure retains
potentially exposed allocations and blocks boot. Stack layouts and guards are
revalidated before release.

## Assembly layout

Offsets below are bytes relative to the slot, unless stated otherwise. The Rust
layout assertions and [linked checker](../../scripts/checks/secondary_entry.py)
must agree with assembly; a source-only size check does not verify linked code.

| Object/field | Offset or size |
|---|---|
| Context; `host_sp` / `active_vcpu` | 0; fields at 0 / 8 |
| Logical ID / affinity | 16 / 24 |
| Stack bottom / top / present | 32 / 40 / 48 |
| Startup metadata / ready generation | 64 |
| Lifecycle / unique entry claim | 72 / 80 |
| Local checkpoint / entry checkpoint / failure mask | 88 / 248 / 280 |
| Preparation claim | 320 |
| Context size/alignment | 64 / 64 |
| Startup metadata size/alignment | 256 / 64 |
| Slot size/alignment; four-slot array size | 384 / 64; 1536 |
| Config MAIR / TCR / root / SCTLR, relative to config | 0 / 8 / 16 / 24 |
| Config size/alignment; enclosing storage size | 64 / 64; 128 |

`HOST_EL2_CONFIG` is shared, immutable, write-once storage constructed from
validated host translation metadata. It is not copied from arbitrary live CPU
registers and cannot be replaced after failure.

## Preparation, publication and release

[The boot coordinator](../../src/secondary.rs) uses one path for every enabled
secondary. It does not silently skip a failing CPU or fall back to another
startup mechanism.

1. Validate board/CPU/cluster selection, each unique aligned release cell and
   the physical-counter deadline. A cell shared with a disabled CPU is rejected
   too. Full firmware holding-memory ownership must already be reserved.
2. Prepare the shared configuration and per-slot nonzero generations. Validate
   actual entry, vectors, writable objects, config, table arena, release pages,
   all stacks and guard mappings under the EL2 table lock. Freeze ordinary
   writers in that same critical section.
3. Validate every rounded cache-maintenance interval before doing maintenance.
   Clean shared executable/vector/rodata/writable/table ranges once and each
   private stack separately. Cleaning the writable image covers all scanned
   identities, mailboxes, claims and shared configuration storage. Complete
   publication barriers before any firmware release.
4. For each secondary, move Prepared to Starting once. Complete the pre-release
   barrier, recheck its cell is zero, record a physical-counter timestamp before
   the LE64 store, mark potential release before that store, clean the cell
   line, complete barriers and send SEV. PA zero uses assembly, not a Rust
   reference. Release-attempt tracking means “possibly released,” not “arrived.”
5. Wait without resource locks; verify the acquired checkpoint and read back
   released/unreleased cells. Before aggregate success require the exact expected
   slot/generation set, complete attempted-release mask and no fatal condition.

[Publication helpers](../../tvisor_util/startup_publication.rs) retain one-shot
state after any ambiguous hardware error. They do not clear cells, reclaim
resources or retry release because a wait failed.

## Stackless secondary entry

Before any stack access or ordinary Rust call, the trampoline checks CurrentEL,
entry SCTLR M/C/EE, Cortex-A72 identity and maintenance participation. It matches
full affinity against present contexts in exactly four slots, rejecting unknown
or duplicate matches and a logical ID inconsistent with its index.

It checks generation, stack geometry/alignment, context alignment and root
alignment, then installs private SP_EL2, TPIDR_EL2, VBAR_EL2 and shared host
controls. Shared configuration supplies MAIR/TCR/root/SCTLR. The entry performs
the required cache/TLB/barrier sequence and enables the host translation regime.
Only then does an acquire load recheck the same publication generation before
crossing into Rust. Unsupported preconditions enter a holding loop.

Rust verifies the pointer belongs to static slot storage, acquires the config
and generation, and obtains the unique `EntryClaim` before writing diagnostics.
The call ABI passes x0=slot, x1–x4=original SCTLR/MIDR/CPUECTLR/MPIDR and
x5=generation. Original observations remain in registers until the coherent
boundary; the trampoline never clears BSS or repeats global initialization.
[Local validation](../../tvisor_util/secondary_validation.rs) checks original
entry observations, identity, SP/TPIDR/VBAR, translation and control registers,
DAIF, timer controls and stack bounds. It publishes the checkpoint and either
Parked or Failed. Readers acquire that terminal observation before reading
checkpoint fields; they do not inspect partially written Starting diagnostics.

## Lifecycle, wait results and boot policy

These have different owners and meanings:

| State | Owner and meaning |
|---|---|
| Offline | Context has not been initialized |
| Prepared | CPU0 release-published immutable context identity and bounds |
| Starting | CPU0 reserved one startup request; firmware arrival is not implied |
| Parked | Entrant validated/checkpointed, or initialized worker later parked |
| Online | Owner completed management initialization and can service requests |
| Failed | Owner reported genuine initialization/management failure |

The startup path is Offline → Prepared → Starting → Parked/Failed. Only the
unique entry claim may complete initial arrival. Dropping a claim leaves
Starting. Management later moves a valid Parked worker to Online and supports
responsive parking; there is no reset/retry/hotplug API. CPU0's preparation
mailbox is not a description of its running execution state.

`wait_until_parked` is observational. Timeout, invalid generation/deadline and
poll-limit errors do not mutate lifecycle or entry ownership. Multiple waits
can return different results, and a fresh wait may observe a late Parked CPU.
Waits use checked frequency conversion, wrap-safe counter differences, a physical
deadline and a finite poll fallback.

[`BootStatus`](../../tvisor_util/secondary_boot.rs) separately records the
one-shot aggregate operation: Idle → Preparing → Releasing → Ready, or terminal
PreparationFailed/ReleaseFailed/WaitFailed. A later Parked observation cannot
reverse a failed boot operation. Guest execution also requires no shared fatal
condition. Late/failed CPUs retain stacks, slots, config and mappings until reset.

## Runtime workers

After startup CPU0 activates per-CPU GIC management interfaces and verifies
bidirectional communication. Normal workers are then parked and freshly pinged;
they retain their EL2 mappings and answer management SGIs. Parked does not mean
powered off or excluded from [TLB synchronization](memory.md#synchronized-el2-mapping-updates).
Management operations serialize lifecycle changes against mapping membership.
No worker executes a guest, migrates vCPUs or independently reclaims boot memory.

See [interrupts](interrupts.md) for request and completion ordering and
[hardware tests](../testing/hardware-tests.md) for success and timeout coverage.
