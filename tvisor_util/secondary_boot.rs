//! One-shot boot gate and validated physical-counter deadline.
use crate::{secondary_mailbox::WaitError, secondary_startup::StartupView};
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
    parked_observation: Option<(u64, u64)>,
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
        Ok(Attempt {
            boot: self,
            parked_observation: None,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootWaitError {
    PolicyTerminated,
    Wait(WaitError),
}
impl Attempt<'_> {
    pub fn release_attempted(&mut self) -> Result<(), BootError> {
        self.boot
            .state
            .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| BootError::InvalidTransition)
    }
    /// Boot policy wraps the optional pure readiness wait and permanently records
    /// failure. A successful observation is bound to this exact slot/generation.
    pub fn wait_for_parked<F: FnMut() -> u64>(
        &mut self,
        startup: StartupView<'_>,
        start: u64,
        ticks: u64,
        max_polls: usize,
        counter: F,
    ) -> Result<(), BootWaitError> {
        if self.boot.status() != BootStatus::ReleaseAttempted {
            return Err(BootWaitError::PolicyTerminated);
        }
        match startup.mailbox.wait_until_parked(
            startup.generation,
            start,
            ticks,
            max_polls,
            counter,
        ) {
            Ok(()) => {
                self.parked_observation = Some((startup.slot_address(), startup.generation));
                Ok(())
            }
            Err(error) => {
                self.wait_failed();
                Err(BootWaitError::Wait(error))
            }
        }
    }
    pub fn wait_failed(&mut self) {
        self.parked_observation = None;
        let _ = self
            .boot
            .state
            .compare_exchange(2, 6, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn complete(
        self,
        record: StartupView<'_>,
        generation: u64,
        fatal: bool,
    ) -> Result<GuestPermit, BootError> {
        if self.boot.status() != BootStatus::ReleaseAttempted {
            return Err(BootError::InvalidTransition);
        }
        if fatal
            || record.generation != generation
            || self.parked_observation != Some((record.slot_address(), generation))
            || !record.mailbox.has_parked(generation)
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
    use crate::{secondary_mailbox::CpuState, secondary_startup::test_fixture::Fixture};
    fn record() -> std::boxed::Box<Fixture> {
        Fixture::new(1, 1, 0x800000, 0x430000, 7)
    }
    fn park(s: &Fixture) {
        let r = s.get(7).unwrap();
        r.mailbox.begin_start(7).unwrap();
        r.mailbox.claim_entry(7).unwrap().park().unwrap();
    }
    #[test]
    fn guest_runs_once_only_after_a_matching_successful_boot_wait() {
        let gate = SecondaryBoot::new();
        let s = record();
        let mut attempt = gate.begin().unwrap();
        assert!(gate.begin().is_err());
        attempt.release_attempted().unwrap();
        park(&s);
        attempt
            .wait_for_parked(s.get(7).unwrap(), 0, 10, 1, || 0)
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
                let mut a = gate.begin().unwrap();
                if phase > 0 {
                    a.release_attempted().unwrap();
                }
                if phase == 2 {
                    a.wait_failed();
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
    fn late_park_and_successful_new_wait_never_reverse_failed_boot_policy() {
        let gate = SecondaryBoot::new();
        let s = record();
        let r = s.get(7).unwrap();
        let mut a = gate.begin().unwrap();
        a.release_attempted().unwrap();
        r.mailbox.begin_start(7).unwrap();
        assert_eq!(
            a.wait_for_parked(r, 0, 10, 1, || 10),
            Err(BootWaitError::Wait(WaitError::Timeout))
        );
        assert_eq!(r.mailbox.lifecycle(), CpuState::Starting);
        r.mailbox.claim_entry(7).unwrap().park().unwrap();
        assert_eq!(r.mailbox.wait_until_parked(7, 0, 10, 1, || 0), Ok(()));
        assert_eq!(
            a.wait_for_parked(r, 0, 10, 1, || 0),
            Err(BootWaitError::PolicyTerminated)
        );
        assert!(a.complete(r, 7, false).is_err());
        assert_eq!(gate.status(), BootStatus::WaitFailed);
        assert!(gate.begin().is_err());
        assert_eq!(r.mailbox.lifecycle(), CpuState::Parked);
    }
    #[test]
    fn no_permit_without_wait_or_with_stale_wrong_slot_fatal_or_bad_checkpoint() {
        for case in 0..6 {
            let gate = SecondaryBoot::new();
            let s = record();
            let mut a = gate.begin().unwrap();
            a.release_attempted().unwrap();
            park(&s);
            let r = s.get(7).unwrap();
            if case != 0 {
                a.wait_for_parked(r, 0, 10, 1, || 0).unwrap();
            }
            if case == 3 {
                r.failures.store(1, Ordering::Relaxed);
            }
            let other = record();
            if case == 4 {
                park(&other);
            }
            if case == 5 {
                a.wait_failed();
            }
            assert!(
                a.complete(
                    if case == 4 { other.get(7).unwrap() } else { r },
                    if case == 1 { 8 } else { 7 },
                    case == 2
                )
                .is_err()
            );
            assert_eq!(gate.status(), BootStatus::WaitFailed);
            assert!(gate.begin().is_err());
        }
    }
    #[test]
    fn release_policy_failure_is_not_cpu_failure_and_survives_late_parking() {
        let gate = SecondaryBoot::new();
        let s = record();
        let r = s.get(7).unwrap();
        {
            let mut a = gate.begin().unwrap();
            a.release_attempted().unwrap();
            r.mailbox.begin_start(7).unwrap();
        }
        assert_eq!(gate.status(), BootStatus::ReleaseFailed);
        assert_eq!(r.mailbox.lifecycle(), CpuState::Starting);
        r.mailbox.claim_entry(7).unwrap().park().unwrap();
        assert_eq!(r.mailbox.wait_until_parked(7, 0, 10, 1, || 0), Ok(()));
        assert_eq!(gate.status(), BootStatus::ReleaseFailed);
        assert!(gate.begin().is_err());
    }
    #[test]
    fn genuine_failure_and_wait_bounds_fail_boot_without_overwriting_lifecycle() {
        for case in 0..3 {
            let gate = SecondaryBoot::new();
            let s = record();
            let r = s.get(7).unwrap();
            let mut a = gate.begin().unwrap();
            a.release_attempted().unwrap();
            r.mailbox.begin_start(7).unwrap();
            if case == 0 {
                r.mailbox.claim_entry(7).unwrap().fail().unwrap();
            }
            let result = a.wait_for_parked(r, 0, if case == 1 { 0 } else { 10 }, 1, || 0);
            assert_eq!(
                result,
                Err(BootWaitError::Wait(
                    [
                        WaitError::CpuFailed,
                        WaitError::InvalidDeadline,
                        WaitError::PollLimit
                    ][case]
                ))
            );
            assert_eq!(
                r.mailbox.lifecycle(),
                if case == 0 {
                    CpuState::Failed
                } else {
                    CpuState::Starting
                }
            );
            assert_eq!(gate.status(), BootStatus::WaitFailed);
            assert!(a.complete(r, 7, false).is_err());
        }
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
