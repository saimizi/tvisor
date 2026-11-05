//! Explicit hardware acceptance only; normal workers sleep with timers off.
use super::*;
use core::arch::global_asm;

static MODE: AtomicU64 = AtomicU64::new(0);
static SENT: AtomicU64 = AtomicU64::new(0);
static ACKED: AtomicU64 = AtomicU64::new(0);
static QUIESCENT: AtomicBool = AtomicBool::new(true);
static FAILURE_CODE: AtomicU64 = AtomicU64::new(0);
fn fail(code: u64) {
    let _ = FAILURE_CODE.compare_exchange(0, code, Ordering::Relaxed, Ordering::Relaxed);
    FAILED.store(true, Ordering::Release);
}
static FAILED: AtomicBool = AtomicBool::new(false);
static MAX_ACK_TICKS: AtomicU64 = AtomicU64::new(0);

pub struct Worker {
    pending: Option<Pending<'static>>,
    start: u64,
    next: u64,
    serial: u64,
}
impl Worker {
    pub const fn new() -> Self {
        Self {
            pending: None,
            start: 0,
            next: 0,
            serial: 0,
        }
    }
    pub fn active(&self) -> bool {
        let active = self.pending.is_some() || MODE.load(Ordering::Acquire) != 0;
        QUIESCENT.store(!active, Ordering::Release);
        active
    }
    pub fn step(&mut self) -> bool {
        if let Some(p) = self.pending.as_mut() {
            match p.try_result() {
                Ok(Some(reply)) => {
                    if reply.failed || reply.value != self.serial {
                        fail(1);
                    }
                    MAX_ACK_TICKS.fetch_max(counter().wrapping_sub(self.start), Ordering::Relaxed);
                    ACKED.fetch_add(1, Ordering::Release);
                    self.pending = None;
                }
                Ok(None) => {
                    if counter().wrapping_sub(self.start) > frequency() * 2 {
                        fail(2);
                    }
                }
                Err(_) => {
                    fail(3);
                }
            }
        }
        let mode = MODE.load(Ordering::Acquire);
        if mode == 0 {
            return self.pending.is_some();
        }
        if self.pending.is_none() && counter().wrapping_sub(self.next) < 1 << 63 {
            self.serial += 1;
            match request(0, Operation::Ping, self.serial) {
                Ok(p) => {
                    self.pending = Some(p);
                    self.start = counter();
                    self.next = self.start + frequency() / 50;
                    SENT.fetch_add(1, Ordering::Release);
                }
                Err(_) => {
                    fail(4);
                }
            }
        }
        core::hint::spin_loop();
        true
    }
}
fn arm(mode: u64) -> Result<(), &'static str> {
    QUIESCENT.store(false, Ordering::Release);
    MODE.store(mode, Ordering::Release);
    let mut wake = request(1, Operation::Ping, mode)?;
    if wait(&mut wake)?
        != (Reply {
            value: mode,
            failed: false,
        })
    {
        return Err("acceptance wake mismatch");
    }
    Ok(())
}

fn drain() -> Result<(), &'static str> {
    MODE.store(0, Ordering::Release);
    let _restore = RestoreIrq::<LocalIrq>::save_and_mask();
    LocalIrq::restore(0);
    let start = counter();
    for _ in 0..100_000_000 {
        if QUIESCENT.load(Ordering::Acquire)
            && SENT.load(Ordering::Acquire) == ACKED.load(Ordering::Acquire)
        {
            return Ok(());
        }
        if counter().wrapping_sub(start) > frequency() {
            return Err("acceptance drain deadline");
        }
        core::hint::spin_loop();
    }
    Err("acceptance drain poll limit")
}

// Both probes keep guest PSTATE.I=1. x2..x30 and all SIMD registers hold known
// patterns across every lower-EL IRQ. No memory or stack access occurs at EL1.
global_asm!(
    r#"
    .section .text.management_probe,"ax"
    .global __management_masked_probe
__management_masked_probe:
    mrs x0, cntpct_el0
    cmp x0, x1
    b.lo __management_masked_probe
    hvc #0x114
1:  b 1b
    .global __management_wfi_probe
__management_wfi_probe:
    wfi
    mrs x0, cntpct_el0
    cmp x0, x1
    b.lo __management_wfi_probe
    hvc #0x114
2:  b 2b
"#
);
unsafe extern "C" {
    fn __management_masked_probe();
    fn __management_wfi_probe();
}

