//! Opt-in hardware acceptance instrumentation, absent from normal builds.
//! Delay only after coherent entry, local validation and checkpoint writes.
//! Never change mappings, registers, release routing or publication maintenance.
use super::{BOOT_ATTEMPT, SECONDARY_RECORD, physical_counter, report_checkpoint};
use core::sync::atomic::Ordering;
use tvisor_util::{
    println,
    secondary_boot::{BootError, BootStatus, StartupDeadline},
    secondary_mailbox::{Outcome, ProtocolError, WorkerState},
    system_info::PhysAddr,
};

const MAX_POLLS: usize = 250_000_000;

fn deadline(seconds: u64) -> Option<StartupDeadline> {
    let frequency: u64;
    unsafe {
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) frequency,
            options(nomem, nostack, preserves_flags));
    }
    StartupDeadline::new(frequency, seconds)
}

pub(super) fn delay_acknowledgement() {
    if let Some(deadline) = deadline(4) {
        let start = physical_counter();
        for _ in 0..MAX_POLLS {
            if deadline.elapsed(start, physical_counter()) >= deadline.ticks() {
                return;
            }
            core::hint::spin_loop();
        }
    }
    // A broken clock/poll fallback must not produce an early acknowledgement.
    // CPU0 still has its unchanged finite startup wait and bounded observer.
    loop {
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}

/// Observe a real late arrival without retrying release or minting a permit.
/// Invoked after the production attempt and abort-finalization have completed.
pub(super) fn observe_terminal_failure() {
    let Some(record) = SECONDARY_RECORD.get(1) else {
        println!("Phase 11.3 timeout-test FAIL: no retained record");
        return;
    };
    if BOOT_ATTEMPT.status() != BootStatus::WaitFailed
        || record.mailbox.outcome() != Outcome::TimedOut
    {
        println!("Phase 11.3 timeout-test FAIL: expected terminal timeout");
        return;
    }
    println!(
        "Phase 11.3 timeout-test terminal before late arrival: boot={:?} outcome={:?} worker={:?}",
        BOOT_ATTEMPT.status(),
        record.mailbox.outcome(),
        record.mailbox.state(1)
    );
    let Some(deadline) = deadline(5) else {
        println!("Phase 11.3 timeout-test FAIL: invalid observer deadline");
        return;
    };
    let start = physical_counter();
    for _ in 0..MAX_POLLS {
        if record.mailbox.state(1) == Ok(WorkerState::Parked)
            || deadline.elapsed(start, physical_counter()) >= deadline.ticks()
        {
            break;
        }
        core::hint::spin_loop();
    }
    if record.mailbox.state(1) != Ok(WorkerState::Parked) {
        println!("Phase 11.3 timeout-test FAIL: no late parked checkpoint");
        return;
    }
    report_checkpoint(record);
    let late_success_rejected =
        record.mailbox.finish(1, Outcome::Success) == Err(ProtocolError::AlreadyFinished);
    // This probes only the one-shot gate; it never calls start_one/release again.
    let retry_rejected = matches!(BOOT_ATTEMPT.begin(), Err(BootError::AlreadyAttempted));
    let retained = SECONDARY_RECORD
        .get(1)
        .is_some_and(|again| core::ptr::eq(record, again))
        && crate::pcpu::validate_secondary_context(
            record.logical_id,
            record.affinity,
            record.stack_bottom,
            record.stack_top,
            record.checkpoint.tpidr.load(Ordering::Relaxed),
        );
    unsafe extern "C" {
        fn __secondary_entry();
    }
    let cells_unchanged = crate::smp::topology()
        .and_then(|topology| {
            tvisor_util::secondary_entry::validate_spin_table_target(
                topology.cpus(),
                record.logical_id as usize,
            )
            .ok()
        })
        .is_some_and(|address| {
            crate::smp::verify_release_result(
                PhysAddr::new(address),
                __secondary_entry as *const () as u64,
            )
            .is_ok()
        });
    let passed = late_success_rejected
        && retry_rejected
        && retained
        && cells_unchanged
        && record.failures.load(Ordering::Relaxed) == 0
        && record.mailbox.outcome() == Outcome::TimedOut
        && BOOT_ATTEMPT.status() == BootStatus::WaitFailed;
    println!(
        "Phase 11.3 timeout-test retained record={:#x} context={:#x} stack=[{:#x}, {:#x}) root={:#x}",
        record as *const _ as u64,
        record.context,
        record.stack_bottom,
        record.stack_top,
        record.root
    );
    println!(
        "Phase 11.3 timeout-test {}: boot={:?} outcome={:?} worker={:?} late_success_rejected={} retry_rejected={} retained={} cells_unchanged={}",
        if passed { "PASS" } else { "FAIL" },
        BOOT_ATTEMPT.status(),
        record.mailbox.outcome(),
        record.mailbox.state(1),
        late_success_rejected,
        retry_rejected,
        retained,
        cells_unchanged
    );
}
