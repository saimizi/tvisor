//! Secondary lifecycle, publication token and side-effect-free readiness waits.
//! Coherent memory is a prerequisite for every atomic access here.
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    InvalidGeneration,
    InvalidTransition,
    DuplicateEntry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum CpuState {
    Offline = 0,
    Prepared = 1,
    /// Startup has been requested, not proof of a completed firmware release.
    Starting = 2,
    Parked = 3,
    /// Management handler and per-CPU interrupt interface are ready.
    Online = 4,
    Failed = 5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitError {
    Timeout,
    CpuFailed,
    PollLimit,
    InvalidDeadline,
    InvalidGeneration,
}

/// Generation is publication metadata, state is lifecycle, and entry_claimed
/// protects the single diagnostic writer. No waiting result is stored here.
#[repr(C)]
pub struct SecondaryMailbox {
    ready: AtomicU64,
    state: AtomicU64,
    entry_claimed: AtomicBool,
}

/// Only a winning coherent Rust entrant may publish Parked/Failed.
/// Dropping a claim leaves Starting and never invents an initialization result.
pub struct EntryClaim<'a> {
    mailbox: &'a SecondaryMailbox,
    generation: u64,
}
impl EntryClaim<'_> {
    pub fn park(self) -> Result<(), ProtocolError> {
        self.mailbox
            .transition(self.generation, CpuState::Starting, CpuState::Parked)
    }
    pub fn fail(self) -> Result<(), ProtocolError> {
        self.mailbox
            .transition(self.generation, CpuState::Starting, CpuState::Failed)
    }
}
const _: () = {
    assert!(core::mem::offset_of!(SecondaryMailbox, ready) == 0);
    assert!(core::mem::offset_of!(SecondaryMailbox, state) == 8);
    assert!(core::mem::offset_of!(SecondaryMailbox, entry_claimed) == 16);
    assert!(core::mem::size_of::<SecondaryMailbox>() == 24);
};
impl SecondaryMailbox {
    pub const fn new() -> Self {
        Self {
            ready: AtomicU64::new(0),
            state: AtomicU64::new(CpuState::Offline as u64),
            entry_claimed: AtomicBool::new(false),
        }
    }
    pub fn lifecycle(&self) -> CpuState {
        match self.state.load(Ordering::Acquire) {
            0 => CpuState::Offline,
            1 => CpuState::Prepared,
            2 => CpuState::Starting,
            3 => CpuState::Parked,
            4 => CpuState::Online,
            5 => CpuState::Failed,
            _ => unreachable!(),
        }
    }
    /// Called only after immutable context fields have been initialized.
    pub(crate) fn mark_prepared(&self) -> Result<(), ProtocolError> {
        self.state
            .compare_exchange(
                CpuState::Offline as u64,
                CpuState::Prepared as u64,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .map(|_| ())
            .map_err(|_| ProtocolError::InvalidTransition)
    }
    pub fn publish(&self, generation: u64) -> Result<(), ProtocolError> {
        if generation == 0 {
            return Err(ProtocolError::InvalidGeneration);
        }
        if self.lifecycle() != CpuState::Prepared {
            return Err(ProtocolError::InvalidTransition);
        }
        self.ready
            .compare_exchange(0, generation, Ordering::Release, Ordering::Relaxed)
            .map(|_| ())
            .map_err(|_| ProtocolError::InvalidTransition)
    }
    pub fn observe_ready(&self, generation: u64) -> bool {
        generation != 0 && self.ready.load(Ordering::Acquire) == generation
    }
    fn check(&self, generation: u64) -> Result<(), ProtocolError> {
        if self.observe_ready(generation) {
            Ok(())
        } else {
            Err(ProtocolError::InvalidGeneration)
        }
    }
    pub fn state(&self, generation: u64) -> Result<CpuState, ProtocolError> {
        self.check(generation)?;
        Ok(self.lifecycle())
    }
    fn transition(
        &self,
        generation: u64,
        from: CpuState,
        to: CpuState,
    ) -> Result<(), ProtocolError> {
        self.check(generation)?;
        self.state
            .compare_exchange(from as u64, to as u64, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| ProtocolError::InvalidTransition)
    }
    /// Called by the consuming release handle before its first firmware operation.
    /// Failure after this transition does not prove that a CPU failed to initialize.
    pub fn begin_start(&self, generation: u64) -> Result<(), ProtocolError> {
        self.transition(generation, CpuState::Prepared, CpuState::Starting)
    }
    /// # Safety
    /// Only the owning CPU may call this, after preparing vectors, management
    /// state and its banked interrupt interface, immediately before IRQ unmask.
    pub unsafe fn management_online(&self, generation: u64) -> Result<(), ProtocolError> {
        self.transition(generation, CpuState::Parked, CpuState::Online)
    }
    /// # Safety
    /// Only the owning CPU may publish this management parking checkpoint.
    /// This is a responsive holding pen, not CPU_OFF or permission to recycle.
    pub unsafe fn management_park(&self, generation: u64) -> Result<(), ProtocolError> {
        self.transition(generation, CpuState::Online, CpuState::Parked)
    }
    /// # Safety
    /// Only the owner may report an actual management initialization failure.
    /// Resources remain retained; this does not permit release or boot retry.
    pub unsafe fn management_failed(&self, generation: u64) -> Result<(), ProtocolError> {
        match self.state(generation)? {
            CpuState::Parked => self.transition(generation, CpuState::Parked, CpuState::Failed),
            CpuState::Online => self.transition(generation, CpuState::Online, CpuState::Failed),
            _ => Err(ProtocolError::InvalidTransition),
        }
    }
    pub fn claim_entry(&self, generation: u64) -> Result<EntryClaim<'_>, ProtocolError> {
        if self.state(generation)? != CpuState::Starting {
            return Err(ProtocolError::InvalidTransition);
        }
        self.entry_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ProtocolError::DuplicateEntry)?;
        Ok(EntryClaim {
            mailbox: self,
            generation,
        })
    }
    pub fn entry_is_claimed(&self, generation: u64) -> Result<bool, ProtocolError> {
        self.check(generation)?;
        Ok(self.entry_claimed.load(Ordering::Acquire))
    }
    pub fn has_parked(&self, generation: u64) -> bool {
        self.state(generation) == Ok(CpuState::Parked)
    }

    /// Waiting never changes lifecycle or publication/entry-claim metadata.
    /// A new wait may succeed after an earlier timeout; boot policy is separate.
    pub fn wait_until_parked<F: FnMut() -> u64>(
        &self,
        generation: u64,
        start: u64,
        ticks: u64,
        max_polls: usize,
        mut counter: F,
    ) -> Result<(), WaitError> {
        self.check(generation)
            .map_err(|_| WaitError::InvalidGeneration)?;
        if ticks == 0 || ticks >= 1 << 63 {
            return Err(WaitError::InvalidDeadline);
        }
        for _ in 0..max_polls {
            if counter().wrapping_sub(start) >= ticks {
                return Err(WaitError::Timeout);
            }
            match self.lifecycle() {
                CpuState::Failed => return Err(WaitError::CpuFailed),
                CpuState::Parked => {
                    return if counter().wrapping_sub(start) >= ticks {
                        Err(WaitError::Timeout)
                    } else {
                        Ok(())
                    };
                }
                _ => {}
            }
            core::hint::spin_loop();
        }
        Err(WaitError::PollLimit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn published() -> SecondaryMailbox {
        let m = SecondaryMailbox::new();
        m.mark_prepared().unwrap();
        m.publish(7).unwrap();
        m
    }
    fn starting() -> SecondaryMailbox {
        let m = published();
        m.begin_start(7).unwrap();
        m
    }
    fn park(m: &SecondaryMailbox) {
        m.claim_entry(7).unwrap().park().unwrap();
    }
    #[test]
    fn preparation_publication_and_start_are_distinct_one_shot_operations() {
        let m = SecondaryMailbox::new();
        assert_eq!(m.lifecycle(), CpuState::Offline);
        assert!(m.publish(7).is_err());
        assert!(m.begin_start(7).is_err());
        m.mark_prepared().unwrap();
        assert!(m.mark_prepared().is_err());
        assert_eq!(m.lifecycle(), CpuState::Prepared);
        assert!(m.publish(0).is_err());
        m.publish(7).unwrap();
        assert_eq!(m.lifecycle(), CpuState::Prepared);
        assert!(m.publish(8).is_err());
        assert!(m.claim_entry(7).is_err());
        m.begin_start(7).unwrap();
        assert_eq!(m.lifecycle(), CpuState::Starting);
        assert!(!m.entry_is_claimed(7).unwrap()); // Release completion/entry is not implied.
        assert!(m.begin_start(7).is_err());
    }
    #[test]
    fn zero_stale_and_unsolicited_generations_are_rejected() {
        let m = published();
        for g in [0, 6, 8] {
            assert!(m.begin_start(g).is_err());
            assert!(m.claim_entry(g).is_err());
            assert_eq!(
                m.wait_until_parked(g, 0, 10, 1, || 0),
                Err(WaitError::InvalidGeneration)
            );
        }
        assert_eq!(m.lifecycle(), CpuState::Prepared);
    }
    #[test]
    fn one_claim_owns_diagnostics_and_only_real_initialization_sets_terminal_state() {
        let m = starting();
        let claim = m.claim_entry(7).unwrap();
        assert!(matches!(
            m.claim_entry(7),
            Err(ProtocolError::DuplicateEntry)
        ));
        assert_eq!(m.lifecycle(), CpuState::Starting);
        claim.park().unwrap();
        assert_eq!(m.lifecycle(), CpuState::Parked);
        assert!(m.claim_entry(7).is_err());
        assert!(m.begin_start(7).is_err());
        let m = starting();
        m.claim_entry(7).unwrap().fail().unwrap();
        assert_eq!(m.lifecycle(), CpuState::Failed);
        assert!(m.claim_entry(7).is_err());
        assert_eq!(
            m.wait_until_parked(7, 0, 10, 1, || 0),
            Err(WaitError::CpuFailed)
        );
    }
    #[test]
    fn timeout_and_repeated_waits_do_not_change_cpu_lifecycle() {
        let m = starting();
        assert_eq!(
            m.wait_until_parked(7, 0, 10, 1, || 10),
            Err(WaitError::Timeout)
        );
        assert_eq!(m.lifecycle(), CpuState::Starting);
        park(&m);
        assert_eq!(m.wait_until_parked(7, 0, 10, 1, || 0), Ok(()));
        assert_eq!(m.lifecycle(), CpuState::Parked);
        assert_eq!(
            m.wait_until_parked(7, 0, 10, 1, || 10),
            Err(WaitError::Timeout)
        );
        assert_eq!(m.lifecycle(), CpuState::Parked);
    }
    #[test]
    fn waiting_for_a_prepared_cpu_does_not_request_startup() {
        let m = published();
        assert_eq!(
            m.wait_until_parked(7, 0, 10, 1, || 10),
            Err(WaitError::Timeout)
        );
        assert_eq!(m.lifecycle(), CpuState::Prepared);
        assert!(!m.entry_is_claimed(7).unwrap());
        m.begin_start(7).unwrap();
        park(&m);
        assert_eq!(m.wait_until_parked(7, 0, 10, 1, || 0), Ok(()));
    }
    #[test]
    fn failure_after_timeout_is_genuine_failure_not_waiting_policy() {
        let m = starting();
        assert_eq!(
            m.wait_until_parked(7, 0, 10, 1, || 10),
            Err(WaitError::Timeout)
        );
        m.claim_entry(7).unwrap().fail().unwrap();
        assert_eq!(m.lifecycle(), CpuState::Failed);
        assert_eq!(
            m.wait_until_parked(7, 0, 10, 1, || 0),
            Err(WaitError::CpuFailed)
        );
    }
    #[test]
    fn invalid_deadline_and_poll_limit_leave_state_and_claim_unchanged() {
        let m = starting();
        for ticks in [0, 1 << 63, u64::MAX] {
            assert_eq!(
                m.wait_until_parked(7, 0, ticks, 1, || panic!("invalid deadline must not poll")),
                Err(WaitError::InvalidDeadline)
            );
        }
        let mut calls = 0;
        assert_eq!(
            m.wait_until_parked(7, 0, 10, 3, || {
                calls += 1;
                0
            }),
            Err(WaitError::PollLimit)
        );
        assert_eq!(calls, 3);
        assert_eq!(
            m.wait_until_parked(7, 0, 10, 0, || panic!("no polls")),
            Err(WaitError::PollLimit)
        );
        assert_eq!(m.lifecycle(), CpuState::Starting);
        assert!(!m.entry_is_claimed(7).unwrap());
    }
    #[test]
    fn counter_wrap_and_exact_park_deadline_remain_bounded() {
        let m = starting();
        let start = u64::MAX - 1;
        let mut now = start;
        assert_eq!(
            m.wait_until_parked(7, start, 4, 8, || {
                now = now.wrapping_add(1);
                now
            }),
            Err(WaitError::Timeout)
        );
        park(&m);
        let mut now = 8;
        assert_eq!(
            m.wait_until_parked(7, 0, 10, 1, || {
                now += 1;
                now
            }),
            Err(WaitError::Timeout)
        );
        assert_eq!(m.wait_until_parked(7, 0, 10, 1, || 9), Ok(()));
    }
    #[test]
    fn concurrent_entry_has_one_writer_and_dropped_claim_cannot_be_retried() {
        let m = starting();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                barrier.wait();
                m.claim_entry(7).is_ok()
            });
            let b = scope.spawn(|| {
                barrier.wait();
                m.claim_entry(7).is_ok()
            });
            assert_eq!(
                [a.join().unwrap(), b.join().unwrap()]
                    .iter()
                    .filter(|x| **x)
                    .count(),
                1
            );
        });
        assert_eq!(m.lifecycle(), CpuState::Starting);
        assert!(m.claim_entry(7).is_err());
    }
    #[test]
    fn full_width_generation_and_concurrent_waiters_are_independent() {
        let m = SecondaryMailbox::new();
        m.mark_prepared().unwrap();
        m.publish(u64::MAX).unwrap();
        m.begin_start(u64::MAX).unwrap();
        m.claim_entry(u64::MAX).unwrap().park().unwrap();
        assert!(m.claim_entry(u64::MAX - 1).is_err());
        std::thread::scope(|scope| {
            let a = scope.spawn(|| m.wait_until_parked(u64::MAX, 0, 10, 1, || 0));
            let b = scope.spawn(|| m.wait_until_parked(u64::MAX, 0, 10, 1, || 10));
            assert_eq!(a.join().unwrap(), Ok(()));
            assert_eq!(b.join().unwrap(), Err(WaitError::Timeout));
        });
        assert_eq!(m.lifecycle(), CpuState::Parked);
    }
    #[test]
    fn actual_management_failure_is_terminal_and_does_not_enable_retry() {
        let m = starting();
        park(&m);
        unsafe { m.management_failed(7) }.unwrap();
        assert_eq!(m.lifecycle(), CpuState::Failed);
        assert!(unsafe { m.management_online(7) }.is_err());
        assert!(m.begin_start(7).is_err());
        assert_eq!(
            m.wait_until_parked(7, 0, 10, 1, || 0),
            Err(WaitError::CpuFailed)
        );
    }
    #[test]
    fn management_readiness_and_responsive_park_are_owner_transitions() {
        let m = starting();
        assert_eq!(
            unsafe { m.management_online(7) },
            Err(ProtocolError::InvalidTransition)
        );
        park(&m);
        assert_eq!(
            unsafe { m.management_online(8) },
            Err(ProtocolError::InvalidGeneration)
        );
        unsafe { m.management_online(7) }.unwrap();
        assert_eq!(m.lifecycle(), CpuState::Online);
        assert_eq!(
            unsafe { m.management_online(7) },
            Err(ProtocolError::InvalidTransition)
        );
        unsafe { m.management_park(7) }.unwrap();
        assert_eq!(m.lifecycle(), CpuState::Parked);
        assert!(m.begin_start(7).is_err());
        assert_eq!(
            unsafe { m.management_park(7) },
            Err(ProtocolError::InvalidTransition)
        );
    }
    #[test]
    fn park_racing_timeout_is_observable_by_a_fresh_wait() {
        let m = starting();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                barrier.wait();
                park(&m)
            });
            assert_eq!(
                m.wait_until_parked(7, 0, 10, 1, || {
                    barrier.wait();
                    10
                }),
                Err(WaitError::Timeout)
            );
            worker.join().unwrap();
        });
        assert_eq!(m.lifecycle(), CpuState::Parked);
        assert_eq!(m.wait_until_parked(7, 0, 10, 1, || 0), Ok(()));
    }
}
