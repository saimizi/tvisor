//! Opt-in, bounded acceptance workloads. IRQs only admit a one-shot job;
//! allocation, logging, barriers and management waits run in ordinary EL2 code.
//! A failed job retains its allocations and state until reset.
use crate::management::{request, wait};
use core::{
    alloc::Layout,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
use tvisor_util::{
    aarch64_reg::{counter_frequency as frequency, physical_counter as counter},
    management::{Operation, Reply},
    sync::{InterruptControl, LocalIrq, RestoreIrq},
    system_info::PhysAddr,
};
const CPUS: usize = 4;
const OBJECTS: usize = 12;
struct CpuJob {
    state: AtomicU64, // 0 unused, 1 admitted, 2 running, 3 complete
    stage: AtomicU64,
    addresses: [AtomicU64; OBJECTS],
    pings: AtomicU64,
}
impl CpuJob {
    const fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
            stage: AtomicU64::new(0),
            addresses: [const { AtomicU64::new(0) }; OBJECTS],
            pings: AtomicU64::new(0),
        }
    }
}
struct Phase {
    jobs: [CpuJob; CPUS],
    started: AtomicU64,
    failed: AtomicBool,
}
impl Phase {
    const fn new() -> Self {
        Self {
            jobs: [const { CpuJob::new() }; CPUS],
            started: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        }
    }
}
static PHASES: [Phase; 2] = [const { Phase::new() }; 2];
static GUEST_STARTED: AtomicBool = AtomicBool::new(false);
static GUEST_FINISHED: AtomicBool = AtomicBool::new(false);
// Only CPU0 accesses the saved baseline, before admission and after completion.
static BASELINE_PAGES: AtomicU64 = AtomicU64::new(0);
static BASELINE_USED: AtomicU64 = AtomicU64::new(0);
static BASELINE_LIVE: AtomicU64 = AtomicU64::new(0);
static BASELINE_FAILURES: AtomicU64 = AtomicU64::new(0);
fn members(phase: usize) -> u64 {
    crate::secondary::required_mask() | if phase == 0 { 1 } else { 0 }
}
fn size(index: usize) -> usize {
    if index < 8 {
        4096
    } else {
        128 + (index - 8) * 137
    }
}
fn bounded(phase: usize, mut predicate: impl FnMut() -> bool) -> Result<(), &'static str> {
    let p = &PHASES[phase];
    let ticks = frequency()
        .checked_mul(90)
        .ok_or("stress counter overflow")?;
    for _ in 0..2_000_000_000u64 {
        if p.failed.load(Ordering::Acquire) {
            return Err("another stress worker failed");
        }
        if predicate() {
            return Ok(());
        }
        if counter().wrapping_sub(p.started.load(Ordering::Acquire)) >= ticks {
            return Err("stress deadline");
        }
        core::hint::spin_loop();
    }
    Err("stress poll limit")
}
fn barrier(phase: usize, id: usize, stage: u64) -> Result<(), &'static str> {
    let p = &PHASES[phase];
    p.jobs[id].stage.store(stage, Ordering::Release);
    bounded(phase, || {
        (0..CPUS).all(|other| {
            members(phase) & (1 << other) == 0
                || p.jobs[other].stage.load(Ordering::Acquire) >= stage
        })
    })
}
/// IRQ context: fixed storage and one CAS, with no allocation or wait.
pub fn submit(id: usize, phase: u64) -> bool {
    let Some(p) = PHASES.get(phase as usize) else {
        return false;
    };
    id > 0
        && id < CPUS
        && members(phase as usize) & (1 << id) != 0
        && p.started.load(Ordering::Acquire) != 0
        && p.jobs[id]
            .state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
}
pub fn pending(id: usize) -> bool {
    PHASES
        .iter()
        .any(|p| p.jobs[id].state.load(Ordering::Acquire) == 1)
}
pub fn worker(id: usize) {
    for phase in 0..2 {
        if PHASES[phase].jobs[id]
            .state
            .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            if let Err(error) = exercise(phase, id) {
                fail(phase, id, error);
            }
            PHASES[phase].jobs[id].state.store(3, Ordering::Release);
        }
    }
}
fn fail(phase: usize, id: usize, error: &str) -> ! {
    PHASES[phase].failed.store(true, Ordering::Release);
    tvisor_util::println!(
        "Phase 11.5 STRESS FAIL phase={} CPU{} reason={}; resources retained",
        phase,
        id,
        error
    );
    crate::pcpu::record_fatal(tvisor_util::failure::Failure {
        kind: 5,
        syndrome: phase as u64,
        pc: 0,
        address: id as u64,
    });
    crate::management::notify_fatal();
    tvisor_util::halt()
}
fn exercise(phase: usize, id: usize) -> Result<(), &'static str> {
    let rounds = if phase == 0 { 256 } else { 640 };
    let job = &PHASES[phase].jobs[id];
    for round in 0..rounds {
        // All addresses remain published until every participant has checked
        // disjointness, written and verified its payload. No pointer is used
        // until the complete set has passed the overlap check.
        for i in 0..OBJECTS {
            let address = if i < 8 {
                crate::mm::allocate_page()
                    .map_err(|_| "physical allocation")?
                    .value()
            } else {
                let layout = Layout::from_size_align(size(i), 64).map_err(|_| "heap layout")?;
                let ptr = unsafe { alloc::alloc::alloc(layout) };
                if ptr.is_null() {
                    return Err("heap allocation");
                }
                ptr as u64
            };
            job.addresses[i].store(address, Ordering::Relaxed);
        }
        barrier(phase, id, round * 3 + 1)?;
        let mut ranges = [tvisor_util::allocation_check::Range::EMPTY; CPUS * OBJECTS];
        let mut count = 0;
        for other in 0..CPUS {
            if members(phase) & (1 << other) == 0 {
                continue;
            }
            for i in 0..OBJECTS {
                ranges[count] = tvisor_util::allocation_check::Range {
                    start: PHASES[phase].jobs[other].addresses[i].load(Ordering::Relaxed),
                    bytes: size(i) as u64,
                    alignment: if i < 8 { 4096 } else { 64 },
                };
                count += 1;
            }
        }
        tvisor_util::allocation_check::validate(&ranges[..count])
            .map_err(|_| "invalid/overlapping live allocations")?;
        for i in 0..OBJECTS {
            let ptr = job.addresses[i].load(Ordering::Relaxed) as *mut u8;
            let byte = (id * 37 + i * 11 + round as usize) as u8;
            // SAFETY: exclusive disjoint live allocation, bounded by its layout.
            unsafe {
                core::ptr::write_bytes(ptr, byte, size(i));
            }
        }
        barrier(phase, id, round * 3 + 2)?;
        for i in 0..OBJECTS {
            let ptr = job.addresses[i].load(Ordering::Relaxed) as *const u8;
            let byte = (id * 37 + i * 11 + round as usize) as u8;
            for offset in 0..size(i) {
                if unsafe { core::ptr::read_volatile(ptr.add(offset)) } != byte {
                    return Err("payload corruption");
                }
            }
        }
        barrier(phase, id, round * 3 + 3)?;
        // Reverse the release order on alternating rounds to exercise merging.
        for n in 0..OBJECTS {
            let i = if round % 2 == 0 { n } else { OBJECTS - 1 - n };
            let address = job.addresses[i].load(Ordering::Relaxed);
            if i < 8 {
                crate::mm::free_page(PhysAddr::new(address)).map_err(|_| "physical free")?;
            } else {
                unsafe {
                    alloc::alloc::dealloc(
                        address as *mut u8,
                        Layout::from_size_align(size(i), 64).unwrap(),
                    );
                }
            }
        }
        if round % 4 == 0 {
            for other in 0..CPUS {
                if other == id || (other != 0 && members(phase) & (1 << other) == 0) {
                    continue;
                }
                let value =
                    ((phase as u64) << 48) | ((id as u64) << 40) | (round << 8) | other as u64;
                let mut pending = request(other, Operation::Ping, value)?;
                if wait(&mut pending)?
                    != (Reply {
                        value,
                        failed: false,
                    })
                {
                    return Err("cross-CPU ping mismatch");
                }
                job.pings.fetch_add(1, Ordering::Relaxed);
            }
        }
        if round % 32 == 0 {
            tvisor_util::println!(
                "Phase 11.5 STRESS phase={} CPU{} round={} logging PASS",
                phase,
                id,
                round
            );
        }
        if phase == 1 {
            let until =
                PHASES[phase].started.load(Ordering::Acquire) + (round + 1) * frequency() / 10;
            bounded(phase, || counter() >= until)?;
        }
    }
    tvisor_util::println!(
        "Phase 11.5 STRESS phase={} CPU{} rounds={} pages={} heap={} pings={} PASS",
        phase,
        id,
        rounds,
        rounds * 8,
        rounds * 4,
        job.pings.load(Ordering::Relaxed)
    );
    Ok(())
}
fn baseline() -> Result<(), &'static str> {
    BASELINE_PAGES.store(
        crate::mm::allocator_stats()
            .map_err(|_| "page stats")?
            .in_use_pages as u64,
        Ordering::Relaxed,
    );
    let h = crate::heap::stats().map_err(|_| "heap stats")?;
    BASELINE_USED.store(h.used_bytes as u64, Ordering::Relaxed);
    BASELINE_LIVE.store(h.live_allocations as u64, Ordering::Relaxed);
    BASELINE_FAILURES.store(h.failed_allocations as u64, Ordering::Relaxed);
    Ok(())
}
fn start(phase: usize) -> Result<(), &'static str> {
    baseline()?;
    PHASES[phase]
        .started
        .compare_exchange(0, counter().max(1), Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| "phase already started")?;
    for id in 1..CPUS {
        if members(phase) & (1 << id) == 0 {
            continue;
        }
        let mut pending = request(id, Operation::RunTest, phase as u64)?;
        if wait(&mut pending)?
            != (Reply {
                value: phase as u64,
                failed: false,
            })
        {
            return Err("job admission rejected");
        }
    }
    Ok(())
}
fn done(phase: usize) -> bool {
    (0..CPUS).all(|id| {
        members(phase) & (1 << id) == 0 || PHASES[phase].jobs[id].state.load(Ordering::Acquire) == 3
    })
}
fn verify(phase: usize) -> Result<(), &'static str> {
    let pages = crate::mm::allocator_stats().map_err(|_| "final page stats")?;
    let heap = crate::heap::stats().map_err(|_| "final heap stats")?;
    if pages.in_use_pages as u64 != BASELINE_PAGES.load(Ordering::Relaxed)
        || heap.used_bytes as u64 != BASELINE_USED.load(Ordering::Relaxed)
        || heap.live_allocations as u64 != BASELINE_LIVE.load(Ordering::Relaxed)
        || heap.failed_allocations as u64 != BASELINE_FAILURES.load(Ordering::Relaxed)
    {
        return Err("allocator baseline not restored");
    }
    tvisor_util::println!(
        "Phase 11.5 STRESS phase={} mask={:#x} allocator baseline restored pages={} heap_used={} live={} PASS",
        phase,
        members(phase),
        pages.in_use_pages,
        heap.used_bytes,
        heap.live_allocations
    );
    Ok(())
}
pub fn before_guest() -> Result<(), &'static str> {
    let _restore = RestoreIrq::<LocalIrq>::save_and_mask();
    LocalIrq::restore(0);
    start(0)?;
    PHASES[0].jobs[0].state.store(2, Ordering::Release);
    exercise(0, 0)?;
    PHASES[0].jobs[0].state.store(3, Ordering::Release);
    bounded(0, || done(0))?;
    verify(0)
}
pub fn guest_progress() {
    if !GUEST_STARTED.swap(true, Ordering::AcqRel) {
        if let Err(e) = start(1) {
            fail(1, 0, e);
        }
    }
    if !GUEST_FINISHED.load(Ordering::Acquire) {
        if PHASES[1].failed.load(Ordering::Acquire)
            || counter().wrapping_sub(PHASES[1].started.load(Ordering::Acquire)) > frequency() * 90
        {
            fail(1, 0, "guest stress deadline/failure");
        }
        if done(1) {
            if let Err(e) = verify(1) {
                fail(1, 0, e);
            }
            GUEST_FINISHED.store(true, Ordering::Release);
        }
    }
}
