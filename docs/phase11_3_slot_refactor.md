# Phase 11.3 per-CPU startup storage refactor

2026-11-01: **implementation and repeated acceptance complete; ready for review.** This design supersedes the singleton-record ABI
in the original Task 3–15 reports. The earlier evidence remains a historical
record of that implementation, not acceptance evidence for this ABI.

## Storage and ownership

`SecondaryRecord`, `RecordStorage` and `SECONDARY_RECORD` are removed.
`tvisor_util::pcpu::PcpuSlot<V>` is the production layout used by both bare-metal
code and host tests. `src/pcpu.rs` owns the lifetime-stable `PCPU_SLOTS` array.

Each slot contains an `UnsafeCell<PcpuContext<V>>`, its own `SecondaryStartup`,
and one-time preparation flags. `PcpuContext` is the only persistent source of
logical ID, full affinity and stack bounds. Its address is derived from the slot;
no context pointer is stored elsewhere in startup metadata. The owner's mutable
`host_sp` and `active_vcpu` fields retain their existing world-switch offsets.

`SecondaryStartup` contains only the production mailbox, local checkpoint,
original entry observations and failure bits. Its mailbox's ready word is the
authoritative generation. Worker state and terminal outcome are independent
for every slot; there is no second generation or preparation handoff protocol.
The overall CPU0 `SecondaryBoot` gate remains one-shot for this milestone.

`HOST_EL2_CONFIG` is separate shared, write-once storage for MAIR/TCR/root/SCTLR.
CPU0 constructs it from the validated `HostTranslation` policy before publishing
the selected slot's ready generation. Once claimed it cannot be replaced, even
after failure. It is not reconstructed from a secondary's live registers.

Rust's `StartupView` is a transient acquired view, never an assembly-visible
record or global copy: it references the slot diagnostics and shared config,
and snapshots immutable context fields. `PcpuSlot::identity` individually reads
those fields through raw pointers after preparation acquisition. It creates no
shared reference to a whole mutable context. The owner-only idle-context check
reads world-switch fields only before arrival, or after CPU0 acquires Parked;
Phase 11.3 secondaries never execute guests or mutate those fields.

## Assembly ABI

All assembly-visible offsets, sizes and alignments have compile-time assertions. The actual
`Vcpu` specialization also asserts the same slot stride as the host-tested type.

| Object | Offset/size in bytes |
|---|---|
| `PcpuSlot.context` | 0, size 64 |
| Context `host_sp` / `active_vcpu` | 0 / 8 |
| Context logical ID / affinity | 16 / 24 |
| Context stack bottom / top / present | 32 / 40 / 48 |
| Slot startup / mailbox | 64 |
| Slot ready generation / worker / outcome | 64 / 72 / 80 |
| Slot local checkpoint / entry checkpoint / failure bits | 88 / 248 / 280 |
| `SecondaryStartup` size/alignment | 256 / 64 |
| Slot preparation flags | 320 / 321 |
| `PcpuSlot` size/alignment; four-slot array | 384 / 64; 1536 |
| `HostEl2Config` MAIR / TCR / root / SCTLR | 0 / 8 / 16 / 24 |
| Config size/alignment; enclosing storage size | 64 / 64; 128 |

Preparation rejects invalid identity/stack data before claiming storage, then
claims once, writes the context and release-publishes preparation. Startup
publication requires a prepared non-boot slot, valid shared policy and nonzero
generation. Repeated or stale publication cannot replace the first generation.
Production slot/config storage is static and allocated stacks remain retained.

## Secondary entry

The firmware EL2/MMU/cache/endianness/coherency contract is unchanged. Before
stack or ordinary Rust access, the trampoline validates EL, SCTLR entry bits,
A72 identity and SMPEN, then reads full MPIDR affinity.

It scans exactly four 384-byte slots, considering only contexts with `present=1`.
Affinities are compared in full and need not be dense or equal logical IDs.
The scan rejects duplicate matches and an unknown affinity. The matched logical
ID must equal that slot's index and must be nonzero. It then checks a nonzero
ready generation, private stack geometry/alignment, context alignment and the
shared root alignment before installing SP/TPIDR/VBAR. It never reads the
mutable `host_sp`/`active_vcpu` fields or uses them to locate a slot.

MAIR/TCR/root/SCTLR are loaded from `HOST_EL2_CONFIG`, not per-slot copies.
The established host controls and translation/cache/TLB barriers remain intact.
The trampoline acquire-loads the same slot's ready generation after enabling
the host mappings and checks it against its original observation. Rust receives
the slot pointer in x0, entry observations in x1–x4 and generation in x5.