fn probe(wfi: bool) -> Result<(), &'static str> {
    let gic = gicv2::global().ok_or("probe GIC missing")?;
    let entry = if wfi {
        __management_wfi_probe as *const () as u64
    } else {
        __management_masked_probe as *const () as u64
    };
    let mut vcpu = crate::vcpu::Vcpu::new(entry, 0);
    for i in 2..31 {
        vcpu.context_mut().x[i] = 0x1140_0000 + i as u64;
    }
    for i in 0..32 {
        vcpu.context_mut().q[i] = ((i as u128 + 1) << 64) | 0x1140_0000 | i as u128;
    }
    let expected_q = vcpu.context().q;
    vcpu.context_mut().x[1] = counter() + frequency();
    arm(if wfi { 2 } else { 1 })?;
    let before = CPUS[0].guest_masked.load(Ordering::Acquire);
    let start = counter();
    // Stage-2 is still off; this explicit feature-only EL1 stub uses physical
    // addresses. It cannot run arbitrary guest code or modify host memory.
    loop {
        if counter().wrapping_sub(start) > frequency() * 3 {
            return Err("masked EL1 probe deadline");
        }
        let vector = unsafe { crate::vcpu::__vcpu_run(&mut vcpu) };
        for i in 2..31 {
            if vcpu.context().x[i] != 0x1140_0000 + i as u64 {
                return Err("masked EL1 GPR corruption");
            }
        }
        if vcpu.context().q != expected_q {
            return Err("masked EL1 SIMD corruption");
        }
        if vcpu.context().spsr_el2 & 0x3cf != 0x3c5 {
            return Err("masked EL1 PSTATE corruption");
        }
        match vector {
            9 => {
                if let Some(ack) = unsafe { gic.acknowledge() } {
                    if ack.intid() != gicv2::MANAGEMENT_SGI {
                        unsafe { gic.complete_host_interrupt(ack) };
                        return Err("non-management probe IRQ");
                    }
                    if !dispatch(gic, ack, true, true) {
                        return Err("masked EL1 dispatch failed");
                    }
                }
            }
            8 if vcpu.exit().esr_el2 >> 26 == 0x16 && vcpu.exit().esr_el2 & 0xffff == 0x114 => {
                break;
            }
            _ => return Err("unexpected masked EL1 probe exit"),
        }
    }
    drain()?;
    let irqs = CPUS[0].guest_masked.load(Ordering::Acquire) - before;
    if irqs < 10 || FAILED.load(Ordering::Acquire) {
        return Err("insufficient/failed masked EL1 SGIs");
    }
    tvisor_util::println!(
        "Phase 11.4 TEST masked-EL1 {} PASS: IRQs={} GPR/SIMD/PSTATE preserved management injection=0",
        if wfi { "WFI" } else { "active" },
        irqs
    );
    Ok(())
}
pub fn before_linux() -> Result<(), &'static str> {
    tvisor_util::println!("Phase 11.4 TEST ONLY: management-irq-test");
    // A request pending in a bounded EL2 masked region is delivered on unmask.
    arm(1)?;
    let saved = LocalIrq::save_and_mask();
    let old = CPUS[0].current_irqs.load(Ordering::Acquire);
    let start = counter();
    while counter().wrapping_sub(start) < frequency() / 20 {
        core::hint::spin_loop();
    }
    if CPUS[0].current_irqs.load(Ordering::Acquire) != old {
        return Err("IRQ dispatched inside masked EL2 region");
    }
    LocalIrq::restore(0);
    let start = counter();
    while CPUS[0].current_irqs.load(Ordering::Acquire) == old {
        if counter().wrapping_sub(start) > frequency() {
            return Err("pending EL2 SGI not serviced");
        }
    }
    MODE.store(0, Ordering::Release);
    LocalIrq::restore(saved);
    tvisor_util::println!("Phase 11.4 TEST masked EL2 pending SGI PASS");
    drain()?;
    unsafe { asm!("msr daifset, #2", options(nostack, preserves_flags)) };
    probe(false)?;
    probe(true)?;
    // Start Linux traffic only at its first actual guest entry, after the
    // potentially long masked VM/resource preparation has completed.
    Ok(())
}
pub fn linux_progress() {
    if FAILED.load(Ordering::Acquire) {
        panic!(
            "Phase 11.4 management acceptance failed: code={} sent={} ack={} max_ack_ticks={}",
            FAILURE_CODE.load(Ordering::Relaxed),
            SENT.load(Ordering::Acquire),
            ACKED.load(Ordering::Acquire),
            MAX_ACK_TICKS.load(Ordering::Relaxed)
        );
    }
    static START: AtomicU64 = AtomicU64::new(0);
    static ARMED: AtomicBool = AtomicBool::new(false);
    if !ARMED.swap(true, Ordering::Relaxed) {
        START.store(counter(), Ordering::Relaxed);
        arm(3).expect("Linux management test arming failed");
    }
    static STAGE: AtomicU64 = AtomicU64::new(0);
    static CONTROL: tvisor_util::sync::IrqSpinLock<Option<Pending<'static>>> =
        tvisor_util::sync::IrqSpinLock::new(None);
    let stage = STAGE.load(Ordering::Relaxed);
    if stage == 0 && counter().wrapping_sub(START.load(Ordering::Relaxed)) >= frequency() * 30 {
        MODE.store(0, Ordering::Release);
        STAGE.store(1, Ordering::Relaxed);
    } else if stage == 1
        && QUIESCENT.load(Ordering::Acquire)
        && SENT.load(Ordering::Acquire) == ACKED.load(Ordering::Acquire)
    {
        *CONTROL.lock() = Some(request(1, Operation::Park, 0).expect("park request"));
        STAGE.store(2, Ordering::Relaxed);
    } else if (2..=4).contains(&stage) {
        let mut pending = CONTROL.lock();
        let reply = pending
            .as_mut()
            .expect("control receipt")
            .try_result()
            .expect("control receipt reused");
        if let Some(reply) = reply {
            assert!(!reply.failed, "management control failed");
            match stage {
                2 => {
                    assert_eq!(
                        crate::pcpu::PCPU_SLOTS[1].startup.mailbox.lifecycle(),
                        tvisor_util::secondary_mailbox::CpuState::Parked
                    );
                    *pending = Some(request(1, Operation::Ping, 0x1142).expect("parked ping"));
                }
                3 => {
                    assert_eq!(reply.value, 0x1142);
                    *pending = Some(request(1, Operation::FatalStop, 0).expect("fatal stop"));
                }
                _ => {
                    *pending = None;
                }
            }
            STAGE.store(stage + 1, Ordering::Relaxed);
        }
    } else if stage == 5 && CPUS[1].halted.load(Ordering::Acquire) {
        STAGE.store(6, Ordering::Relaxed);
        tvisor_util::println!(
            "Phase 11.4 TEST responsive park and fatal-stop PASS: CPU1 Parked IRQ-masked retained=true"
        );
    }
    if (1..6).contains(&STAGE.load(Ordering::Relaxed))
        && counter().wrapping_sub(START.load(Ordering::Relaxed)) > frequency() * 34
    {
        panic!("Phase 11.4 management control deadline");
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = counter();
    let next = NEXT.load(Ordering::Relaxed);
    if now.wrapping_sub(next) < 1 << 63 {
        NEXT.store(now + frequency() * 10, Ordering::Relaxed);
        tvisor_util::println!(
            "Phase 11.4 TEST Linux host counters: sent={} ack={} current={} lower={} masked={} stray={} max_ack_us={} noninjection_checks={} management_injection=0 raw={:#x}",
            SENT.load(Ordering::Acquire),
            ACKED.load(Ordering::Acquire),
            CPUS[0].current_irqs.load(Ordering::Relaxed),
            CPUS[0].guest_irqs.load(Ordering::Relaxed),
            CPUS[0].guest_masked.load(Ordering::Relaxed),
            CPUS[0].stray.load(Ordering::Relaxed),
            MAX_ACK_TICKS
                .load(Ordering::Relaxed)
                .saturating_mul(1_000_000)
                / frequency(),
            CPUS[0].noninjection_checks.load(Ordering::Relaxed),
            CPUS[0].last_token.load(Ordering::Relaxed)
        );
    }
}

/// Exercise the production activation/deactivation operations with the actual,
/// retained guest root, before any guest executes. UART RX is already enabled
/// by VM preparation, so mask its physical SPI during the host-only SGI window;
/// the current-EL test handler intentionally accepts only management SGIs.
pub fn stage2_profiles(
    regs: &tvisor_util::guest_stage2_translation::GuestStage2RegisterValues,
) -> Result<(), &'static str> {
    macro_rules! read {
        ($reg:literal) => {{
            let value: u64;
            unsafe {
                asm!(concat!("mrs {}, ", $reg), out(reg) value, options(nomem, nostack, preserves_flags));
            }
            value
        }};
    }
    use tvisor_util::el2_policy::{GUEST_HCR, HOST_HCR};
    let gic = gicv2::global().ok_or("HCR test GIC missing")?;
    if read!("daif") & (1 << 7) == 0 || read!("hcr_el2") != HOST_HCR {
        return Err("HCR test requires masked host profile");
    }
    let uart = crate::guest::get_host_console_irq();
    let enabled = gic
        .gicd_isenabler_n_get(uart)
        .map_err(|_| "UART mask read failed")?;
    gic.gicd_disable_irq(uart)
        .map_err(|_| "UART test mask failed")?;
    // Drop restores the original IRQ mask before the UART is re-enabled below,
    // including on an ordinary error return from this test closure.
    let result = (|| {
        let _restore = RestoreIrq::<LocalIrq>::save_and_mask();
        unsafe { crate::guest::activate_guest_stage2(regs) };
        if read!("hcr_el2") != GUEST_HCR || read!("vttbr_el2") != regs.vttbr_el2 {
            return Err("guest HCR/VTTBR readback failed");
        }
        unsafe { crate::guest::deactivate_guest_stage2() };
        if read!("hcr_el2") != HOST_HCR || read!("vttbr_el2") != 0 {
            return Err("host HCR/VTTBR restoration failed");
        }
        let before = CPUS[0].current_irqs.load(Ordering::Acquire);
        arm(1)?;
        LocalIrq::restore(0);
        let start = counter();
        let mut serviced = false;
        for _ in 0..100_000_000 {
            if counter().wrapping_sub(start) >= frequency() * 2 {
                return Err("post-deactivation host SGI deadline");
            }
            if CPUS[0].current_irqs.load(Ordering::Acquire) > before {
                serviced = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !serviced {
            return Err("post-deactivation host SGI poll limit");
        }
        drain()?;
        let host_irqs = CPUS[0].current_irqs.load(Ordering::Acquire) - before;
        let _ = LocalIrq::save_and_mask();
        // A second activation checks that deactivation left no inherited guest
        // controls or VMID. Keep the tables retained throughout both cycles.
        unsafe { crate::guest::activate_guest_stage2(regs) };
        if read!("hcr_el2") != GUEST_HCR || read!("vttbr_el2") != regs.vttbr_el2 {
            return Err("guest HCR/VTTBR reactivation failed");
        }
        unsafe { crate::guest::deactivate_guest_stage2() };
        if read!("hcr_el2") != HOST_HCR || read!("vttbr_el2") != 0 {
            return Err("final host HCR/VTTBR restoration failed");
        }
        if FAILED.load(Ordering::Acquire) {
            return Err("HCR test management request failed");
        }
        tvisor_util::println!(
            "Phase 11.4 TEST HCR profiles PASS: guest={:#x} host={:#x} VTTBR-cleared=true reactivated=true host_sgi_irqs={}",
            GUEST_HCR,
            HOST_HCR,
            host_irqs
        );
        Ok(())
    })();
    if enabled {
        gic.gicd_enable_irq(uart)
            .map_err(|_| "UART test mask restore failed")?;
    }
    result
}
