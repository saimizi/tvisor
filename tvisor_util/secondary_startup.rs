//! Per-slot startup synchronization/diagnostics; no duplicated CPU bootstrap fields.
use crate::{el2_policy::HostEl2Config, pcpu::CpuIdentity, secondary_mailbox::SecondaryMailbox};
use core::{ops::Deref, sync::atomic::AtomicU64};

#[repr(C, align(64))]
pub struct SecondaryStartup {
    pub mailbox: SecondaryMailbox,
    pub checkpoint: Checkpoint,
    pub entry: EntryCheckpoint,
    pub failures: AtomicU64,
}
impl SecondaryStartup {
    pub const fn new() -> Self {
        Self {
            mailbox: SecondaryMailbox::new(),
            checkpoint: Checkpoint::new(),
            entry: EntryCheckpoint::new(),
            failures: AtomicU64::new(0),
        }
    }
}
/// Transient acquired view, never an assembly-visible record or persistent copy.
/// Identity comes only from immutable context fields; configuration is shared.
#[derive(Clone, Copy)]
pub struct StartupView<'a> {
    pub identity: CpuIdentity,
    pub config: &'a HostEl2Config,
    pub generation: u64,
    startup: &'a SecondaryStartup,
    slot_address: u64,
}
impl<'a> StartupView<'a> {
    pub(crate) fn new(
        identity: CpuIdentity,
        config: &'a HostEl2Config,
        generation: u64,
        startup: &'a SecondaryStartup,
        slot_address: u64,
    ) -> Self {
        Self {
            identity,
            config,
            generation,
            startup,
            slot_address,
        }
    }
    pub fn slot_address(self) -> u64 {
        self.slot_address
    }
    pub fn config_address(self) -> u64 {
        self.config as *const _ as u64
    }
    pub fn mailbox(self) -> &'a SecondaryMailbox {
        &self.startup.mailbox
    }
}
impl Deref for StartupView<'_> {
    type Target = SecondaryStartup;
    fn deref(&self) -> &Self::Target {
        self.startup
    }
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

const _: () = {
    assert!(core::mem::offset_of!(SecondaryStartup, mailbox) == 0);
    assert!(core::mem::offset_of!(SecondaryStartup, checkpoint) == 24);
    assert!(core::mem::offset_of!(SecondaryStartup, entry) == 184);
    assert!(core::mem::offset_of!(SecondaryStartup, failures) == 216);
    assert!(core::mem::size_of::<SecondaryStartup>() == 256);
    assert!(core::mem::size_of::<Checkpoint>() == 160);
    assert!(core::mem::size_of::<EntryCheckpoint>() == 32);
};

#[cfg(test)]
pub(crate) mod test_fixture {
    use super::*;
    use crate::{
        el2_policy::{HostEl2ConfigStorage, HostTranslation},
        pcpu::{PcpuSlot, stack_layout},
    };
    pub struct Fixture {
        pub slot: PcpuSlot<()>,
        pub config: HostEl2ConfigStorage,
    }
    impl Fixture {
        pub fn new(
            id: usize,
            affinity: u64,
            bottom: u64,
            root: u64,
            generation: u64,
        ) -> std::boxed::Box<Self> {
            let f = std::boxed::Box::new(Self {
                slot: PcpuSlot::new(),
                config: HostEl2ConfigStorage::new(),
            });
            f.slot
                .prepare(id, affinity, stack_layout(bottom - 4096).unwrap())
                .unwrap();
            f.config
                .publish(HostTranslation::new(root, 4).unwrap())
                .unwrap();
            f.slot
                .publish_startup(generation, f.config.get().unwrap())
                .unwrap();
            f
        }
        pub fn get(&self, generation: u64) -> Option<StartupView<'_>> {
            self.slot.startup_view(generation, self.config.get()?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        el2_policy::{HostEl2ConfigStorage, HostTranslation},
        pcpu::{PcpuSlot, stack_layout},
        secondary_mailbox::{CpuState, WaitError},
    };
    use core::sync::atomic::Ordering;

