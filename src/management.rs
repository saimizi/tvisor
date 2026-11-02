//! Host-only management SGIs. No allocator, UART formatting or waiting in IRQ.
use core::{
    arch::asm,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
use tvisor_util::{
    cpu_topology::MAX_CPUS,
    gicv2::{self, CpuTarget, GicV2, InterruptAck},
    management::{Channel, Operation, Pending, Reply},
    sync::{InterruptControl, LocalIrq},
};

#[cfg(feature = "management-irq-test")]
#[path = "management_test.rs"]
mod acceptance;

static CHANNELS: [[Channel; MAX_CPUS]; MAX_CPUS] =
    [const { [const { Channel::new() }; MAX_CPUS] }; MAX_CPUS];
static ACTIVATE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
struct CpuRuntime {
    target: AtomicU64,
    ready: AtomicBool,
    stopped: AtomicBool,
    generation: AtomicU64,
    halted: AtomicBool,
    control: AtomicU64,
    pmr: AtomicU64,
    bpr: AtomicU64,
    security: AtomicU64,
    current_irqs: AtomicU64,
    guest_irqs: AtomicU64,
    guest_masked: AtomicU64,
    requests: AtomicU64,
    stray: AtomicU64,
    last_token: AtomicU64,
    noninjection_checks: AtomicU64,
}
impl CpuRuntime {
    const fn new() -> Self {
        Self {
            target: AtomicU64::new(0),
            ready: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            halted: AtomicBool::new(false),
            control: AtomicU64::new(0),
            pmr: AtomicU64::new(0),
            bpr: AtomicU64::new(0),
            security: AtomicU64::new(0),
            current_irqs: AtomicU64::new(0),
            guest_irqs: AtomicU64::new(0),
            guest_masked: AtomicU64::new(0),
            requests: AtomicU64::new(0),
            stray: AtomicU64::new(0),
            last_token: AtomicU64::new(0),
            noninjection_checks: AtomicU64::new(0),
        }
    }
}
static CPUS: [CpuRuntime; MAX_CPUS] = [const { CpuRuntime::new() }; MAX_CPUS];

pub(crate) fn current_id() -> Option<usize> {
    let pointer: u64;
    unsafe {
        asm!("mrs {}, tpidr_el2", out(reg) pointer, options(nomem, nostack, preserves_flags))
    };
    crate::pcpu::PCPU_SLOTS
        .iter()
        .position(|s| s.context_ptr() as u64 == pointer && s.is_prepared())
}
pub fn counter() -> u64 {
    let value;
    unsafe {
        asm!("isb", "mrs {}, cntpct_el0", out(reg) value, options(nomem, nostack, preserves_flags))
    };
    value
}
pub(crate) fn frequency() -> u64 {
    let value;
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}
fn target(id: usize) -> Option<CpuTarget> {
    let cpu = CPUS.get(id)?;
    if !cpu.ready.load(Ordering::Acquire) {
        return None;
    }
    CpuTarget::new(cpu.target.load(Ordering::Relaxed) as u8).ok()
}
fn check_routing() -> Result<u64, &'static str> {
    let hcr: u64;
    unsafe { asm!("mrs {}, hcr_el2", out(reg) hcr, options(nomem, nostack, preserves_flags)) };
    // A temporary EL2 handler can run under either configured profile. Both
    // carry the shared IRQ policy; reject all unconfigured/stale register bits.
    if hcr != tvisor_util::el2_policy::HOST_HCR && hcr != tvisor_util::el2_policy::GUEST_HCR {
        return Err("invalid management HCR profile");
    }
    Ok(hcr)
}
fn initialize_local(worker: bool) -> Result<(), &'static str> {
    let id = current_id().ok_or("unknown management CPU")?;
    let gic = gicv2::global().ok_or("GIC missing")?;
    for region in [
        gic.info().distributor(),
        gic.info().cpu_interface(),
        gic.info().hypervisor_interface(),
    ] {
        crate::mm::validate_device_identity(region).map_err(|_| "invalid local GIC mapping")?;
    }
    check_routing()?;
    let state = unsafe { gic.initialize_cpu_interface(worker) }
        .map_err(|error| {
            if !worker { tvisor_util::println!("Phase 11.4 CPU interface rejected: {:?}; CTLR={:#x} PMR={:#x} BPR={} SGI-enabled={:?}",error,
                unsafe{core::ptr::read_volatile(gic.gicc_ctlr() as *const u32)},
                unsafe{core::ptr::read_volatile(gic.gicc_pmr() as *const u32)},
                unsafe{core::ptr::read_volatile((gic.gicc_ctlr()+8) as *const u32)},gic.gicd_isenabler_n_get(gicv2::MANAGEMENT_SGI)); }
            "management GIC contract failed"
        })?;
    for (other, cpu) in CPUS.iter().enumerate() {
        if other != id
            && cpu.ready.load(Ordering::Acquire)
            && cpu.target.load(Ordering::Relaxed) == state.target.mask() as u64
        {
            return Err("duplicate GIC target interface");
        }
    }
    let cpu = &CPUS[id];
    cpu.target
        .store(state.target.mask() as u64, Ordering::Relaxed);
    cpu.control.store(state.control as u64, Ordering::Relaxed);
    cpu.pmr.store(state.priority_mask as u64, Ordering::Relaxed);
    cpu.bpr.store(state.binary_point as u64, Ordering::Relaxed);
    cpu.security.store(
        (state.security_extensions as u64) | ((state.group1 as u64) << 1),
        Ordering::Relaxed,
    );
    cpu.ready.store(true, Ordering::Release);
    Ok(())
}
/// Boot CPU only, before shared tables are frozen and any secondary is released.
pub fn initialize_boot() -> Result<(), &'static str> {
    if current_id() != Some(0) {
        return Err("management boot initialization on worker");
    }
    let gic = gicv2::global().ok_or("GIC missing")?;
    for region in [
        gic.info().distributor(),
        gic.info().cpu_interface(),
        gic.info().hypervisor_interface(),
    ] {
        crate::mm::validate_device_identity(region).map_err(|_| "invalid boot GIC mapping")?;
    }
    tvisor_util::diagnostic_println!(
        "Phase 11.4 boot GIC TYPER={:#x} banked targets={:x?}",
        unsafe { core::ptr::read_volatile(gic.gicd_typer() as *const u32) },
        gic.banked_target_words()
    );
    let boot = gic.local_target().map_err(|error| {
        tvisor_util::println!("Phase 11.4 target discovery rejected: {:?}", error);
        "invalid boot GIC target"
    })?;
    unsafe { gic.initialize_distributor(crate::guest::get_host_console_irq(), boot) }.map_err(
        |error| {
            tvisor_util::println!("Phase 11.4 distributor rejected: {:?}", error);
            "GIC distributor initialization failed"
        },
    )?;
    initialize_local(false)?;
    #[cfg(feature = "diagnostics")]
    report(0);
    Ok(())
}
pub fn request(
    target_id: usize,
    operation: Operation,
    argument: u64,
) -> Result<Pending<'static>, &'static str> {
    let source = current_id().ok_or("unknown management sender")?;
    if target_id == source {
        return Err("self request unsupported");
    }
    let target = target(target_id).ok_or("management target not ready")?;
    if CPUS[target_id].stopped.load(Ordering::Acquire) {
        return Err("management target stopped");
    }
    let pending = CHANNELS[source][target_id]
        .submit(operation, argument)
        .map_err(|_| "management channel busy/exhausted")?;
    unsafe {
        gicv2::global()
            .ok_or("GIC missing")?
            .send_management(target)
    };
    Ok(pending)
}
pub(crate) fn wait(pending: &mut Pending<'_>) -> Result<Reply, &'static str> {
    let ticks = frequency()
        .checked_mul(2)
        .filter(|n| *n > 0 && *n < (1 << 63))
        .ok_or("invalid management deadline")?;
    let start = counter();
    for _ in 0..100_000_000 {
        if counter().wrapping_sub(start) >= ticks {
            return Err("management acknowledgement timeout");
        }
        if let Some(reply) = pending
            .try_result()
            .map_err(|_| "management receipt already consumed")?
        {
            return Ok(reply);
        }
        core::hint::spin_loop();
    }
    Err("management poll limit")
}
struct RestoreIrq(u64);
impl Drop for RestoreIrq {
    fn drop(&mut self) {
        LocalIrq::restore(self.0);
    }
}

/// Returns after validating the worker's interface and bidirectional boot-time
/// dispatch. CPU0 restores its previous IRQ mask before guest preparation.
pub fn activate_secondary() -> Result<(), &'static str> {
    let required = crate::secondary::required_mask();
    let _restore = RestoreIrq(LocalIrq::save_and_mask());
    LocalIrq::restore(0);
    for id in 1..MAX_CPUS {
        if required & (1 << id) == 0 {
            continue;
        }
        ACTIVATE[id]
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "management activation already attempted")?;
        unsafe { asm!("dsb ishst", "sev", options(nostack, preserves_flags)) };
        let ticks = frequency().checked_mul(2).ok_or("invalid counter")?;
        let start = counter();
        let mut ready = false;
        for _ in 0..100_000_000 {
            if crate::pcpu::has_fatal() {
                return Err("management initialization failed");
            }
            if target(id).is_some()
                && crate::pcpu::PCPU_SLOTS[id].startup.mailbox.lifecycle()
                    == tvisor_util::secondary_mailbox::CpuState::Online
            {
                ready = true;
                break;
            }
            if counter().wrapping_sub(start) >= ticks {
                return Err("management initialization timeout");
            }
            core::hint::spin_loop();
        }
        if !ready {
            return Err("management initialization poll limit");
        }
        #[cfg(feature = "diagnostics")]
        report(id);
        let value = 0x1150 + id as u64;
        let mut ping = request(id, Operation::Ping, value)?;
        if wait(&mut ping)?
            != (Reply {
                value,
                failed: false,
            })
        {
            return Err("worker ping mismatch");
        }
        let mut job = request(id, Operation::WorkerTest, 1024)?;
        if wait(&mut job)?
            != (Reply {
                value: worker_test(1024),
                failed: false,
            })
        {
            return Err("worker computation mismatch");
        }
        let mut rejected = request(id, Operation::WorkerTest, 4097)?;
        if !wait(&mut rejected)?.failed {
            return Err("unbounded worker job accepted");
        }
        // Owner startup also sends a source-bearing ping to CPU0. Waiting here
        // has IRQ enabled and holds no allocator/logging/table lock.
        let start = counter();
        for poll in 0..100_000_000 {
            if CHANNELS[id][0].completed_count() != 0 {
                break;
            }
            if poll == 99_999_999 {
                return Err("worker-to-boot ping poll limit");
            }
            if counter().wrapping_sub(start) >= ticks {
                return Err("worker-to-boot ping timeout");
            }
            core::hint::spin_loop();
        }
        tvisor_util::diagnostic_println!(
            "Phase 11.5 CPU{} Online management bidirectional PASS",
            id
        );
    }
    for id in 1..MAX_CPUS {
        if required & (1 << id) != 0 {
            for other in 0..id {
                if (other == 0 || required & (1 << other) != 0) && target(id) == target(other) {
                    return Err("duplicate GIC targets");
                }
            }
        }
    }
    let _hcr = check_routing()?;
    tvisor_util::diagnostic_println!(
        "Phase 11.5 all-core management PASS required_mask={:#x} HCR={:#x}",
        required,
        _hcr
    );
    #[cfg(feature = "management-irq-test")]
    acceptance::before_linux()?;
    Ok(())
}

