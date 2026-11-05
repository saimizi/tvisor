//! Opt-in scratch remapping acceptance; no allocations or remaps in normal boot.
use super::UPDATES;
#[cfg(feature = "tlb-update-test")]
use super::park_workers;
use core::{
    arch::asm,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
use tvisor_util::{
    aarch64_reg::{counter_frequency, physical_counter},
    management::{Operation, Reply},
    sync::{InterruptControl, LocalIrq, RestoreIrq},
    tlb_update::Phase,
};
static PREPARED: AtomicBool = AtomicBool::new(false);
static PAGES: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static CURRENT: AtomicU64 = AtomicU64::new(0);
static OLD_VALUE: AtomicU64 = AtomicU64::new(0);
static NEW_VALUE: AtomicU64 = AtomicU64::new(0);
static STARTED: AtomicU64 = AtomicU64::new(0);
const INITIAL: u64 = 0x1160_0000_0000_0000;
pub fn prepare() -> Result<(), &'static str> {
    if PREPARED.swap(true, Ordering::AcqRel) {
        return Err("scratch preparation already attempted");
    }
    let pages = crate::mm::prepare_tlb_scratch(INITIAL)?;
    for (slot, pa) in PAGES.iter().zip(pages) {
        slot.store(pa, Ordering::Release);
    }
    CURRENT.store(pages[0], Ordering::Release);
    OLD_VALUE.store(INITIAL, Ordering::Release);
    tvisor_util::println!(
        "Phase 11.6 scratch VA={:#x} reserved backing={:x?}; mappings remain frozen except this leaf",
        tvisor_util::tlb_update::SCRATCH_VA,
        pages
    );
    Ok(())
}
pub fn ready_to_write(epoch: u64) -> bool {
    UPDATES.may_write(epoch)
}
/// Only the permanently reserved pair is admitted; callers cannot remap live
/// code, stacks, vectors, device mappings, or guest backing through this API.
pub fn owns(pa: u64) -> bool {
    pa != 0 && PAGES.iter().any(|p| p.load(Ordering::Acquire) == pa)
}
fn local_sync() {
    unsafe { asm!("dsb ish", "isb", options(nostack, preserves_flags)) };
}
fn read() -> u64 {
    let value;
    unsafe {
        asm!("ldr {}, [{}]",out(reg)value,in(reg)tvisor_util::tlb_update::SCRATCH_VA,options(nostack,preserves_flags))
    };
    value
}
fn elapsed() -> u64 {
    (physical_counter().wrapping_sub(STARTED.load(Ordering::Acquire))) & ((1u64 << 56) - 1)
}
fn wait_for(seconds: u64, mut condition: impl FnMut() -> bool) -> Result<(), &'static str> {
    let deadline = tvisor_util::secondary_boot::StartupDeadline::new(counter_frequency(), seconds)
        .ok_or("invalid mapping counter deadline")?;
    for _ in 0..1_000_000_000u64 {
        if UPDATES.phase() == Phase::Failed || crate::pcpu::has_fatal() {
            return Err("failed mapping update");
        }
        if condition() {
            return Ok(());
        }
        if elapsed() >= deadline.ticks() {
            return Err("mapping rendezvous timeout");
        }
        core::hint::spin_loop();
    }
    Err("mapping rendezvous poll limit")
}
#[cfg(feature = "tlb-update-timeout-test")]
static LATE_REJECTED: AtomicBool = AtomicBool::new(false);
pub fn worker(id: usize) {
    let epoch = UPDATES.epoch();
    #[cfg(feature = "tlb-update-timeout-test")]
    if id == 2 {
        let deadline = tvisor_util::secondary_boot::StartupDeadline::new(counter_frequency(), 4)
            .expect("fault deadline");
        for _ in 0..1_000_000_000u64 {
            if elapsed() >= deadline.ticks() {
                break;
            }
            core::hint::spin_loop();
        }
        // The real owner arrives late. The failed protocol must reject it.
        if UPDATES.quiesce(id, epoch).is_err() {
            LATE_REJECTED.store(true, Ordering::Release);
            return;
        }
        fatal("fault injection arrived before timeout");
    }
    // Warm an actual old translation before confirming the software safe point.
    if read() != OLD_VALUE.load(Ordering::Acquire) {
        fatal("old scratch signature mismatch");
    }
    local_sync();
    if UPDATES.quiesce(id, epoch).is_err() {
        fatal("invalid quiescent owner");
    }
    if wait_for(10, || UPDATES.phase() == Phase::Synchronizing).is_err() {
        #[cfg(feature = "tlb-update-timeout-test")]
        if UPDATES.phase() == Phase::Failed {
            return;
        }
        fatal("worker update wait failed");
    }
    local_sync();
    if read() != NEW_VALUE.load(Ordering::Acquire) {
        fatal("stale scratch translation");
    }
    local_sync();
    if UPDATES.synchronized(id, epoch).is_err() {
        fatal("invalid synchronization acknowledgement");
    }
    // No scratch/old-backing access after this acknowledgement. Return to the
    // holding loop so an immediately following epoch cannot be missed.
}
fn fatal(reason: &str) -> ! {
    UPDATES.fail();
    crate::pcpu::record_fatal(tvisor_util::failure::Failure {
        kind: 6,
        syndrome: UPDATES.epoch(),
        pc: 0,
        address: CURRENT.load(Ordering::Relaxed),
    });
    crate::management::notify_fatal();
    tvisor_util::println!(
        "Phase 11.6 FAIL epoch={} reason={}; mappings/backing retained until reset",
        UPDATES.epoch(),
        reason
    );
    tvisor_util::halt()
}
/// CPU0 only. Completion permits reuse of the *old* backing, never of a table
/// page. Both permanently reserved pages remain allocator-owned until reset.
pub fn remap() -> Result<u64, &'static str> {
    if crate::management::current_id() != Some(0) || CURRENT.load(Ordering::Acquire) == 0 {
        return Err("mapping coordinator unavailable");
    }
    let hcr: u64;
    unsafe { asm!("mrs {}, hcr_el2",out(reg) hcr,options(nomem,nostack,preserves_flags)) };
    if hcr != tvisor_util::el2_policy::HOST_HCR {
        return Err("mapping update requires deactivated guest/host HCR profile");
    }
    let _restore = RestoreIrq::<LocalIrq>::save_and_mask();
    LocalIrq::restore(0);
    let members = crate::secondary::required_mask() | 1;
    let epoch = UPDATES
        .begin(members)
        .map_err(|_| "mapping update busy/failed")?;
    STARTED.store(physical_counter(), Ordering::Release);
    let result = (|| {
        crate::management::validate_mapping_members(members)?;
        let old = CURRENT.load(Ordering::Acquire);
        let new = if old == PAGES[0].load(Ordering::Acquire) {
            PAGES[1].load(Ordering::Acquire)
        } else {
            PAGES[0].load(Ordering::Acquire)
        };
        let value = INITIAL.wrapping_add(epoch);
        // No member may use this old/reusable page after the preceding epoch.
        unsafe { core::ptr::write_volatile(new as *mut u64, value) };
        NEW_VALUE.store(value, Ordering::Release);
        if read() != OLD_VALUE.load(Ordering::Acquire) {
            return Err("coordinator old signature mismatch");
        }
        local_sync();
        UPDATES
            .quiesce(0, epoch)
            .map_err(|_| "coordinator quiescence")?;
        for id in 1..tvisor_util::cpu_topology::MAX_CPUS {
            if members & (1 << id) == 0 {
                continue;
            }
            let mut pending = crate::management::request(id, Operation::TlbQuiesce, epoch)?;
            if crate::management::wait(&mut pending)?
                != (Reply {
                    value: epoch,
                    failed: false,
                })
            {
                return Err("mapping admission rejected");
            }
        }
        #[cfg(feature = "tlb-update-timeout-test")]
        let seconds = 2;
        #[cfg(not(feature = "tlb-update-timeout-test"))]
        let seconds = 10;
        wait_for(seconds, || UPDATES.all_quiet())?;
        UPDATES
            .start_replace(epoch)
            .map_err(|_| "not all owners quiescent")?;
        crate::mm::replace_tlb_scratch(epoch, old, new)?;
        UPDATES
            .publish(epoch)
            .map_err(|_| "mapping publication state")?;
        local_sync();
        if read() != value {
            return Err("coordinator stale translation");
        }
        local_sync();
        UPDATES
            .synchronized(0, epoch)
            .map_err(|_| "coordinator synchronization")?;
        wait_for(10, || UPDATES.all_synced())?;
        UPDATES
            .finish(epoch)
            .map_err(|_| "incomplete mapping acknowledgements")?;
        if !UPDATES.reusable(epoch) {
            return Err("old backing cannot be reused");
        }
        CURRENT.store(new, Ordering::Release);
        OLD_VALUE.store(value, Ordering::Release);
        // Deliberately repurpose the old page only after hardware *and* software
        // completion. A subsequent stale translation will observe this poison.
        unsafe { core::ptr::write_volatile(old as *mut u64, !value) };
        tvisor_util::println!(
            "Phase 11.6 remap PASS epoch={} members={:#x} old={:#x} new={:#x} value={:#x} old_reusable=true",
            epoch,
            members,
            old,
            new,
            value
        );
        Ok(epoch)
    })();
    match result {
        Ok(epoch) => Ok(epoch),
        Err(reason) => {
            #[cfg(feature = "tlb-update-timeout-test")]
            {
                UPDATES.fail();
                return Err(reason);
            }
            #[cfg(not(feature = "tlb-update-timeout-test"))]
            fatal(reason)
        }
    }
}
#[cfg(feature = "tlb-update-test")]
pub fn before_guest() -> Result<(), &'static str> {
    let before = crate::mm::allocator_stats().map_err(|_| "allocator stats")?;
    for _ in 0..32 {
        remap()?;
    }
    park_workers()?;
    for _ in 0..16 {
        remap()?;
    }
    if before != crate::mm::allocator_stats().map_err(|_| "allocator stats")? {
        return Err("mapping test leaked pages");
    }
    tvisor_util::println!(
        "Phase 11.6 scratch acceptance PASS updates=48 online_and_parked=true old_reuse_checked=true"
    );
    Ok(())
}