    #[test]
    fn slots_have_independent_generations_checkpoints_and_lifecycles() {
        let slots = [PcpuSlot::<()>::new(), PcpuSlot::new(), PcpuSlot::new()];
        let config = HostEl2ConfigStorage::new();
        config
            .publish(HostTranslation::new(0x800000, 4).unwrap())
            .unwrap();
        let c = config.get().unwrap();
        for (id, generation, affinity) in [(1, 7, 0x102), (2, 9, 0x50003)] {
            slots[id]
                .prepare(
                    id,
                    affinity,
                    stack_layout(0x900000 + id as u64 * 0x20000).unwrap(),
                )
                .unwrap();
            assert!(slots[id].publish_startup(0, c).is_err());
            slots[id].publish_startup(generation, c).unwrap();
            assert!(slots[id].publish_startup(generation + 1, c).is_err());
            assert!(slots[id].startup_view(generation + 1, c).is_none());
        }
        let a = slots[1].startup_view(7, c).unwrap();
        let b = slots[2].startup_view(9, c).unwrap();
        assert_ne!(a.identity.context, b.identity.context);
        assert_eq!(a.config_address(), b.config_address());
        a.mailbox.begin_start(7).unwrap();
        b.mailbox.begin_start(9).unwrap();
        assert_eq!(
            a.mailbox.wait_until_parked(7, 0, 10, 1, || 10),
            Err(WaitError::Timeout)
        );
        assert_eq!(a.mailbox.lifecycle(), CpuState::Starting);
        for v in [a, b] {
            let claim = v.mailbox.claim_entry(v.generation).unwrap();
            v.checkpoint
                .sp
                .store(v.identity.stack_top - 16, Ordering::Relaxed);
            if v.identity.logical_id == 1 {
                claim.park().unwrap();
            } else {
                claim.fail().unwrap();
            }
            assert!(v.mailbox.claim_entry(v.generation).is_err());
        }
        assert_eq!(a.mailbox.state(7), Ok(CpuState::Parked));
        assert_eq!(b.mailbox.state(9), Ok(CpuState::Failed));
        assert_eq!(a.mailbox.wait_until_parked(7, 0, 10, 1, || 0), Ok(()));
        assert_eq!(
            b.mailbox.wait_until_parked(9, 0, 10, 1, || 0),
            Err(WaitError::CpuFailed)
        );
        assert_eq!(
            a.checkpoint.sp.load(Ordering::Relaxed),
            a.identity.stack_top - 16
        );
        assert_eq!(
            b.checkpoint.sp.load(Ordering::Relaxed),
            b.identity.stack_top - 16
        );
        assert!(slots[0].identity().is_none());
        assert!(slots[0].startup_view(7, c).is_none());
    }

