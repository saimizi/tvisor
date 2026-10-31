//! One-shot boot gate and validated physical-counter deadline.
use crate::{secondary_mailbox::Outcome, secondary_record::SecondaryRecord};
use core::sync::atomic::{AtomicU64, Ordering};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum BootStatus {
    Idle = 0,
    Preparing = 1,
    ReleaseAttempted = 2,
    Success = 3,
    PreparationFailed = 4,
    ReleaseFailed = 5,
    WaitFailed = 6,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootError {
    AlreadyAttempted,
    InvalidTransition,
    InvalidCheckpoint,
}
pub struct SecondaryBoot {
    state: AtomicU64,
}
pub struct Attempt<'a> {
    boot: &'a SecondaryBoot,
}
/// Only a matching validated Parked checkpoint can create this consuming permit.
pub struct GuestPermit {
    _private: (),
}
impl GuestPermit {
    pub fn run<T>(self, guest: impl FnOnce() -> T) -> T {
        guest()
    }
}
impl SecondaryBoot {
    pub const fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
        }
    }
    pub fn status(&self) -> BootStatus {
        match self.state.load(Ordering::Acquire) {
            0 => BootStatus::Idle,
            1 => BootStatus::Preparing,
            2 => BootStatus::ReleaseAttempted,
            3 => BootStatus::Success,
            4 => BootStatus::PreparationFailed,
            5 => BootStatus::ReleaseFailed,
            6 => BootStatus::WaitFailed,
            _ => unreachable!(),
        }
    }
    pub fn begin(&self) -> Result<Attempt<'_>, BootError> {
        self.state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| BootError::AlreadyAttempted)?;
        Ok(Attempt { boot: self })
    }
}
impl Attempt<'_> {
    pub fn release_attempted(&mut self) -> Result<(), BootError> {
        self.boot
            .state
            .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| BootError::InvalidTransition)
    }
    pub fn wait_failed(&mut self) {
        let _ = self
            .boot
            .state
            .compare_exchange(2, 6, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn complete(
        self,
        record: &SecondaryRecord,
        generation: u64,
        fatal: bool,
    ) -> Result<GuestPermit, BootError> {
        if self.boot.status() != BootStatus::ReleaseAttempted {
            return Err(BootError::InvalidTransition);
        }
        if fatal
            || record.generation != generation
            || !record.mailbox.has_arrived(generation)
            || !record.mailbox.has_parked(generation)
            || record.mailbox.outcome() != Outcome::Success
            || record.failures.load(Ordering::Relaxed) != 0
        {
            let _ = self
                .boot
                .state
                .compare_exchange(2, 6, Ordering::AcqRel, Ordering::Acquire);
            return Err(BootError::InvalidCheckpoint);
        }
        self.boot
            .state
            .compare_exchange(2, 3, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| BootError::InvalidTransition)?;
        Ok(GuestPermit { _private: () })
    }
}
impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        // Any early return/unwind is terminal; successful completion is untouched.
        let _ = self
            .boot
            .state
            .compare_exchange(1, 4, Ordering::AcqRel, Ordering::Acquire);
        let _ = self
            .boot
            .state
            .compare_exchange(2, 5, Ordering::AcqRel, Ordering::Acquire);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartupDeadline {
    ticks: u64,
}
impl StartupDeadline {
    /// CNTFRQ is a 32-bit architectural frequency. Use checked integer conversion.
    pub fn new(frequency: u64, seconds: u64) -> Option<Self> {
        if frequency == 0 || frequency > u32::MAX as u64 || seconds == 0 {
            return None;
        }
        let ticks = frequency.checked_mul(seconds)?;
        if ticks >= 1 << 55 {
            return None;
        }
        Some(Self { ticks })
    }
    pub const fn ticks(self) -> u64 {
        self.ticks
    }
    /// Low 56 bits work for every architecturally permitted 56–64-bit counter.
    pub fn elapsed(self, start: u64, now: u64) -> u64 {
        now.wrapping_sub(start) & ((1u64 << 56) - 1)
    }
    pub fn counter_progresses(start: u64, now: u64) -> bool {
        let elapsed = now.wrapping_sub(start) & ((1u64 << 56) - 1);
        elapsed != 0 && elapsed < 1 << 55
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        el2_policy::HostTranslation,
        secondary_record::{RecordStorage, StartupRecord},
    };
    fn record() -> RecordStorage {
        let s = RecordStorage::new();
        let p = HostTranslation::new(0x430000, 4).unwrap();
        s.publish(StartupRecord {
            logical_id: 1,
            affinity: 1,
            stack_bottom: 0x800000,
            stack_top: 0x810000,
            context: 0x420000,
            mair: p.mair(),
            tcr: p.tcr(),
            root: p.root(),
            sctlr: p.sctlr(),
            generation: 7,
        })
        .unwrap();
        s
    }
    fn park(s: &RecordStorage) {
        let m = &s.get(7).unwrap().mailbox;
        m.enter(7).unwrap();
        m.acknowledge(7).unwrap();
        m.park(7).unwrap();
    }
    #[test]
    fn guest_runs_once_only_after_validated_success_and_boot_cannot_restart() {
        let gate = SecondaryBoot::new();
        let s = record();
        let mut attempt = gate.begin().unwrap();
        assert!(gate.begin().is_err());
        attempt.release_attempted().unwrap();
        park(&s);
        s.get(7)
            .unwrap()
            .mailbox
            .finish(7, Outcome::Success)
            .unwrap();
        let permit = attempt.complete(s.get(7).unwrap(), 7, false).unwrap();
        let mut calls = 0;
        permit.run(|| calls += 1);
        assert_eq!(calls, 1);
        assert_eq!(gate.status(), BootStatus::Success);
        assert!(gate.begin().is_err());
    }
    #[test]
    fn every_preparation_release_and_wait_failure_is_terminal() {
        for phase in 0..3 {
            let gate = SecondaryBoot::new();
            {
                let mut attempt = gate.begin().unwrap();
                if phase > 0 {
                    attempt.release_attempted().unwrap();
                }
                if phase == 2 {
                    attempt.wait_failed();
                }
            }
            assert_eq!(
                gate.status(),
                [
                    BootStatus::PreparationFailed,
                    BootStatus::ReleaseFailed,
                    BootStatus::WaitFailed
                ][phase]
            );
            assert!(gate.begin().is_err());
        }
    }
    #[test]
    fn no_permit_for_pending_arrival_failure_timeout_stale_generation_fatal_or_bad_checkpoint() {
        for case in 0..7 {
            let gate = SecondaryBoot::new();
            let s = record();
            let mut attempt = gate.begin().unwrap();
            attempt.release_attempted().unwrap();
            let r = s.get(7).unwrap();
            match case {
                0 => {}
                1 => {
                    r.mailbox.enter(7).unwrap();
                    r.mailbox.acknowledge(7).unwrap();
                }
                2 => {
                    r.mailbox.enter(7).unwrap();
                    r.mailbox.fail(7).unwrap();
                    r.mailbox.finish(7, Outcome::SecondaryFailed).unwrap();
                }
                3 => {
                    r.mailbox.finish(7, Outcome::TimedOut).unwrap();
                    park(&s);
                }
                _ => {
                    park(&s);
                    r.mailbox.finish(7, Outcome::Success).unwrap();
                }
            }
            if case == 6 {
                r.failures.store(1, Ordering::Relaxed);
            }
            assert!(
                attempt
                    .complete(r, if case == 4 { 8 } else { 7 }, case == 5)
                    .is_err()
            );
            assert_eq!(gate.status(), BootStatus::WaitFailed);
            assert!(gate.begin().is_err());
        }
    }
    #[test]
    fn boot_abort_preserves_first_mailbox_outcome_despite_late_worker() {
        let s = record();
        let r = s.get(7).unwrap();
        r.mailbox.finish(7, Outcome::BootAborted).unwrap();
        park(&s);
        assert!(r.mailbox.finish(7, Outcome::Success).is_err());
        assert_eq!(r.mailbox.outcome(), Outcome::BootAborted);
    }
    #[test]
    fn validates_deadline_conversion_and_counter_wrap_progress() {
        assert_eq!(
            StartupDeadline::new(54_000_000, 2).unwrap().ticks(),
            108_000_000
        );
        for (f, s) in [
            (0, 2),
            (1, 0),
            (u32::MAX as u64 + 1, 2),
            (1, 1 << 55),
            (1, 1 << 63),
            (u32::MAX as u64, u64::MAX),
        ] {
            assert!(StartupDeadline::new(f, s).is_none());
        }
        assert!(!StartupDeadline::counter_progresses(10, 10));
        assert!(!StartupDeadline::counter_progresses(10, 9));
        assert!(StartupDeadline::counter_progresses(u64::MAX, 0));
        let d = StartupDeadline::new(54_000_000, 2).unwrap();
        assert_eq!(d.elapsed((1 << 56) - 2, 1), 3);
        assert_eq!(d.elapsed(u64::MAX - 1, 1), 3);
    }
}
