# Phase 11.3 startup ABI and ownership

The current ABI is the 2026-11-01 [per-slot startup design](phase11_3_slot_refactor.md).
It replaces the original singleton startup record. There is no separate persistent
identity/stack record: `PcpuContext` is the single source of these fields.

Production types are in [`pcpu.rs`](../tvisor_util/pcpu.rs),
[`secondary_startup.rs`](../tvisor_util/secondary_startup.rs),
[`secondary_mailbox.rs`](../tvisor_util/secondary_mailbox.rs) and
[`el2_policy.rs`](../tvisor_util/el2_policy.rs). The bare-metal array and shared
configuration are owned by `src/pcpu.rs` and `src/secondary.rs` respectively.

## Layout

`PcpuSlot<V>` is `repr(C, align(64))`, 384 bytes. Each array element has its own
runtime context and startup state. The actual `Vcpu` specialization and the
host-tested specialization assert the same layout.

| Slot offset | Contents |
|---|---|
| 0 | `PcpuContext`: 64 bytes, world-switch fields at 0/8 |
| 16 / 24 | Immutable logical ID / full affinity |
| 32 / 40 / 48 | Immutable stack bottom / stack top / present |
| 64 / 72 / 80 | Mailbox ready generation / worker state / terminal outcome |
| 88 | Twenty atomic local checkpoint fields |
| 248 | Four original entry observations |
| 280 | Atomic validation failure mask |
| 320 / 321 | One-time prepared / preparation-claimed flags |

`SecondaryStartup` occupies 256 bytes at slot offset 64. Generation is held only
in its mailbox's ready word. `HostEl2Config` separately contains MAIR, TCR, root
and SCTLR at offsets 0/8/16/24; its enclosing one-shot published storage is 128
bytes. Both config and slot storage are static in the production image.

## Ownership and publication

CPU0 claims each slot once, initializes its context and release-publishes
preparation. Identity and stack bounds can never change after preparation.
Only the owner may write `host_sp` and `active_vcpu` through world-switch
assembly; a shared `PcpuSlot` reference encloses them in `UnsafeCell`.
`identity()` takes individual raw-field snapshots and never borrows a whole
context. Startup diagnostics are atomics, independently owned by each slot.

CPU0 publishes the shared config from validated host policy, then publishes
only logical CPU1's nonzero mailbox generation. Before the physical cell store,
resource validation/freeze and cache publication cover the complete array and
configuration as well as code, vectors, constants, tables and selected stack.
The assembly scan reads only bounded immutable context fields, then the matched
slot's ready word; after enabling mappings it LDAR-acquires that same word.

`StartupView` is an acquired, transient Rust view of one slot's diagnostics,
shared config and immutable identity snapshot. It is never stored globally or
used as a second assembly ABI. Views borrow their backing storage; production
storage remains valid across guest execution and failure. Hosted fixtures keep
storage stable before capturing simulated context addresses.

## Protocol and Rust boundary

The mailbox protocol is unchanged: Ready → Entered → Arrived → Parked, or
Entered → Failed. Ready holds the one-shot generation; each transition checks
it. CPU0 decides the overall terminal outcome exactly once. Parked means locally
validated and about to hold with interrupts masked, not Online.

The trampoline scans full affinities, rejects missing/duplicate matches and
checks the matched index against the context logical ID. It passes x0=slot,
x1–x4=entry observations, x5=generation to Rust. Rust validates the pointer against
the static array, acquire-observes config/ready and claims Entered before writing
a checkpoint. No Rust code, stack memory access, lock or allocation precedes the
coherent acquire boundary.

No timeout, late arrival or failure authorizes a new publication, cell clear,
release retry, mapping change or resource reclamation. A late worker may advance
its diagnostic state but cannot replace a terminal failure. All later attempts
fail the one-shot boot gate; no guest permit is produced after failed startup.
See the current design for ABI checks, tests and repeated physical acceptance.
