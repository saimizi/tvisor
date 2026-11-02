//! Persistent EL2 mapping-update membership and epoch acknowledgements.
//! Architectural TLBI completion is a separate obligation of the table writer.
use crate::cpu_topology::MAX_CPUS;
use core::sync::atomic::{AtomicU64, Ordering};
pub const SCRATCH_VA: u64 = 0x1_0000_0000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Phase {
    Idle,
    Quiescing,
    Replacing,
    Synchronizing,
    Complete,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Busy,
    Failed,
    Membership,
    Epoch,
    Phase,
    MissingAck,
    Exhausted,
}
/// One persistent protocol, not a release capability or transient transaction.
/// Gate 1 owns an update; gate 2 serializes lifecycle changes against membership.
pub struct UpdateState {
    gate: AtomicU64,
    epoch: AtomicU64,
    members: AtomicU64,
    phase: AtomicU64,
    requested: [AtomicU64; MAX_CPUS],
    quiet: [AtomicU64; MAX_CPUS],
    synced: [AtomicU64; MAX_CPUS],
}
impl UpdateState {
    pub const fn new() -> Self {
        Self {
            gate: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            members: AtomicU64::new(0),
            phase: AtomicU64::new(0),
            requested: [const { AtomicU64::new(0) }; MAX_CPUS],
            quiet: [const { AtomicU64::new(0) }; MAX_CPUS],
            synced: [const { AtomicU64::new(0) }; MAX_CPUS],
        }
    }
    pub fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Acquire) {
            0 => Phase::Idle,
            1 => Phase::Quiescing,
            2 => Phase::Replacing,
            3 => Phase::Synchronizing,
            4 => Phase::Complete,
            _ => Phase::Failed,
        }
    }
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
    pub fn members(&self) -> u64 {
        self.members.load(Ordering::Acquire)
    }
    pub fn try_membership_change(&self) -> bool {
        self.phase() != Phase::Failed
            && self
                .gate
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            && self.phase() != Phase::Failed
    }
    pub fn finish_membership_change(&self) {
        assert_eq!(self.gate.swap(0, Ordering::Release), 2);
    }
    pub fn begin(&self, members: u64) -> Result<u64, Error> {
        if self.phase() == Phase::Failed {
            return Err(Error::Failed);
        }
        if members & 1 == 0 || members >> MAX_CPUS != 0 || members == 1 {
            return Err(Error::Membership);
        }
        self.gate
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Busy)?;
        let previous = self.phase();
        if !matches!(previous, Phase::Idle | Phase::Complete) {
            self.fail();
            return Err(Error::Phase);
        }
        let Some(epoch) = self.epoch().checked_add(1) else {
            self.fail();
            return Err(Error::Exhausted);
        };
        self.members.store(members, Ordering::Relaxed);
        self.epoch.store(epoch, Ordering::Release);
        self.phase
            .compare_exchange(
                previous as u64,
                Phase::Quiescing as u64,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| Error::Failed)?;
        Ok(epoch)
    }
    fn check(&self, id: usize, epoch: u64, phase: Phase) -> Result<(), Error> {
        if self.phase() == Phase::Failed {
            return Err(Error::Failed);
        }
        if id >= MAX_CPUS || self.members() & (1 << id) == 0 {
            return Err(Error::Membership);
        }
        if epoch == 0 || self.epoch() != epoch {
            return Err(Error::Epoch);
        }
        if self.phase() != phase || self.gate.load(Ordering::Acquire) != 1 {
            return Err(Error::Phase);
        }
        Ok(())
    }
    /// Bounded IRQ-side admission. Completion is the owner's later safe point.
    pub fn admit(&self, id: usize, epoch: u64) -> Result<(), Error> {
        self.check(id, epoch, Phase::Quiescing)?;
        if id == 0 {
            return Err(Error::Membership);
        }
        let previous = self.requested[id].load(Ordering::Acquire);
        if previous >= epoch {
            return Err(Error::Epoch);
        }
        self.requested[id]
            .compare_exchange(previous, epoch, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| Error::Epoch)
    }
    pub fn pending(&self, id: usize) -> bool {
        id < MAX_CPUS
            && self.phase() == Phase::Quiescing
            && self.requested[id].load(Ordering::Acquire) == self.epoch()
            && self.quiet[id].load(Ordering::Acquire) != self.epoch()
    }
    pub fn quiesce(&self, id: usize, epoch: u64) -> Result<(), Error> {
        self.check(id, epoch, Phase::Quiescing)?;
        if id != 0 && self.requested[id].load(Ordering::Acquire) != epoch {
            return Err(Error::Epoch);
        }
        self.quiet[id].store(epoch, Ordering::Release);
        Ok(())
    }
    fn all(&self, acks: &[AtomicU64; MAX_CPUS]) -> bool {
        (0..MAX_CPUS).all(|id| {
            self.members() & (1 << id) == 0 || acks[id].load(Ordering::Acquire) == self.epoch()
        })
    }
    pub fn all_quiet(&self) -> bool {
        self.phase() == Phase::Quiescing && self.all(&self.quiet)
    }
    pub fn start_replace(&self, epoch: u64) -> Result<(), Error> {
        self.check(0, epoch, Phase::Quiescing)?;
        if !self.all(&self.quiet) {
            return Err(Error::MissingAck);
        }
        self.phase
            .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| Error::Phase)
    }
    pub fn may_write(&self, epoch: u64) -> bool {
        self.check(0, epoch, Phase::Replacing).is_ok() && self.all(&self.quiet)
    }
    /// Called only after the writer's break/make maintenance and broadcast DSB
    /// have completed. This publication alone does not execute TLBI or an ISB.
    pub fn publish(&self, epoch: u64) -> Result<(), Error> {
        self.check(0, epoch, Phase::Replacing)?;
        self.phase
            .compare_exchange(2, 3, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| Error::Phase)
    }
    /// Each owner executes its local synchronization and stops using old backing
    /// before acknowledging. A stale acknowledgement cannot release resources.
    pub fn synchronized(&self, id: usize, epoch: u64) -> Result<(), Error> {
        self.check(id, epoch, Phase::Synchronizing)?;
        self.synced[id].store(epoch, Ordering::Release);
        Ok(())
    }
    pub fn all_synced(&self) -> bool {
        self.phase() == Phase::Synchronizing && self.all(&self.synced)
    }
    pub fn finish(&self, epoch: u64) -> Result<(), Error> {
        self.check(0, epoch, Phase::Synchronizing)?;
        if !self.all(&self.synced) {
            return Err(Error::MissingAck);
        }
        self.phase
            .compare_exchange(
                Phase::Synchronizing as u64,
                Phase::Complete as u64,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| Error::Phase)?;
        self.gate.store(0, Ordering::Release);
        Ok(())
    }
    pub fn reusable(&self, epoch: u64) -> bool {
        epoch != 0
            && self.epoch() == epoch
            && self.phase() == Phase::Complete
            && self.all(&self.synced)
    }
    /// Sticky failure: do not unlock, retry, reclaim, or accept late completion.
    pub fn fail(&self) {
        self.phase.store(Phase::Failed as u64, Ordering::Release);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "concurrent protocol stalled");
            std::thread::yield_now();
        }
    }

    #[test]
    fn concurrent_owners_observe_every_epoch_before_backing_reuse() {
        let state = UpdateState::new();
        let mapping = AtomicU64::new(0);
        std::thread::scope(|scope| {
            for id in 1..MAX_CPUS {
                let state = &state;
                let mapping = &mapping;
                scope.spawn(move || {
                    for expected in 1..=128 {
                        until(|| state.pending(id));
                        assert_eq!(state.epoch(), expected);
                        assert_eq!(mapping.load(Ordering::Acquire), expected - 1);
                        state.quiesce(id, expected).unwrap();
                        until(|| state.phase() == Phase::Synchronizing);
                        assert_eq!(mapping.load(Ordering::Acquire), expected);
                        state.synchronized(id, expected).unwrap();
                    }
                });
            }
            for expected in 1..=128 {
                let epoch = state.begin(15).unwrap();
                assert_eq!(epoch, expected);
                for id in 1..MAX_CPUS {
                    state.admit(id, epoch).unwrap();
                }
                state.quiesce(0, epoch).unwrap();
                until(|| state.all_quiet());
                state.start_replace(epoch).unwrap();
                // Model publication only; hardware maintenance is audited and
                // tested separately on the target.
                mapping.store(epoch, Ordering::Release);
                state.publish(epoch).unwrap();
                state.synchronized(0, epoch).unwrap();
                until(|| state.all_synced());
                state.finish(epoch).unwrap();
                assert!(state.reusable(epoch));
            }
        });
    }

    fn quiet(s: &UpdateState, e: u64) {
        s.quiesce(0, e).unwrap();
        for id in 1..MAX_CPUS {
            s.admit(id, e).unwrap();
            s.quiesce(id, e).unwrap();
        }
    }
    #[test]
    fn exact_epochs_and_all_members_are_required_before_write_and_reuse() {
        let s = UpdateState::new();
        for _ in 0..3 {
            let e = s.begin(15).unwrap();
            assert!(!s.may_write(e));
            assert!(!s.reusable(e));
            assert_eq!(s.start_replace(e), Err(Error::MissingAck));
            quiet(&s, e);
            s.start_replace(e).unwrap();
            assert!(s.may_write(e));
            assert!(!s.reusable(e));
            s.publish(e).unwrap();
            for id in 0..3 {
                s.synchronized(id, e).unwrap();
            }
            assert_eq!(s.finish(e), Err(Error::MissingAck));
            s.synchronized(3, e).unwrap();
            s.finish(e).unwrap();
            assert!(s.reusable(e));
            assert!(!s.reusable(e + 1));
        }
    }
    #[test]
    fn late_or_duplicate_requests_and_failed_updates_never_authorize() {
        for phase in 0..3 {
            let s = UpdateState::new();
            let e = s.begin(15).unwrap();
            assert!(s.admit(1, e + 1).is_err());
            s.admit(1, e).unwrap();
            assert!(s.admit(1, e).is_err());
            s.quiesce(0, e).unwrap();
            for id in 1..4 {
                if id != 1 {
                    s.admit(id, e).unwrap();
                }
                s.quiesce(id, e).unwrap();
            }
            if phase > 0 {
                s.start_replace(e).unwrap();
            }
            if phase > 1 {
                s.publish(e).unwrap();
            }
            s.fail();
            assert_eq!(s.begin(15), Err(Error::Failed));
            assert!(!s.reusable(e));
            assert!(!s.may_write(e));
            assert!(s.synchronized(2, e).is_err());
            assert!(s.finish(e).is_err());
        }
    }
    #[test]
    fn lifecycle_mutation_and_updates_cannot_race() {
        let s = UpdateState::new();
        assert!(s.try_membership_change());
        assert_eq!(s.begin(15), Err(Error::Busy));
        s.finish_membership_change();
        let e = s.begin(15).unwrap();
        assert!(!s.try_membership_change());
        quiet(&s, e);
        s.start_replace(e).unwrap();
        s.publish(e).unwrap();
        for id in 0..4 {
            s.synchronized(id, e).unwrap();
        }
        s.finish(e).unwrap();
        assert!(s.try_membership_change());
        s.finish_membership_change();
        s.fail();
        assert!(!s.try_membership_change());
        assert_eq!(s.begin(15), Err(Error::Failed));
    }
    #[test]
    fn invalid_membership_and_unrequested_owners_are_rejected() {
        let s = UpdateState::new();
        for mask in [0, 1, 14, 31] {
            assert_eq!(s.begin(mask), Err(Error::Membership));
        }
        let e = s.begin(11).unwrap();
        assert!(s.admit(2, e).is_err());
        assert!(s.admit(4, e).is_err());
        assert!(s.quiesce(1, e).is_err());
        assert!(s.quiesce(0, e + 1).is_err());
        assert!(s.synchronized(0, e).is_err());
    }
}

#[cfg(test)]
mod terminal_tests {
    use super::*;
    #[test]
    fn sequence_exhaustion_and_failure_are_not_reversible() {
        let s = UpdateState::new();
        s.epoch.store(u64::MAX, Ordering::Relaxed);
        assert_eq!(s.begin(15), Err(Error::Exhausted));
        assert_eq!(s.phase(), Phase::Failed);
        assert!(!s.try_membership_change());
        assert_eq!(s.begin(15), Err(Error::Failed));
    }
    #[test]
    fn failure_cannot_be_replaced_by_a_late_final_ack() {
        let s = UpdateState::new();
        let e = s.begin(3).unwrap();
        s.admit(1, e).unwrap();
        s.quiesce(0, e).unwrap();
        s.quiesce(1, e).unwrap();
        s.start_replace(e).unwrap();
        s.publish(e).unwrap();
        s.synchronized(0, e).unwrap();
        s.synchronized(1, e).unwrap();
        s.fail();
        assert!(s.finish(e).is_err());
        assert_eq!(s.phase(), Phase::Failed);
        assert!(!s.reusable(e));
    }
}
