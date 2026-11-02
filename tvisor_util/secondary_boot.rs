//! Persistent aggregate boot policy, independent of optional mailbox observations.
use crate::{cpu_topology::MAX_CPUS, secondary_mailbox::WaitError, secondary_startup::StartupView};
use core::sync::atomic::{AtomicU64, Ordering};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum BootStatus {
    Idle,
    Preparing,
    Releasing,
    Ready,
    PreparationFailed,
    ReleaseFailed,
    WaitFailed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootError {
    AlreadyAttempted,
    InvalidTransition,
    InvalidCheckpoint,
    Fatal,
    Preparation(&'static str),
    Release(crate::startup_publication::PublicationError),
    Wait(WaitError),
    Readback,
}
impl core::fmt::Display for BootError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}
pub fn boot_status(state: &AtomicU64) -> BootStatus {
    match state.load(Ordering::Acquire) {
        0 => BootStatus::Idle,
        1 => BootStatus::Preparing,
        2 => BootStatus::Releasing,
        3 => BootStatus::Ready,
        4 => BootStatus::PreparationFailed,
        5 => BootStatus::ReleaseFailed,
        6 => BootStatus::WaitFailed,
        _ => unreachable!(),
    }
}
pub fn begin_boot_once(state: &AtomicU64) -> Result<(), BootError> {
    state
        .compare_exchange(
            BootStatus::Idle as u64,
            BootStatus::Preparing as u64,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map(|_| ())
        .map_err(|_| BootError::AlreadyAttempted)
}
pub fn begin_release(state: &AtomicU64) -> Result<(), BootError> {
    state
        .compare_exchange(
            BootStatus::Preparing as u64,
            BootStatus::Releasing as u64,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map(|_| ())
        .map_err(|_| BootError::InvalidTransition)
}
/// Explicitly finalize every result. Terminal status can never be overwritten,
/// including by late mailbox updates or a second successful observation.
pub fn finalize_boot(state: &AtomicU64, result: Result<(), BootError>) -> Result<(), BootError> {
    let old = boot_status(state);
    let next = match (old, result) {
        (BootStatus::Releasing, Ok(())) => BootStatus::Ready,
        (BootStatus::Preparing, _) => BootStatus::PreparationFailed,
        (
            BootStatus::Releasing,
            Err(
                BootError::Wait(_)
                | BootError::InvalidCheckpoint
                | BootError::Fatal
                | BootError::Readback,
            ),
        ) => BootStatus::WaitFailed,
        (BootStatus::Releasing, Err(_)) => BootStatus::ReleaseFailed,
        _ => return Err(BootError::InvalidTransition),
    };
    state
        .compare_exchange(old as u64, next as u64, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| BootError::InvalidTransition)?;
    if next == BootStatus::Ready {
        Ok(())
    } else {
        result.and(Err(BootError::InvalidTransition))
    }
}
pub fn verify_checkpoint(
    record: StartupView<'_>,
    id: usize,
    generation: u64,
) -> Result<(), BootError> {
    if id == 0
        || id >= MAX_CPUS
        || record.identity.logical_id != id as u64
        || record.identity.context != record.slot_address()
        || generation == 0
        || record.generation != generation
        || !record.mailbox.has_parked(generation)
        || record.failures.load(Ordering::Relaxed) != 0
    {
        return Err(BootError::InvalidCheckpoint);
    }
    Ok(())
}
/// Recheck the exact generation-bound set while the aggregate is still live.
/// The caller supplies views from the canonical slots, never a count of arrivals.
pub fn verify_all_parked<'a>(
    state: &AtomicU64,
    required: u64,
    generation: u64,
    records: impl IntoIterator<Item = (usize, StartupView<'a>)>,
    fatal: bool,
) -> Result<(), BootError> {
    if boot_status(state) != BootStatus::Releasing {
        return Err(BootError::InvalidTransition);
    }
    if fatal {
        return Err(BootError::Fatal);
    }
    if required == 0 || required & 1 != 0 || required >> MAX_CPUS != 0 {
        return Err(BootError::InvalidCheckpoint);
    }
    let mut seen = 0u64;
    for (id, record) in records {
        verify_checkpoint(record, id, generation)?;
        let bit = 1 << id;
        if required & bit == 0 || seen & bit != 0 {
            return Err(BootError::InvalidCheckpoint);
        }
        seen |= bit;
    }
    if seen != required {
        return Err(BootError::InvalidCheckpoint);
    }
    Ok(())
}
/// Observe within both a counter deadline and finite poll budget. Finalization
/// belongs to start_all, never to the mailbox's optional wait operation.
pub fn wait_for_parked(
    state: &AtomicU64,
    record: StartupView<'_>,
    id: usize,
    generation: u64,
    ticks: u64,
    max_polls: usize,
    mut elapsed: impl FnMut() -> u64,
    mut fatal: impl FnMut() -> bool,
) -> Result<(), BootError> {
    if boot_status(state) != BootStatus::Releasing {
        return Err(BootError::InvalidTransition);
    }
    if id == 0
        || id >= MAX_CPUS
        || record.identity.logical_id != id as u64
        || record.generation != generation
    {
        return Err(BootError::InvalidCheckpoint);
    }
    let mut saw_fatal = false;
    let result = record
        .mailbox
        .wait_until_parked(generation, 0, ticks, max_polls, || {
            if fatal() {
                saw_fatal = true;
                ticks
            } else {
                elapsed()
            }
        });
    if saw_fatal || fatal() {
        return Err(BootError::Fatal);
    }
    result.map_err(BootError::Wait)?;
    if boot_status(state) != BootStatus::Releasing {
        return Err(BootError::InvalidTransition);
    }
    verify_checkpoint(record, id, generation)
}
pub fn guest_authorized(state: &AtomicU64, fatal: bool) -> Result<(), BootError> {
    if fatal {
        Err(BootError::Fatal)
    } else if boot_status(state) == BootStatus::Ready {
        Ok(())
    } else {
        Err(BootError::InvalidTransition)
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
    fn state() -> AtomicU64 {
        AtomicU64::new(BootStatus::Idle as u64)
    }
    fn releasing(s: &AtomicU64) {
        begin_boot_once(s).unwrap();
        begin_release(s).unwrap();
    }
    fn parked(id: usize) -> std::boxed::Box<Fixture> {
        let f = Fixture::new(id, id as u64, 0x900000 + id as u64 * 0x20000, 0x800000, 7);
        let r = f.get(7).unwrap();
        r.mailbox.begin_start(7).unwrap();
        r.mailbox.claim_entry(7).unwrap().park().unwrap();
        f
    }
    #[test]
    fn readiness_requires_exact_complete_generation_bound_set() {
        for case in 0..8 {
            let s = state();
            releasing(&s);
            let a = parked(1);
            let b = parked(2);
            let c = parked(3);
            let mut records = std::vec![
                (1, a.get(7).unwrap()),
                (2, b.get(7).unwrap()),
                (3, c.get(7).unwrap())
            ];
            match case {
                0 => {
                    records.pop();
                }
                1 => records[1] = (2, a.get(7).unwrap()),
                2 => records.push((3, c.get(7).unwrap())),
                3 => records[0].1.generation = 8,
                4 => records[2].1.failures.store(1, Ordering::Relaxed),
                _ => {}
            }
            let result =
                verify_all_parked(&s, if case == 5 { 6 } else { 14 }, 7, records, case == 6);
            assert_eq!(result.is_ok(), case == 7);
            finalize_boot(&s, result).unwrap_or_else(|_| assert!(case != 7));
            assert_eq!(guest_authorized(&s, false).is_ok(), case == 7);
            assert!(begin_boot_once(&s).is_err());
        }
    }
    #[test]
    fn every_preparation_release_and_wait_failure_is_terminal() {
        for case in 0..3 {
            let s = state();
            begin_boot_once(&s).unwrap();
            if case > 0 {
                begin_release(&s).unwrap();
            }
            let error = [
                BootError::Preparation("fault"),
                BootError::Release(crate::startup_publication::PublicationError::Io),
                BootError::Wait(WaitError::Timeout),
            ][case];
            assert_eq!(finalize_boot(&s, Err(error)), Err(error));
            assert_eq!(
                boot_status(&s),
                [
                    BootStatus::PreparationFailed,
                    BootStatus::ReleaseFailed,
                    BootStatus::WaitFailed
                ][case]
            );
            assert_eq!(finalize_boot(&s, Ok(())), Err(BootError::InvalidTransition));
            assert!(begin_boot_once(&s).is_err());
            assert!(guest_authorized(&s, false).is_err());
        }
    }
    #[test]
    fn late_park_and_new_observation_cannot_reverse_a_middle_cpu_timeout() {
        let s = state();
        releasing(&s);
        let first = parked(1);
        let middle = Fixture::new(2, 2, 0x940000, 0x800000, 7);
        let r = middle.get(7).unwrap();
        r.mailbox.begin_start(7).unwrap();
        wait_for_parked(&s, first.get(7).unwrap(), 1, 7, 10, 1, || 0, || false).unwrap();
        let error = wait_for_parked(&s, r, 2, 7, 10, 1, || 10, || false);
        assert_eq!(error, Err(BootError::Wait(WaitError::Timeout)));
        finalize_boot(&s, error).unwrap_err();
        assert_eq!(r.mailbox.lifecycle(), CpuState::Starting);
        r.mailbox.claim_entry(7).unwrap().park().unwrap();
        r.mailbox.wait_until_parked(7, 0, 10, 1, || 0).unwrap();
        assert_eq!(
            wait_for_parked(&s, r, 2, 7, 10, 1, || 0, || false),
            Err(BootError::InvalidTransition)
        );
        assert!(verify_all_parked(&s, 6, 7, [(1, first.get(7).unwrap()), (2, r)], false).is_err());
        assert_eq!(boot_status(&s), BootStatus::WaitFailed);
        assert!(guest_authorized(&s, false).is_err());
        assert!(begin_boot_once(&s).is_err());
    }
    #[test]
    fn bad_checkpoint_poll_deadline_local_and_global_failure_are_rejected() {
        for case in 0..7 {
            let s = state();
            releasing(&s);
            let f = Fixture::new(1, 1, 0x900000, 0x800000, 7);
            let r = f.get(7).unwrap();
            r.mailbox.begin_start(7).unwrap();
            if case == 0 {
                r.mailbox.claim_entry(7).unwrap().fail().unwrap();
            }
            if case == 4 {
                r.mailbox.claim_entry(7).unwrap().park().unwrap();
                r.failures.store(1, Ordering::Relaxed);
            }
            let result = wait_for_parked(
                &s,
                r,
                if case == 5 { 2 } else { 1 },
                if case == 6 { 8 } else { 7 },
                if case == 1 { 0 } else { 10 },
                1,
                || 0,
                || case == 3,
            );
            assert!(result.is_err());
            finalize_boot(&s, result).unwrap_err();
            assert!(guest_authorized(&s, false).is_err());
        }
    }
    #[test]
    fn fatal_after_ready_still_blocks_guest_and_preparing_cannot_finalize_success() {
        let s = state();
        releasing(&s);
        let f = parked(1);
        verify_all_parked(&s, 2, 7, [(1, f.get(7).unwrap())], false).unwrap();
        finalize_boot(&s, Ok(())).unwrap();
        assert_eq!(guest_authorized(&s, true), Err(BootError::Fatal));
        let other = state();
        begin_boot_once(&other).unwrap();
        assert!(finalize_boot(&other, Ok(())).is_err());
        assert_eq!(boot_status(&other), BootStatus::PreparationFailed);
    }
    #[test]
    fn invalid_masks_and_unexpected_boot_cpu_never_authorize() {
        for mask in [0, 1, 3, 1 << MAX_CPUS] {
            let s = state();
            releasing(&s);
            assert!(verify_all_parked(&s, mask, 7, [], false).is_err());
        }
        let s = state();
        releasing(&s);
        let f = parked(1);
        let mut view = f.get(7).unwrap();
        view.identity.logical_id = 0;
        assert!(verify_all_parked(&s, 2, 7, [(0, view)], false).is_err());
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
