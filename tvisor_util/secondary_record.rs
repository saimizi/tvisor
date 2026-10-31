//! Production secondary record ABI and one-shot publication storage.
use crate::secondary_mailbox::SecondaryMailbox;
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

/// Assembly ABI: one immutable startup record, populated before publication.
/// Ordinary fields are immutable after publication. The embedded production
/// mailbox and checkpoint contain the only atomic protocol/diagnostic words.
#[repr(C, align(64))]
pub struct SecondaryRecord {
    pub affinity: u64,
    pub stack_top: u64,
    pub context: u64,
    pub mair: u64,
    pub tcr: u64,
    pub root: u64,
    pub sctlr: u64,
    pub generation: u64,
    pub mailbox: SecondaryMailbox,
    pub logical_id: u64,
    pub stack_bottom: u64,
    pub checkpoint: Checkpoint,
    pub entry: EntryCheckpoint,
    pub failures: AtomicU64,
}
/// Pre-MMU observations, written only by the claimed Rust entrant.
#[repr(C)]
pub struct EntryCheckpoint {
    pub sctlr: AtomicU64,
    pub midr: AtomicU64,
    pub cpuectlr: AtomicU64,
    pub mpidr: AtomicU64,
}
impl EntryCheckpoint {
    const fn new() -> Self {
        Self {
            sctlr: AtomicU64::new(0),
            midr: AtomicU64::new(0),
            cpuectlr: AtomicU64::new(0),
            mpidr: AtomicU64::new(0),
        }
    }
}
#[repr(C)]
pub struct Checkpoint {
    pub current_el: AtomicU64,
    pub affinity: AtomicU64,
    pub sp: AtomicU64,
    pub tpidr: AtomicU64,
    pub vbar: AtomicU64,
    pub mair: AtomicU64,
    pub tcr: AtomicU64,
    pub root: AtomicU64,
    pub sctlr: AtomicU64,
    pub daif: AtomicU64,
    pub hcr: AtomicU64,
    pub cptr: AtomicU64,
    pub hstr: AtomicU64,
    pub vttbr: AtomicU64,
    pub cntvoff: AtomicU64,
    pub cnthctl: AtomicU64,
    pub cnthp_ctl: AtomicU64,
    pub cntp_ctl: AtomicU64,
    pub cntv_ctl: AtomicU64,
    pub spsel: AtomicU64,
}
impl Checkpoint {
    const fn new() -> Self {
        Self {
            current_el: AtomicU64::new(0),
            affinity: AtomicU64::new(0),
            sp: AtomicU64::new(0),
            tpidr: AtomicU64::new(0),
            vbar: AtomicU64::new(0),
            mair: AtomicU64::new(0),
            tcr: AtomicU64::new(0),
            root: AtomicU64::new(0),
            sctlr: AtomicU64::new(0),
            daif: AtomicU64::new(0),
            hcr: AtomicU64::new(0),
            cptr: AtomicU64::new(0),
            hstr: AtomicU64::new(0),
            vttbr: AtomicU64::new(0),
            cntvoff: AtomicU64::new(0),
            cnthctl: AtomicU64::new(0),
            cnthp_ctl: AtomicU64::new(0),
            cntp_ctl: AtomicU64::new(0),
            cntv_ctl: AtomicU64::new(0),
            spsel: AtomicU64::new(0),
        }
    }
}
const _: () = assert!(core::mem::offset_of!(SecondaryRecord, mailbox) == 64);
const _: () = assert!(core::mem::size_of::<SecondaryMailbox>() == 24);
const _: () = assert!(core::mem::align_of::<SecondaryRecord>() == 64);
const _: () = {
    assert!(core::mem::offset_of!(SecondaryRecord, affinity) == 0);
    assert!(core::mem::offset_of!(SecondaryRecord, stack_top) == 8);
    assert!(core::mem::offset_of!(SecondaryRecord, context) == 16);
    assert!(core::mem::offset_of!(SecondaryRecord, mair) == 24);
    assert!(core::mem::offset_of!(SecondaryRecord, tcr) == 32);
    assert!(core::mem::offset_of!(SecondaryRecord, root) == 40);
    assert!(core::mem::offset_of!(SecondaryRecord, sctlr) == 48);
    assert!(core::mem::offset_of!(SecondaryRecord, generation) == 56);
    assert!(core::mem::size_of::<SecondaryRecord>() == 320);
    assert!(core::mem::offset_of!(SecondaryRecord, logical_id) == 88);
    assert!(core::mem::offset_of!(SecondaryRecord, stack_bottom) == 96);
    assert!(core::mem::offset_of!(SecondaryRecord, checkpoint) == 104);
    assert!(core::mem::size_of::<Checkpoint>() == 160);
    assert!(core::mem::offset_of!(SecondaryRecord, entry) == 264);
    assert!(core::mem::size_of::<EntryCheckpoint>() == 32);
    assert!(core::mem::offset_of!(SecondaryRecord, failures) == 296);
    assert!(core::mem::align_of::<SecondaryRecord>() == 64);
};