fn worker_test(count: u64) -> u64 {
    let mut value = 0x114u64;
    for i in 0..count {
        value = value.rotate_left(7) ^ i.wrapping_mul(0x9e3779b97f4a7c15);
    }
    value
}
/// Called only for the reserved management INTID, with IRQ masked by exception
/// entry. The raw source interface is mapped through discovered targets.
pub fn dispatch(gic: &GicV2, ack: InterruptAck, guest: bool, guest_masked: bool) -> bool {
    let Some(id) = current_id() else {
        unsafe { gic.complete_host_interrupt(ack) };
        return false;
    };
    let cpu = &CPUS[id];
    let virtual_before = (gic.gich_vmcr_get(), gic.gich_lr0_get(), gic.gich_lr1_get());
    let routing_before = check_routing();
    cpu.last_token.store(ack.raw() as u64, Ordering::Relaxed);
    if guest {
        cpu.guest_irqs.fetch_add(1, Ordering::Relaxed);
    } else {
        cpu.current_irqs.fetch_add(1, Ordering::Relaxed);
    }
    if guest_masked {
        cpu.guest_masked.fetch_add(1, Ordering::Relaxed);
    }
    let source = ack.source().and_then(|interface| {
        CPUS.iter().position(|other| {
            other.ready.load(Ordering::Acquire)
                && other.target.load(Ordering::Relaxed) == 1u64 << interface
        })
    });
    let request = source
        .filter(|s| *s != id)
        .and_then(|s| CHANNELS[s][id].take());
    let Some(request) = request else {
        cpu.stray.fetch_add(1, Ordering::Relaxed);
        unsafe { gic.complete_host_interrupt(ack) };
        return source.is_some(); // a duplicate/coalesced doorbell needs no reply
    };
    let reply = match request.operation {
        Operation::Ping => Reply {
            value: request.argument,
            failed: false,
        },
        Operation::WorkerTest if id != 0 && request.argument <= 4096 => Reply {
            value: worker_test(request.argument),
            failed: false,
        },
        #[cfg(feature = "all-core-stress-test")]
        Operation::RunTest if id != 0 => Reply {
            value: request.argument,
            failed: !crate::all_core_test::submit(id, request.argument),
        },
        #[cfg(any(feature = "tlb-update-test", feature = "tlb-update-timeout-test"))]
        Operation::TlbQuiesce if id != 0 => Reply {
            value: request.argument,
            failed: crate::tlb::UPDATES.admit(id, request.argument).is_err(),
        },
        Operation::Park if id != 0 && crate::tlb::UPDATES.try_membership_change() => {
            let mailbox = &crate::pcpu::PCPU_SLOTS[id].startup.mailbox;
            let result = if mailbox.lifecycle() == tvisor_util::secondary_mailbox::CpuState::Parked
            {
                Ok(())
            } else {
                unsafe { mailbox.management_park(cpu.generation.load(Ordering::Relaxed)) }
            };
            crate::tlb::UPDATES.finish_membership_change();
            Reply {
                value: 0,
                failed: result.is_err(),
            }
        }
        Operation::FatalStop if crate::tlb::UPDATES.try_membership_change() => {
            cpu.stopped.store(true, Ordering::Release);
            crate::tlb::UPDATES.finish_membership_change();
            Reply {
                value: 0,
                failed: false,
            }
        }
        _ => Reply {
            value: 0,
            failed: true,
        },
    };
    unsafe { gic.complete_host_interrupt(ack) };
    let virtual_after = (gic.gich_vmcr_get(), gic.gich_lr0_get(), gic.gich_lr1_get());
    if virtual_before != virtual_after
        || routing_before.is_err()
        || routing_before != check_routing()
    {
        cpu.stopped.store(true, Ordering::Release);
        request.acknowledge(Reply {
            value: 0,
            failed: true,
        });
        return false;
    }
    cpu.noninjection_checks.fetch_add(1, Ordering::Relaxed);
    cpu.requests.fetch_add(1, Ordering::Relaxed);
    request.acknowledge(reply);
    true
}
/// Current-EL IRQs never borrow a vCPU or pass through guest injection.
pub fn current_irq() -> bool {
    let Some(gic) = gicv2::global() else {
        return false;
    };
    let Some(ack) = (unsafe { gic.acknowledge() }) else {
        return true;
    };
    if ack.intid() != gicv2::MANAGEMENT_SGI {
        unsafe { gic.complete_host_interrupt(ack) };
        return false;
    }
    let handled = dispatch(gic, ack, false, false);
    stop_if_requested();
    handled
}
pub fn stop_if_requested() {
    if current_id().is_some_and(|id| CPUS[id].stopped.load(Ordering::Acquire))
        || crate::pcpu::has_fatal()
    {
        unsafe { asm!("msr daifset, #15", options(nomem, nostack, preserves_flags)) };
        if let Some(id) = current_id() {
            if id != 0 {
                let mailbox = &crate::pcpu::PCPU_SLOTS[id].startup.mailbox;
                if mailbox.lifecycle() == tvisor_util::secondary_mailbox::CpuState::Online {
                    let _ = unsafe {
                        mailbox.management_park(CPUS[id].generation.load(Ordering::Relaxed))
                    };
                }
            }
            CPUS[id].halted.store(true, Ordering::Release);
        }
        tvisor_util::halt();
    }
}
/// Best-effort fatal doorbells; shared fatal records are the authority, so this
/// path needs no channel allocation or free request slot and never waits.
pub fn notify_fatal() {
    if let Some(gic) = gicv2::global() {
        for id in 0..MAX_CPUS {
            if Some(id) != current_id() {
                if let Some(t) = target(id) {
                    unsafe { gic.send_management(t) };
                }
            }
        }
    }
}
pub fn worker(id: usize, generation: u64) -> ! {
    while !ACTIVATE[id].load(Ordering::Acquire) {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
    CPUS[id].generation.store(generation, Ordering::Relaxed);
    if id == 0
        || crate::secondary::required_mask() & (1 << id) == 0
        || initialize_local(true).is_err()
    {
        let _ = unsafe {
            crate::pcpu::PCPU_SLOTS[id]
                .startup
                .mailbox
                .management_failed(generation)
        };
        crate::pcpu::record_fatal(tvisor_util::failure::Failure {
            kind: 3,
            syndrome: 0,
            pc: 0,
            address: id as u64,
        });
        notify_fatal();
        tvisor_util::halt();
    }
    let mailbox = &crate::pcpu::PCPU_SLOTS[id].startup.mailbox;
    // SAFETY: this CPU owns the prepared vectors, private stack and banked GIC.
    if unsafe { mailbox.management_online(generation) }.is_err() {
        tvisor_util::halt();
    }
    unsafe { asm!("msr daifclr, #2", "isb", options(nostack, preserves_flags)) };
    if let Ok(mut ping) = request(0, Operation::Ping, 0x1141) {
        if wait(&mut ping)
            != Ok(Reply {
                value: 0x1141,
                failed: false,
            })
        {
            CPUS[id].stopped.store(true, Ordering::Release);
        }
    } else {
        CPUS[id].stopped.store(true, Ordering::Release);
    }
    #[cfg(feature = "management-irq-test")]
    let mut test_worker = acceptance::Worker::new();
    loop {
        unsafe { asm!("msr daifset, #2", options(nostack, preserves_flags)) };
        stop_if_requested();
        #[cfg(any(feature = "tlb-update-test", feature = "tlb-update-timeout-test"))]
        if crate::tlb::UPDATES.pending(id) {
            unsafe { asm!("msr daifclr, #2", options(nostack, preserves_flags)) };
            crate::tlb::worker(id);
            continue;
        }
        #[cfg(feature = "all-core-stress-test")]
        if crate::all_core_test::pending(id) {
            unsafe { asm!("msr daifclr, #2", options(nostack, preserves_flags)) };
            crate::all_core_test::worker(id);
            continue;
        }
        #[cfg(feature = "management-irq-test")]
        if id == 1 && test_worker.active() {
            unsafe { asm!("msr daifclr, #2", options(nostack, preserves_flags)) };
            test_worker.step();
            continue;
        }
        unsafe {
            asm!(
                "dsb sy",
                "wfi",
                "msr daifclr, #2",
                "isb",
                options(nostack, preserves_flags)
            )
        };
    }
}
#[cfg(feature = "diagnostics")]
fn report(id: usize) {
    let c = &CPUS[id];
    let _ = c.ready.load(Ordering::Acquire);
    tvisor_util::diagnostic_println!(
        "Phase 11.4 CPU{} GIC target={:#x} CTLR={:#x} PMR={:#x} BPR={} security/group={} management SGI={}",
        id,
        c.target.load(Ordering::Relaxed),
        c.control.load(Ordering::Relaxed),
        c.pmr.load(Ordering::Relaxed),
        c.bpr.load(Ordering::Relaxed),
        c.security.load(Ordering::Relaxed),
        gicv2::MANAGEMENT_SGI
    );
}

/// Check every entry, including entries after stage-2 control rewrites. Reporting
/// happens outside IRQ with no allocator/table lock held.
pub fn guest_entry() {
    let hcr = check_routing().expect("guest entry lost physical IRQ routing");
    assert_eq!(
        hcr,
        tvisor_util::el2_policy::GUEST_HCR,
        "guest HCR profile not installed"
    );
    #[cfg(feature = "diagnostics")]
    static REPORTED: AtomicBool = AtomicBool::new(false);
    #[cfg(feature = "diagnostics")]
    if !REPORTED.swap(true, Ordering::Relaxed) {
        tvisor_util::diagnostic_println!(
            "Phase 11.4 guest IRQ routing HCR={:#x} IMO=1 FMO=0 AMO=0 TGE=0 E2H=0 VI=0 VF=0",
            hcr
        );
    }
    #[cfg(feature = "management-irq-test")]
    acceptance::linux_progress();
    #[cfg(feature = "all-core-stress-test")]
    crate::all_core_test::guest_progress();
}

#[cfg(feature = "management-irq-test")]
pub fn verify_stage2_profiles(
    regs: &tvisor_util::guest_stage2_translation::GuestStage2RegisterValues,
) {
    acceptance::stage2_profiles(regs).expect("HCR transition acceptance failed");
}

/// Caller holds the mapping-update gate, so Park/FatalStop cannot race this
/// snapshot. Parked CPUs remain members because they retain EL2 translations.
#[cfg(any(feature = "tlb-update-test", feature = "tlb-update-timeout-test"))]
pub fn validate_mapping_members(members: u64) -> Result<(), &'static str> {
    if members != crate::secondary::required_mask() | 1 {
        return Err("mapping membership changed");
    }
    if physical_smp_masks().0 != members {
        return Err("mapping member unavailable");
    }
    Ok(())
}

/// Observed initialized/retaining CPUs and their Parked subset, rather than
/// topology-derived expectations. Caller serializes lifecycle changes.
pub fn physical_smp_masks() -> (u64, u64) {
    use tvisor_util::secondary_mailbox::CpuState;
    let mut online = 0;
    let mut parked = 0;
    for (id, cpu) in CPUS.iter().enumerate() {
        if target(id).is_none() || cpu.stopped.load(Ordering::Acquire) {
            continue;
        }
        let state = crate::pcpu::PCPU_SLOTS[id].startup.mailbox.lifecycle();
        if id == 0 || matches!(state, CpuState::Online | CpuState::Parked) {
            online |= 1 << id;
            if id != 0 && state == CpuState::Parked {
                parked |= 1 << id;
            }
        }
    }
    (online, parked)
}
