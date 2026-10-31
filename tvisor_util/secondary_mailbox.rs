//! One-shot startup protocol. Requires coherent memory before atomic access.
use core::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    InvalidGeneration,
    InvalidTransition,
    AlreadyFinished,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum WorkerState {
    Ready = 0,
    Entered = 1,
    Arrived = 2,
    Parked = 3,
    Failed = 4,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Outcome {
    Pending = 0,
    Success = 1,
    TimedOut = 2,
    SecondaryFailed = 3,
    PollLimit = 4,
    InvalidDeadline = 5,
    BootAborted = 6,
}

/// ready is immutable after publication. The single state word serializes all
/// worker transitions; outcome is exclusively decided by the boot CPU.
#[repr(C)]
pub struct SecondaryMailbox {
    ready: AtomicU64,
    state: AtomicU64,
    outcome: AtomicU64,
}
const _: () = {
    assert!(core::mem::offset_of!(SecondaryMailbox, ready) == 0);
    assert!(core::mem::offset_of!(SecondaryMailbox, state) == 8);
    assert!(core::mem::offset_of!(SecondaryMailbox, outcome) == 16);
    assert!(core::mem::size_of::<SecondaryMailbox>() == 24);
};
impl SecondaryMailbox {
    pub const fn new() -> Self {
        Self {
            ready: AtomicU64::new(0),
            state: AtomicU64::new(0),
            outcome: AtomicU64::new(0),
        }
    }
    pub fn publish(&self, generation: u64) -> Result<(), ProtocolError> {
        if generation == 0 {
            return Err(ProtocolError::InvalidGeneration);
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
    pub fn state(&self, generation: u64) -> Result<WorkerState, ProtocolError> {
        self.check(generation)?;
        Ok(match self.state.load(Ordering::Acquire) {
            0 => WorkerState::Ready,
            1 => WorkerState::Entered,
            2 => WorkerState::Arrived,
            3 => WorkerState::Parked,
            4 => WorkerState::Failed,
            _ => unreachable!(),
        })
    }
    fn transition(
        &self,
        generation: u64,
        from: WorkerState,
        to: WorkerState,
    ) -> Result<(), ProtocolError> {
        self.check(generation)?;
        self.state
            .compare_exchange(from as u64, to as u64, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| ProtocolError::InvalidTransition)
    }
    /// Claim entry before writing checkpoint fields. Duplicate entrants must
    /// park without changing the first entrant's evidence.
    pub fn enter(&self, generation: u64) -> Result<(), ProtocolError> {
        self.transition(generation, WorkerState::Ready, WorkerState::Entered)
    }
    pub fn acknowledge(&self, generation: u64) -> Result<(), ProtocolError> {
        self.transition(generation, WorkerState::Entered, WorkerState::Arrived)
    }
    pub fn park(&self, generation: u64) -> Result<(), ProtocolError> {
        self.transition(generation, WorkerState::Arrived, WorkerState::Parked)
    }
    /// Local validation failure is published after writing diagnostic fields.
    pub fn fail(&self, generation: u64) -> Result<(), ProtocolError> {
        self.transition(generation, WorkerState::Entered, WorkerState::Failed)
    }
    pub fn has_arrived(&self, generation: u64) -> bool {
        matches!(
            self.state(generation),
            Ok(WorkerState::Arrived | WorkerState::Parked)
        )
    }
    pub fn has_parked(&self, generation: u64) -> bool {
        self.state(generation) == Ok(WorkerState::Parked)
    }
    pub fn outcome(&self) -> Outcome {
        match self.outcome.load(Ordering::Acquire) {
            0 => Outcome::Pending,
            1 => Outcome::Success,
            2 => Outcome::TimedOut,
            3 => Outcome::SecondaryFailed,
            4 => Outcome::PollLimit,
            5 => Outcome::InvalidDeadline,
            6 => Outcome::BootAborted,
            _ => unreachable!(),
        }
    }
    pub fn finish(&self, generation: u64, outcome: Outcome) -> Result<(), ProtocolError> {
        self.check(generation)?;
        if outcome == Outcome::Pending
            || (outcome == Outcome::Success && !self.has_parked(generation))
            || (outcome == Outcome::SecondaryFailed
                && self.state(generation)? != WorkerState::Failed)
        {
            return Err(ProtocolError::InvalidTransition);
        }
        self.outcome
            .compare_exchange(0, outcome as u64, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| ProtocolError::AlreadyFinished)
    }
    /// Boot-owned measured wait. Outcome is finalized here, including on the
    /// finite poll fallback. Late worker evidence never changes that outcome.
    pub fn wait_and_finish<F: FnMut() -> u64>(
        &self,
        generation: u64,
        start: u64,
        ticks: u64,
        max_polls: usize,
        mut counter: F,
    ) -> Result<Outcome, ProtocolError> {
        self.check(generation)?;
        if self.outcome() != Outcome::Pending {
            return Err(ProtocolError::AlreadyFinished);
        }
        let mut result = Outcome::PollLimit;
        if ticks == 0 || ticks >= (1u64 << 63) {
            result = Outcome::InvalidDeadline;
        } else {
            for _ in 0..max_polls {
                if counter().wrapping_sub(start) >= ticks {
                    result = Outcome::TimedOut;
                    break;
                }
                let state = self.state(generation)?;
                if state == WorkerState::Failed {
                    result = Outcome::SecondaryFailed;
                    break;
                }
                if state == WorkerState::Parked {
                    // Bracket the acquired checkpoint with clock observations.
                    result = if counter().wrapping_sub(start) >= ticks {
                        Outcome::TimedOut
                    } else {
                        Outcome::Success
                    };
                    break;
                }
                core::hint::spin_loop();
            }
        }
        self.finish(generation, result)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn published() -> SecondaryMailbox {
        let m = SecondaryMailbox::new();
        m.publish(7).unwrap();
        m
    }
    fn parked(m: &SecondaryMailbox) {
        m.enter(7).unwrap();
        m.acknowledge(7).unwrap();
        m.park(7).unwrap();
    }
    #[test]
    fn rejects_zero_stale_and_unsolicited_generations() {
        let m = SecondaryMailbox::new();
        assert!(m.publish(0).is_err());
        assert!(m.enter(7).is_err());
        m.publish(7).unwrap();
        assert!(m.publish(8).is_err());
        for g in [0, 6, 8] {
            assert!(m.enter(g).is_err());
            assert!(m.acknowledge(g).is_err());
            assert!(m.park(g).is_err());
            assert!(m.fail(g).is_err());
            assert!(m.finish(g, Outcome::TimedOut).is_err());
        }
        assert_eq!(m.state(7), Ok(WorkerState::Ready));
        assert_eq!(m.outcome(), Outcome::Pending);
    }
    #[test]
    fn rejects_skipped_duplicate_and_conflicting_transitions() {
        let m = published();
        assert!(m.acknowledge(7).is_err());
        assert!(m.park(7).is_err());
        assert!(m.fail(7).is_err());
        m.enter(7).unwrap();
        assert!(m.enter(7).is_err());
        m.acknowledge(7).unwrap();
        assert!(m.acknowledge(7).is_err());
        assert!(m.fail(7).is_err());
        m.park(7).unwrap();
        assert!(m.park(7).is_err());
        assert!(m.fail(7).is_err());
        assert!(m.has_arrived(7));
        assert!(m.has_parked(7));
    }
    #[test]
    fn failed_validation_cannot_acknowledge_or_park() {
        let m = published();
        m.enter(7).unwrap();
        m.fail(7).unwrap();
        assert!(m.fail(7).is_err());
        assert!(m.acknowledge(7).is_err());
        assert!(m.park(7).is_err());
        assert!(!m.has_arrived(7));
        assert!(!m.has_parked(7));
        assert_eq!(
            m.wait_and_finish(7, 0, 10, 1, || 0),
            Ok(Outcome::SecondaryFailed)
        );
    }
    #[test]
    fn deadline_is_terminal_despite_late_entry_and_park() {
        let m = published();
        assert_eq!(m.wait_and_finish(7, 0, 10, 1, || 10), Ok(Outcome::TimedOut));
        parked(&m);
        assert!(m.finish(7, Outcome::Success).is_err());
        assert!(m.wait_and_finish(7, 0, 10, 1, || 0).is_err());
        assert_eq!(m.outcome(), Outcome::TimedOut);
    }
    #[test]
    fn failure_after_timeout_preserves_boot_result() {
        let m = published();
        m.finish(7, Outcome::TimedOut).unwrap();
        m.enter(7).unwrap();
        m.fail(7).unwrap();
        assert!(m.finish(7, Outcome::SecondaryFailed).is_err());
        assert_eq!(m.outcome(), Outcome::TimedOut);
        assert_eq!(m.state(7), Ok(WorkerState::Failed));
    }
    #[test]
    fn invalid_deadlines_and_poll_exhaustion_are_distinct_terminal_results() {
        for ticks in [0, 1 << 63, u64::MAX] {
            let m = published();
            assert_eq!(
                m.wait_and_finish(7, 0, ticks, 1, || 0),
                Ok(Outcome::InvalidDeadline)
            );
        }
        let m = published();
        let mut calls = 0;
        assert_eq!(
            m.wait_and_finish(7, 0, 10, 3, || {
                calls += 1;
                0
            }),
            Ok(Outcome::PollLimit)
        );
        assert_eq!(calls, 3);
        let m = published();
        assert_eq!(
            m.wait_and_finish(7, 0, 10, 0, || panic!("no polls")),
            Ok(Outcome::PollLimit)
        );
    }
    #[test]
    fn deadline_wraparound_and_park_observation_boundary() {
        let m = published();
        let start = u64::MAX - 1;
        let mut now = start;
        assert_eq!(
            m.wait_and_finish(7, start, 4, 8, || {
                now = now.wrapping_add(1);
                now
            }),
            Ok(Outcome::TimedOut)
        );
        let m = published();
        parked(&m);
        let mut now = 8;
        assert_eq!(
            m.wait_and_finish(7, 0, 10, 1, || {
                now += 1;
                now
            }),
            Ok(Outcome::TimedOut)
        );
        let m = published();
        parked(&m);
        assert_eq!(m.wait_and_finish(7, 0, 10, 1, || 9), Ok(Outcome::Success));
    }
    #[test]
    fn only_validated_park_can_succeed_and_first_terminal_result_wins() {
        let m = published();
        assert!(m.finish(7, Outcome::Pending).is_err());
        assert!(m.finish(7, Outcome::Success).is_err());
        assert!(m.finish(7, Outcome::SecondaryFailed).is_err());
        parked(&m);
        m.finish(7, Outcome::Success).unwrap();
        assert!(m.finish(7, Outcome::TimedOut).is_err());
        assert_eq!(m.outcome(), Outcome::Success);
    }
    #[test]
    fn concurrent_terminal_decisions_have_exactly_one_winner() {
        let m = published();
        parked(&m);
        let gate = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                gate.wait();
                m.finish(7, Outcome::Success)
            });
            let b = scope.spawn(|| {
                gate.wait();
                m.finish(7, Outcome::TimedOut)
            });
            let results = [a.join().unwrap(), b.join().unwrap()];
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        });
        assert!(matches!(m.outcome(), Outcome::Success | Outcome::TimedOut));
    }
    #[test]
    fn full_width_generation_is_not_truncated() {
        let m = SecondaryMailbox::new();
        m.publish(u64::MAX).unwrap();
        assert!(m.enter(u64::MAX - 1).is_err());
        m.enter(u64::MAX).unwrap();
        m.acknowledge(u64::MAX).unwrap();
        m.park(u64::MAX).unwrap();
        assert_eq!(
            m.wait_and_finish(u64::MAX, 0, 10, 1, || 0),
            Ok(Outcome::Success)
        );
    }
    #[test]
    fn worker_park_racing_deadline_cannot_override_timeout() {
        let m = published();
        let gate = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                gate.wait();
                parked(&m);
            });
            assert_eq!(
                m.wait_and_finish(7, 0, 10, 1, || {
                    gate.wait();
                    10
                }),
                Ok(Outcome::TimedOut)
            );
            worker.join().unwrap();
        });
        assert!(m.has_parked(7));
        assert_eq!(m.outcome(), Outcome::TimedOut);
    }
}
