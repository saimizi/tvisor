# Phase 11.2: shared-resource synchronization

Implementation based on approved Phase 11.1 commit `0d24bf9`. Secondary CPUs
remain in firmware; this milestone does not release them or add a scheduler.

## Lock and ownership policy

`tvisor_util::sync::IrqSpinLock` masks local IRQ before acquisition, uses the
existing spin mutex's acquire/release ordering, unlocks before restoring the
saved IRQ mask, and never enables an IRQ that was previously masked. Only the I
bit is changed; D/A/F remain untouched. Guards cannot move between CPUs. Failed
try-lock attempts restore IRQ state too. Hosted builds execute no privileged
instructions; mock CPU-local state tests the protocol.

One physical allocator lock covers initialization, both mutable bitmap views,
allocation/free and statistics. A separate IRQ-save lock protects EL2 table
metadata. Their private closures are fixed internal operations: no arbitrary
client callbacks, allocation, logging or hardware polling is allowed inside.
Mapping diagnostics are emitted after releasing the table lock. Initial mapping
publication remains boot-only; this lock does not implement TLB shootdowns.

The boot CPU initializes the memory map in static storage and publishes it
only after finalization and reservation validation. Readers acquire an immutable
reference without locking; failed initialization is not published. The boot-only
initializer must be called once, and secondaries must establish coherent shared
memory before accessing the published map. Topology and startup contracts remain
`Once` publications, per-CPU context remains owner-only, and VM/vCPU/device
emulation objects remain exclusively boot-CPU-owned.

The existing heap mutex is retained for thread-side allocation, initialization
and statistics. Heap initialization precedes any future worker release. EL2 IRQ
and exception handlers must not allocate, deallocate or request heap statistics.
Guest exit failures now use typed errors instead of `String`/`format!`. Duplicate
timer delivery increments an atomic counter; the boot run loop reports it after
interrupt completion, preserving the regression suite's timer-error marker.

Console TX uses one IRQ-save lock for an entire formatted message; guest byte
output uses that same lock. UART readiness polling remains finite. CPU 0 alone
consumes RX; workers must record IRQ counters rather than print. Do not nest
resource locks or invoke allocating/reentrant Display implementations inside
console formatting. Allocator scans are bounded by the 4 GiB bitmap aperture;
large allocation, initial map construction and initialization are thread/boot
operations, never worker IRQ operations. These bounds are not real-time latency
guarantees and contention fairness is not guaranteed by the spin mutex.

## Fatal evidence

Panic and unexpected current-EL exception paths first publish an allocation-free
atomic failure record. There is a first-failure slot for each logical CPU, an
extra pre-TPIDR slot, and a shared fatal flag. TPIDR is compared with known slot
addresses without dereferencing an inherited pointer. Readers use acquire to
observe a complete snapshot and never wait for a nested/interrupted writer.
Records preserve kind, syndrome, PC and fault address; panic text is best effort.

Emergency console output makes one try-lock attempt and writes at most 512 text bytes (plus newline carriage returns)
through bounded UART polling. A fault while the CPU owns the console lock keeps
its record and halts without waiting for that lock. Early-stop output also uses
the bounded path. The deliberate boot BRK test remains a normal diagnostic path.
Later worker lifecycle/management code must consume the fatal flag; this change
does not claim to stop other CPUs automatically.

## Validation

- `cargo fmt --check` and bare-metal build.
- 120 native library tests: IRQ restoration for enabled/masked entry, failed
  try-lock and unwinding; unlock-before-restore; four-thread lock contention;
  unique ownership of live physical pages and final allocator statistics;
  concurrent heap integrity/coalescing; complete-message console serialization;
  nonblocking fatal output with the console lock held; first-failure evidence.
- Complete lab-rpi4 quick regression passed on 2026-10-29, finished at
  09:57:30 UTC: four boots, 18 console commands and ten timer/console checks.
  The 600.458-second soak completed 2,136 timed wakeups and 8,137 full 32 MiB
  memory verification passes. Maximum wakeup lateness was 7.582 ms; no wakeups
  exceeded 500 ms. Timer samples increased from 20,749 to 155,923. The final
  reboot/short probe passed; runner cleanup and independent relay readback
  confirmed power off. Fresh logs retained four unmapped guards and 9/16 table
  pages, with no fatal/timer-error markers.

Evidence: `guest/build/phase112-quick-20261029-v2/` (ignored build output).
The tested ELF SHA-256 is
`0de007594576d8018a191a1e8d36e8a66363b602acb018504e4bb2cbd0108f4f`.
Its source is `0d24bf9` plus the preserved tracked patch and untracked source
snapshots. The unchanged baseline Image SHA-256 is
`3a72c904e211f56e5f05d601b41495dfa4ec9029b3d7b1ee3556a72f9bb33b15`;
the diagnostic Image and probe were reused unchanged from the preceding
preparation, with their hashes/configuration/provenance recorded in the manifest.

The preliminary `phase112-quick-20261029/` attempt was intentionally stopped
before the soak to preserve the existing timer-warning test marker in the final
implementation. Its first boot passed; it is not a regression pass. Interrupted
cleanup encountered a transient Matter resource-busy query error. Manual cleanup
then powered off the board and independently verified it before the fresh v2
run. The final run completed without intervention or harness changes.

Host contention tests exercise the production lock and allocators with hosted
threads, not physical secondary CPUs. Hardware cross-core coherency, management
interrupts and concurrent allocation on lab-rpi4 remain milestones 11.3–11.5.