Rust first verifies the pointer belongs to static slot storage, acquire-observes
the config and slot token, and claims Entered before writing diagnostics. The
same local validator consumes immutable identity plus shared config, then
publishes Arrived/Parked or Failed. Permanent holding loops keep DAIF masked.

## Publication, lifetime and release scope

The real-resource validator now checks the complete slot array, the full config
storage and the selected context, including object extents that cross pages.
Context must lie inside the slot array; config must be disjoint from it. Both
objects must be entirely inside validated RW/NX Normal writable storage. Final
publication matches the actual selected slot/config addresses to these resources
and matches the slot index to the selected CPU. Hosted callbacks model those
physical addresses without replacing production object validation.

The complete writable image is cleaned, so all scanned slot identities, selected
mailbox/diagnostics, configuration and publication flags are covered. Existing
text/vector/rodata/table/usable-stack cleaning, cache-line rounding, mapping freeze,
pre-store timestamp, zero recheck, single LE64 store, clean/DSB/SEV and bounded
wait are preserved. No resource lock spans release or waiting.

Only logical CPU1 is eligible for release in this milestone. CPU2/CPU3 contexts
may be prepared but their startup mailboxes are never published by production
boot. Their release cells remain zero. Independent slot state is a foundation
for later milestones; no concurrent/all-core release is added here.

## Validation

The migrated suite has 167 tests, passing in debug and release. New production-
storage tests cover sparse/full affinities, duplicate matches, independent
generations/outcomes/checkpoints, shared config, stale/zero/repeated publication,
concurrent preparation/config initialization, acquire visibility and identity
reads while the owner mutates its separate world-switch fields. Tests also cover
incorrect slot/config physical addresses and complete object mappings.

Normal and timeout-test target builds pass without warnings in both profiles.
The linked checker verifies the bounded scan, exact stride/limit, duplicate
rejection, all allowed memory operands, shared-config materialization, pointer/
generation ABI and preserved maintenance sequence. Ten corrupted ELF copies
per image are rejected, including wrong stride/limit/config offset and removed
duplicate-affinity rejection, alongside the original six corruptions.

Repeated physical failure acceptance passed: CPU0 timed out at 108,000,029 ticks
against 108,000,000 ticks/54 MHz; CPU1 later reached Parked with zero failures.
Its retained slot and context were both `0x407bfc0`, using the original private
stack and shared root. TimedOut/WaitFailed remained terminal, late Success and
retry were rejected, other cells stayed zero, and guest entry was blocked. A
quiet interval and separate verified power-off completed the run.

Local logs, exact code snapshots and this run's artifacts are retained under
ignored `docs/evidence/phase11_3_slot_refactor/`. The complete quick-regression evidence
is in `guest/build/phase113-slot-refactor-20261101/`. Its result is PASS:

| Repeated normal-image check | Result |
|---|---|
| Normal / diagnostic boots | 2 / 2, all with CPU1 generation 1 Success/Parked |
| Console commands / increasing timer samples | 18 / 10 |
| Smoke probe | 35.987 s, 280 memory passes, maximum lateness 7.616 ms |
| Full soak | 600.459 s, 2,136 wakeups, 8,137 memory passes, maximum lateness 7.588 ms |
| Final probe | 5.628 s, 20 wakeups, maximum lateness 4.000 ms |
| Wakeups over 500 ms / fatal or reset markers | Zero / none |
| Other secondary release cells | Zero on all four fresh boots |
| Final cleanup | Runner cleanup and separate verified relay-off query |

The independent timer samples increased from 20,762 to 155,950. Every fresh boot
had exactly one CPU1 checkpoint with zero local failures; historical UART
snapshots were excluded from this assessment. Known prompt-wait notices were
resolved using fresh output and successful unique command completion markers.
The normal guest Image was unchanged; the diagnostic fixture was isolated, with
the recorded proc/sysfs/initramfs changes and its embedded probe verified.
No U-Boot environment was saved and no SD-card write was performed.

The actual images, local-check logs, failure UART/quiet interval/cleanup,
full quick report, dirty-source snapshots and hashes are retained. The new
`docs/evidence/phase11_3_slot_refactor/review-bundle.tar.gz` and adjacent manifest
consolidate this refactoring's evidence. Older Task 11–15 bundles are unchanged
and are not substituted for the repeated acceptance above.

The tested normal debug ELF SHA256 is
`b7e645e5a6539a7b7d326307f284c674e3040473e06047d7ce433d966dbefaac`.

No source commit/push or PR comment is made as part of this implementation turn.