/// Boot-only input, validated before it is committed to the startup record.
#[derive(Clone, Copy, Debug)]
pub struct StartupRecord {
    pub logical_id: u64,
    pub affinity: u64,
    pub stack_bottom: u64,
    pub stack_top: u64,
    pub context: u64,
    pub mair: u64,
    pub tcr: u64,
    pub root: u64,
    pub sctlr: u64,
    pub generation: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RecordError {
    Invalid,
    AlreadyClaimed,
}

/// The first field is the assembly-visible record. The claim flag is outside
/// the ABI and prevents any subsequent mutation, including after timeout.
#[repr(C, align(64))]
pub struct RecordStorage {
    record: UnsafeCell<SecondaryRecord>,
    claimed: AtomicBool,
}
// SAFETY: one successful initializer writes only ordinary fields before ready
// publication. All subsequent access is immutable or atomic, with no reset API.
unsafe impl Sync for RecordStorage {}
const _: () = assert!(core::mem::offset_of!(RecordStorage, record) == 0);
const _: () = assert!(core::mem::size_of::<RecordStorage>() == 384);
const _: () = assert!(core::mem::offset_of!(RecordStorage, claimed) == 320);
impl RecordStorage {
    pub const fn new() -> Self {
        Self {
            record: UnsafeCell::new(SecondaryRecord {
                affinity: 0,
                stack_top: 0,
                context: 0,
                mair: 0,
                tcr: 0,
                root: 0,
                sctlr: 0,
                generation: 0,
                mailbox: SecondaryMailbox::new(),
                logical_id: 0,
                stack_bottom: 0,
                checkpoint: Checkpoint::new(),
                entry: EntryCheckpoint::new(),
                failures: AtomicU64::new(0),
            }),
            claimed: AtomicBool::new(false),
        }
    }
    pub fn publish(&self, data: StartupRecord) -> Result<(), RecordError> {
        if data.generation == 0
            || data.logical_id == 0
            || data.logical_id >= crate::cpu_topology::MAX_CPUS as u64
            || data.stack_bottom == 0
            || data.stack_bottom & 4095 != 0
            || data.stack_top.checked_sub(data.stack_bottom) != Some(crate::pcpu::STACK_SIZE)
            || data.context == 0
            || data.context & 63 != 0
            || data.root == 0
            || data.root & 4095 != 0
            || data.affinity & !crate::cpu_topology::MPIDR_AFFINITY_MASK != 0
        {
            return Err(RecordError::Invalid);
        }
        if self.claimed.swap(true, Ordering::AcqRel) {
            return Err(RecordError::AlreadyClaimed);
        }
        // Never construct a mutable reference covering concurrently readable atomics.
        unsafe {
            let record = self.record.get();
            core::ptr::addr_of_mut!((*record).affinity).write(data.affinity);
            core::ptr::addr_of_mut!((*record).stack_top).write(data.stack_top);
            core::ptr::addr_of_mut!((*record).context).write(data.context);
            core::ptr::addr_of_mut!((*record).mair).write(data.mair);
            core::ptr::addr_of_mut!((*record).tcr).write(data.tcr);
            core::ptr::addr_of_mut!((*record).root).write(data.root);
            core::ptr::addr_of_mut!((*record).sctlr).write(data.sctlr);
            core::ptr::addr_of_mut!((*record).generation).write(data.generation);
            core::ptr::addr_of_mut!((*record).logical_id).write(data.logical_id);
            core::ptr::addr_of_mut!((*record).stack_bottom).write(data.stack_bottom);
            (&*core::ptr::addr_of!((*record).mailbox))
                .publish(data.generation)
                .expect("private one-shot mailbox must be empty");
        }
        Ok(())
    }
    /// Only the atomic subobject may be referenced before record publication.
    fn mailbox(&self) -> &SecondaryMailbox {
        unsafe { &*core::ptr::addr_of!((*self.record.get()).mailbox) }
    }
    pub fn get(&self, generation: u64) -> Option<&SecondaryRecord> {
        if !self.mailbox().observe_ready(generation) {
            return None;
        }
        // SAFETY: acquire observes completed initialization; no ordinary field
        // can ever be modified again. The reference cannot outlive this storage.
        Some(unsafe { &*self.record.get() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn data() -> StartupRecord {
        StartupRecord {
            logical_id: 1,
            affinity: 0x100,
            stack_bottom: 0x800000,
            stack_top: 0x810000,
            context: 0x420000,
            mair: 0x4ff,
            tcr: 0x80843519,
            root: 0x430000,
            sctlr: 0x30cd183d,
            generation: 7,
        }
    }
    #[test]
    fn rejects_invalid_records_without_consuming_storage() {
        let d = data();
        for invalid in [
            StartupRecord { generation: 0, ..d },
            StartupRecord { logical_id: 0, ..d },
            StartupRecord {
                logical_id: crate::cpu_topology::MAX_CPUS as u64,
                ..d
            },
            StartupRecord {
                affinity: 1 << 63,
                ..d
            },
            StartupRecord {
                stack_bottom: 0,
                ..d
            },
            StartupRecord {
                stack_top: d.stack_top + 16,
                ..d
            },
            StartupRecord {
                context: d.context + 1,
                ..d
            },
            StartupRecord {
                root: d.root + 1,
                ..d
            },
        ] {
            let storage = RecordStorage::new();
            assert_eq!(storage.publish(invalid), Err(RecordError::Invalid));
            assert!(storage.get(d.generation).is_none());
            storage.publish(d).unwrap();
        }
    }
    #[test]
    fn complete_record_published_once_and_retained_after_timeout() {
        let storage = RecordStorage::new();
        assert!(storage.get(0).is_none());
        storage.publish(data()).unwrap();
        let r = storage.get(7).unwrap();
        assert_eq!(r.stack_bottom, 0x800000);
        assert!(storage.get(8).is_none());
        r.mailbox
            .finish(7, crate::secondary_mailbox::Outcome::TimedOut)
            .unwrap();
        assert_eq!(
            storage.publish(StartupRecord {
                generation: 8,
                ..data()
            }),
            Err(RecordError::AlreadyClaimed)
        );
        assert_eq!(storage.get(7).unwrap().generation, 7);
        assert_eq!(
            r.mailbox.outcome(),
            crate::secondary_mailbox::Outcome::TimedOut
        );
    }
    #[test]
    fn concurrent_readers_acquire_the_production_record() {
        let storage = RecordStorage::new();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    let r = loop {
                        if let Some(r) = storage.get(7) {
                            break r;
                        }
                        std::thread::yield_now();
                    };
                    let d = data();
                    assert_eq!(
                        (r.affinity, r.logical_id, r.context),
                        (d.affinity, d.logical_id, d.context)
                    );
                    assert_eq!((r.stack_bottom, r.stack_top), (d.stack_bottom, d.stack_top));
                    assert_eq!(
                        (r.mair, r.tcr, r.root, r.sctlr),
                        (d.mair, d.tcr, d.root, d.sctlr)
                    );
                });
            }
            storage.publish(data()).unwrap();
        });
    }
    #[test]
    fn concurrent_initializers_cannot_replace_a_published_record() {
        let storage = RecordStorage::new();
        std::thread::scope(|scope| {
            let a = scope.spawn(|| storage.publish(data()));
            let b = scope.spawn(|| {
                storage.publish(StartupRecord {
                    generation: 8,
                    ..data()
                })
            });
            let results = [a.join().unwrap(), b.join().unwrap()];
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        });
        assert_ne!(storage.get(7).is_some(), storage.get(8).is_some());
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    use crate::secondary_mailbox::{Outcome, WorkerState};
    fn storage() -> RecordStorage {
        let s = RecordStorage::new();
        s.publish(StartupRecord {
            logical_id: 1,
            affinity: 1,
            stack_bottom: 0x800000,
            stack_top: 0x810000,
            context: 0x420000,
            root: 0x430000,
            generation: 7,
            mair: 0x4ff,
            tcr: 0,
            sctlr: 0,
        })
        .unwrap();
        s
    }
    #[test]
    fn acquired_park_publishes_checkpoint_and_duplicate_entry_cannot_rewrite_it() {
        let s = storage();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let r = s.get(7).unwrap();
                r.mailbox.enter(7).unwrap();
                r.checkpoint.sp.store(0x80ff00, Ordering::Relaxed);
                r.mailbox.acknowledge(7).unwrap();
                r.mailbox.park(7).unwrap();
            });
            let r = s.get(7).unwrap();
            while !r.mailbox.has_parked(7) {
                std::thread::yield_now();
            }
            assert_eq!(r.checkpoint.sp.load(Ordering::Relaxed), 0x80ff00);
            assert!(r.mailbox.enter(7).is_err());
            r.mailbox.finish(7, Outcome::Success).unwrap();
        });
    }
    #[test]
    fn acquired_failure_publishes_checkpoint_without_arrival() {
        let s = storage();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let r = s.get(7).unwrap();
                r.mailbox.enter(7).unwrap();
                r.checkpoint.current_el.store(4, Ordering::Relaxed);
                r.mailbox.fail(7).unwrap();
            });
            let r = s.get(7).unwrap();
            while r.mailbox.state(7) != Ok(WorkerState::Failed) {
                std::thread::yield_now();
            }
            assert_eq!(r.checkpoint.current_el.load(Ordering::Relaxed), 4);
            assert!(!r.mailbox.has_arrived(7));
            assert_eq!(
                r.mailbox.wait_and_finish(7, 0, 10, 1, || 0),
                Ok(Outcome::SecondaryFailed)
            );
        });
    }
}
