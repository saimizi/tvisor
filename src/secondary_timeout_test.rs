//! Opt-in hardware acceptance instrumentation, absent from normal builds.
//! Delay only after coherent entry, local validation and checkpoint writes.
//! Never change mappings, registers, release routing or publication maintenance.
use super::{BOOT_STATE, physical_counter, report_checkpoint, startup};
use core::sync::atomic::Ordering;
use tvisor_util::{
    println,
    secondary_boot::{BootError, BootStatus, StartupDeadline},
    secondary_mailbox::CpuState,
};

#[cfg(feature = "all-core-timeout-test")]
pub(super) const TEST_CPU: usize = 2;
#[cfg(not(feature = "all-core-timeout-test"))]
pub(super) const TEST_CPU: usize = 1;

const MAX_POLLS: usize = 250_000_000;

fn make_deadline(seconds: u64) -> Option<StartupDeadline> {
    let frequency: u64;
    unsafe {
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) frequency,
            options(nomem, nostack, preserves_flags));
    }
    StartupDeadline::new(frequency, seconds)
}

pub(super) fn delay_parking() {
    if let Some(deadline) = make_deadline(4) {
        let start = physical_counter();
        for _ in 0..MAX_POLLS {
            if deadline.elapsed(start, physical_counter()) >= deadline.ticks() {
                return;
            }
            core::hint::spin_loop();
        }
    }
    // A broken clock/poll fallback must not produce early parking.
    // CPU0 still has its unchanged finite startup wait and bounded observer.
    loop {
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}

/// Observe a real late arrival without retrying release or minting a permit.
/// Invoked after the production boot-policy attempt has finalized failure.
pub(super) fn observe_terminal_failure() {
    let Some(record) = startup(TEST_CPU, 1) else {
        println!("Phase 11.3 timeout-test FAIL: no retained record");
        return;
    };
    if super::boot_status() != BootStatus::WaitFailed
        || record.mailbox.state(1) != Ok(CpuState::Starting)
        || record.mailbox.entry_is_claimed(1) != Ok(true)
    {
        println!("Phase 11.3 timeout-test FAIL: expected terminal timeout");
        return;
    }
    println!(
        "Phase 11.3 timeout-test before late park: boot={:?} state={:?} entry_claimed={:?}",
        super::boot_status(),
        record.mailbox.state(1),
        record.mailbox.entry_is_claimed(1)
    );
    let Some(deadline) = make_deadline(5) else {
        println!("Phase 11.3 timeout-test FAIL: invalid observer deadline");
        return;
    };
    let start = physical_counter();
    for _ in 0..MAX_POLLS {
        if record.mailbox.state(1) == Ok(CpuState::Parked)
            || deadline.elapsed(start, physical_counter()) >= deadline.ticks()
        {
            break;
        }
        core::hint::spin_loop();
    }
    if record.mailbox.state(1) != Ok(CpuState::Parked) {
        println!("Phase 11.3 timeout-test FAIL: no late parked checkpoint");
        return;
    }
    report_checkpoint(record);
    // Optional waits may be retried; successful readiness must not undo the
    // independent terminal boot-policy decision or create a new guest permit.
    let Some(late_deadline) = make_deadline(1) else {
        println!("Phase 11.3 timeout-test FAIL: invalid late-wait deadline");
        return;
    };
    let late_start = physical_counter();
    let late_wait_succeeded = record
        .mailbox
        .wait_until_parked(1, 0, late_deadline.ticks(), 1, || {
            late_deadline.elapsed(late_start, physical_counter())
        })
        .is_ok();
    // This probes only the one-shot gate; it never calls start_all/release again.
    let retry_rejected = matches!(
        tvisor_util::secondary_boot::begin_boot_once(&BOOT_STATE),
        Err(BootError::AlreadyAttempted)
    );
    let retained = startup(TEST_CPU, 1)
        .is_some_and(|again| record.slot_address() == again.slot_address())
        && crate::pcpu::validate_secondary_context(
            record.identity.logical_id,
            record.identity.affinity,
            record.identity.stack_bottom,
            record.identity.stack_top,
            record.checkpoint.tpidr.load(Ordering::Relaxed),
        );
    unsafe extern "C" {
        fn __secondary_entry();
    }
    let cells_unchanged = crate::smp::verify_release_set(
        super::RELEASE_ATTEMPTED.load(Ordering::Acquire),
        __secondary_entry as *const () as u64,
    )
    .is_ok();
    #[cfg(feature = "all-core-timeout-test")]
    let group_preserved = startup(1, 1).is_some_and(|s| s.mailbox.state(1) == Ok(CpuState::Parked))
        && startup(3, 1).is_some_and(|s| s.mailbox.state(1) == Ok(CpuState::Prepared))
        && super::RELEASE_ATTEMPTED.load(Ordering::Acquire) == 6;
    #[cfg(not(feature = "all-core-timeout-test"))]
    let group_preserved = true;
    println!(
        "Phase 11.5 timeout-test CPU{} group_preserved={}",
        TEST_CPU, group_preserved
    );
    let guest_blocked =
        super::boot_status() == BootStatus::WaitFailed && super::ensure_guest_authorized().is_err();
    let passed = group_preserved
        && late_wait_succeeded
        && guest_blocked
        && retry_rejected
        && retained
        && cells_unchanged
        && record.failures.load(Ordering::Relaxed) == 0
        && super::boot_status() == BootStatus::WaitFailed;
    println!(
        "Phase 11.3 timeout-test retained slot={:#x} context={:#x} stack=[{:#x}, {:#x}) root={:#x}",
        record.slot_address(),
        record.identity.context,
        record.identity.stack_bottom,
        record.identity.stack_top,
        record.config.root
    );
    println!(
        "Phase 11.3 timeout-test {}: boot={:?} state={:?} late_wait_succeeded={} guest_blocked={} retry_rejected={} retained={} cells_unchanged={}",
        if passed { "PASS" } else { "FAIL" },
        super::boot_status(),
        record.mailbox.state(1),
        late_wait_succeeded,
        guest_blocked,
        retry_rejected,
        retained,
        cells_unchanged
    );
}