#[cfg(feature = "tlb-update-timeout-test")]
pub fn timeout_acceptance() -> Result<(), &'static str> {
    let before = crate::mm::allocator_stats().map_err(|_| "fault allocator stats")?;
    let old = CURRENT.load(Ordering::Acquire);
    let result = remap();
    if result != Err("mapping rendezvous timeout")
        || UPDATES.phase() != Phase::Failed
        || crate::mm::tlb_scratch_pa() != Some(old)
    {
        return Err("wrong mapping fault outcome");
    }
    let deadline = tvisor_util::secondary_boot::StartupDeadline::new(counter_frequency(), 7)
        .ok_or("observer deadline")?;
    for _ in 0..1_000_000_000u64 {
        if LATE_REJECTED.load(Ordering::Acquire) || elapsed() >= deadline.ticks() {
            break;
        }
        core::hint::spin_loop();
    }
    let passed = LATE_REJECTED.load(Ordering::Acquire)
        && UPDATES.begin(crate::secondary::required_mask() | 1)
            == Err(tvisor_util::tlb_update::Error::Failed)
        && !UPDATES.reusable(UPDATES.epoch())
        && crate::mm::tlb_scratch_pa() == Some(old)
        && crate::mm::allocator_stats().map_err(|_| "fault allocator stats")? == before;
    tvisor_util::println!(
        "Phase 11.6 timeout acceptance {} epoch={} CPU2_late_rejected={} old_mapping_retained=true backing_retained=true guest_blocked=true",
        if passed { "PASS" } else { "FAIL" },
        UPDATES.epoch(),
        LATE_REJECTED.load(Ordering::Acquire)
    );
    crate::pcpu::record_fatal(tvisor_util::failure::Failure {
        kind: 6,
        syndrome: UPDATES.epoch(),
        pc: 0,
        address: old,
    });
    crate::management::notify_fatal();
    Err("mapping timeout fault image deliberately blocks guest")
}