    #[test]
    fn acquire_observes_real_slot_identity_config_and_checkpoint() {
        let slot = PcpuSlot::<()>::new();
        let config = HostEl2ConfigStorage::new();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                while config.get().is_none() {
                    std::thread::yield_now();
                }
                let c = config.get().unwrap();
                let r = loop {
                    if let Some(v) = slot.startup_view(17, c) {
                        break v;
                    }
                    std::thread::yield_now();
                };
                assert_eq!(r.identity.affinity, 0x12345);
                assert_eq!(r.identity.logical_id, 1);
                assert_eq!(r.identity.stack_bottom, 0x901000);
                assert_eq!(r.identity.context, slot.context_ptr() as u64);
                assert_eq!(r.config.root, 0x800000);
                while r.mailbox.lifecycle() != CpuState::Starting {
                    std::thread::yield_now();
                }
                let claim = r.mailbox.claim_entry(17).unwrap();
                r.checkpoint.sp.store(0x910000, Ordering::Relaxed);
                claim.park().unwrap();
            });
            slot.prepare(1, 0x12345, stack_layout(0x900000).unwrap())
                .unwrap();
            config
                .publish(HostTranslation::new(0x800000, 4).unwrap())
                .unwrap();
            slot.publish_startup(17, config.get().unwrap()).unwrap();
            slot.startup.mailbox.begin_start(17).unwrap();
            while slot.startup.mailbox.state(17) != Ok(CpuState::Parked) {
                std::thread::yield_now();
            }
            assert_eq!(slot.startup.checkpoint.sp.load(Ordering::Relaxed), 0x910000);
        });
    }

    #[test]
    fn late_failure_snapshot_is_acquired_after_a_wait_timeout() {
        let f = test_fixture::Fixture::new(1, 1, 0x900000, 0x800000, 7);
        let r = f.get(7).unwrap();
        r.mailbox.begin_start(7).unwrap();
        assert_eq!(
            r.mailbox.wait_until_parked(7, 0, 10, 1, || 10),
            Err(WaitError::Timeout)
        );
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let claim = r.mailbox.claim_entry(7).unwrap();
                r.failures.store(4, Ordering::Relaxed);
                claim.fail().unwrap();
            });
            while r.mailbox.state(7) != Ok(CpuState::Failed) {
                std::thread::yield_now();
            }
            assert_eq!(r.failures.load(Ordering::Relaxed), 4);
            assert_eq!(
                r.mailbox.wait_until_parked(7, 0, 10, 1, || 0),
                Err(WaitError::CpuFailed)
            );
            assert!(!r.mailbox.has_parked(7));
        });
    }

    #[test]
    fn one_preparer_wins_and_identity_reads_do_not_borrow_owner_mutable_fields() {
        let slot = PcpuSlot::<()>::new();
        let layout = stack_layout(0x900000).unwrap();
        std::thread::scope(|scope| {
            let a = scope.spawn(|| slot.prepare(1, 0x123, layout));
            let b = scope.spawn(|| slot.prepare(2, 0x456, layout));
            assert_eq!(
                [a.join().unwrap(), b.join().unwrap()]
                    .iter()
                    .filter(|x| x.is_ok())
                    .count(),
                1
            );
        });
        let expected = slot.identity().unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for i in 0..10000 {
                    // Simulate owner writes only to its world-switch fields.
                    unsafe {
                        core::ptr::addr_of_mut!((*slot.context_ptr()).host_sp).write(i);
                    }
                }
            });
            for _ in 0..10000 {
                assert_eq!(slot.identity().unwrap(), expected);
            }
        });
    }
}

#[cfg(test)]
mod lookup_tests {
    use crate::{
        el2_policy::{HOST_MAIR, HOST_SCTLR, HostEl2Config, HostTranslation},
        pcpu::{PcpuSlot, find_slot_by_affinity, stack_layout},
    };
    #[test]
    fn sparse_affinities_route_by_full_identity_and_duplicates_are_rejected() {
        let slots = [const { PcpuSlot::<()>::new() }; 4];
        for (id, affinity) in [(0, 0x200), (1, 0x10003), (3, 0x400000005)] {
            slots[id]
                .prepare(
                    id,
                    affinity,
                    stack_layout(0x900000 + id as u64 * 0x20000).unwrap(),
                )
                .unwrap();
            assert!(core::ptr::eq(
                find_slot_by_affinity(&slots, affinity | 0x80000000).unwrap(),
                &slots[id]
            ));
        }
        assert!(find_slot_by_affinity(&slots, 0x5).is_none());
        assert!(find_slot_by_affinity(&slots, 0x10004).is_none());
        slots[2]
            .prepare(2, 0x10003, stack_layout(0xa00000).unwrap())
            .unwrap();
        assert!(find_slot_by_affinity(&slots, 0x10003).is_none());
        assert!(slots[0].startup.mailbox.observe_ready(1) == false);
    }
    #[test]
    fn invalid_preparation_or_config_cannot_consume_publication() {
        let slot = PcpuSlot::<()>::new();
        let layout = stack_layout(0x900000).unwrap();
        for (id, affinity) in [(4, 1), (1, 1 << 63)] {
            assert!(slot.prepare(id, affinity, layout).is_err());
            assert!(slot.identity().is_none());
        }
        let mut bad_layout = layout;
        bad_layout.top += 16;
        assert!(slot.prepare(1, 1, bad_layout).is_err());
        slot.prepare(1, 1, layout).unwrap();
        let policy = HostTranslation::new(0x800000, 4).unwrap();
        let mut config = HostEl2Config {
            mair: HOST_MAIR,
            tcr: policy.tcr(),
            root: policy.root(),
            sctlr: HOST_SCTLR,
        };
        config.root += 1;
        assert!(slot.publish_startup(7, &config).is_err());
        config.root = policy.root();
        slot.publish_startup(7, &config).unwrap();
        assert!(slot.startup_view(7, &config).is_some());
    }
}
