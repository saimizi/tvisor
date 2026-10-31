//! Stackless secondary entry. This entry is not released until the backend
//! publishes a validated record and completes the spin-table release protocol.
use core::{arch::global_asm, sync::atomic::Ordering};
use tvisor_util::secondary_mailbox::Outcome;
use tvisor_util::secondary_record::{RecordStorage, SecondaryRecord, StartupRecord};

#[cfg(feature = "secondary-startup-timeout-test")]
#[path = "secondary_timeout_test.rs"]
mod timeout_test;

#[unsafe(no_mangle)]
static SECONDARY_RECORD: RecordStorage = RecordStorage::new();

pub fn publish_record(data: StartupRecord) -> Result<(), &'static str> {
    SECONDARY_RECORD
        .publish(data)
        .map_err(|_| "secondary record publication failed")
}

static BOOT_ATTEMPT: tvisor_util::secondary_boot::SecondaryBoot =
    tvisor_util::secondary_boot::SecondaryBoot::new();
pub fn boot_status() -> tvisor_util::secondary_boot::BootStatus {
    BOOT_ATTEMPT.status()
}

/// Boot CPU waits without holding a resource lock. The physical counter is
/// additionally bounded by polls so a broken counter cannot hang startup.
fn wait_for_secondary(
    generation: u64,
    deadline: tvisor_util::secondary_boot::StartupDeadline,
    start: u64,
) -> Result<(), &'static str> {
    let record = SECONDARY_RECORD
        .get(generation)
        .ok_or("secondary record is not published")?;
    let outcome = record.mailbox.wait_and_finish(generation, 0, deadline.ticks(), 100_000_000, || {
        let now: u64;
        unsafe { core::arch::asm!("isb", "mrs {}, cntpct_el0", out(reg) now, options(nomem, nostack, preserves_flags)); }
        deadline.elapsed(start, now)
    }).map_err(|_| "invalid secondary wait transition")?;
    #[cfg(feature = "secondary-startup-timeout-test")]
    tvisor_util::println!(
        "Phase 11.3 timeout-test wait elapsed ticks={} budget={}",
        deadline.elapsed(start, physical_counter()),
        deadline.ticks()
    );
    tvisor_util::println!(
        "Phase 11.3 generation={} outcome={:?} worker={:?}",
        generation,
        outcome,
        record.mailbox.state(generation)
    );
    if matches!(
        record.mailbox.state(generation),
        Ok(tvisor_util::secondary_mailbox::WorkerState::Parked
            | tvisor_util::secondary_mailbox::WorkerState::Failed)
    ) {
        report_checkpoint(record);
    }
    match outcome {
        Outcome::Success => {}
        Outcome::SecondaryFailed => return Err("secondary local validation failed"),
        Outcome::TimedOut => return Err("secondary startup timed out"),
        Outcome::PollLimit => return Err("secondary startup poll limit exceeded"),
        Outcome::InvalidDeadline => return Err("invalid secondary startup deadline"),
        Outcome::Pending => return Err("secondary startup remained pending"),
        Outcome::BootAborted => return Err("secondary boot was aborted"),
    }
    Ok(())
}

fn report_checkpoint(record: &SecondaryRecord) {
    let c = &record.checkpoint;
    tvisor_util::println!(
        "Phase 11.3 CPU{} checkpoint generation={} EL={:#x} MPIDR={:#x} SP={:#x} TPIDR={:#x} VBAR={:#x}",
        record.logical_id,
        record.generation,
        c.current_el.load(Ordering::Relaxed),
        c.affinity.load(Ordering::Relaxed),
        c.sp.load(Ordering::Relaxed),
        c.tpidr.load(Ordering::Relaxed),
        c.vbar.load(Ordering::Relaxed)
    );
    tvisor_util::println!(
        "    MAIR={:#x} TCR={:#x} TTBR={:#x} SCTLR={:#x} DAIF={:#x} HCR={:#x} CPTR={:#x}",
        c.mair.load(Ordering::Relaxed),
        c.tcr.load(Ordering::Relaxed),
        c.root.load(Ordering::Relaxed),
        c.sctlr.load(Ordering::Relaxed),
        c.daif.load(Ordering::Relaxed),
        c.hcr.load(Ordering::Relaxed),
        c.cptr.load(Ordering::Relaxed)
    );
    tvisor_util::println!(
        "    entry SCTLR={:#x} MIDR={:#x} CPUECTLR={:#x} MPIDR={:#x}",
        record.entry.sctlr.load(Ordering::Relaxed),
        record.entry.midr.load(Ordering::Relaxed),
        record.entry.cpuectlr.load(Ordering::Relaxed),
        record.entry.mpidr.load(Ordering::Relaxed)
    );
    tvisor_util::println!(
        "    HSTR={:#x} VTTBR={:#x} CNTVOFF={} CNTHCTL={:#x} timers={:#x}/{:#x}/{:#x} SPSel={}",
        c.hstr.load(Ordering::Relaxed),
        c.vttbr.load(Ordering::Relaxed),
        c.cntvoff.load(Ordering::Relaxed),
        c.cnthctl.load(Ordering::Relaxed),
        c.cnthp_ctl.load(Ordering::Relaxed),
        c.cntp_ctl.load(Ordering::Relaxed),
        c.cntv_ctl.load(Ordering::Relaxed),
        c.spsel.load(Ordering::Relaxed)
    );
    let failures = record.failures.load(Ordering::Relaxed);
    tvisor_util::println!("    local validation failures={:#x}", failures);
    for (i, name) in tvisor_util::secondary_validation::FAILURE_NAMES
        .iter()
        .enumerate()
    {
        if failures & (1 << i) != 0 {
            tvisor_util::println!("    failed: {}", name);
        }
    }
}

global_asm!(
    concat!(r#"
    .section .text.secondary, "ax"
    .balign 64
    .global __secondary_entry
    .type __secondary_entry, %function
__secondary_entry:
    msr daifset, #0xf
    mrs x9, CurrentEL
    cmp x9, #8
    b.ne .Lsecondary_reject
    mrs x9, sctlr_el2
    mov x21, x9
    // Require little-endian, MMU and data cache disabled on physical entry.
    tst x9, #1
    b.ne .Lsecondary_reject
    tbnz x9, #2, .Lsecondary_reject
    tbnz x9, #25, .Lsecondary_reject
    // This initial backend is Cortex-A72 only. Check before reading its
    // implementation-defined coherency control register.
    mrs x9, midr_el1
    mov x22, x9
    ubfx x10, x9, #24, #8
    cmp x10, #0x41
    b.ne .Lsecondary_reject
    ubfx x10, x9, #4, #12
    mov x11, #0xd08
    cmp x10, x11
    b.ne .Lsecondary_reject
    mrs x9, S3_1_C15_C2_1
    mov x23, x9
    tbz x9, #6, .Lsecondary_reject
    // Only the explicitly published startup record may be read before Rust.
    adrp x19, SECONDARY_RECORD
    add x19, x19, :lo12:SECONDARY_RECORD
    ldr x20, [x19, #{generation}]
    cbz x20, .Lsecondary_reject
    mrs x9, mpidr_el1
    mov x24, x9
    // Construct the full affinity mask without a literal-pool read.
    movz x10, #0xffff
    movk x10, #0x00ff, lsl #16
    movk x10, #0x00ff, lsl #32
    and x9, x9, x10
    ldr x10, [x19, #{affinity}]
    cmp x9, x10
    b.ne .Lsecondary_reject
    // Validate all stack/context inputs before installing either pointer.
    ldr x9, [x19, #{logical_id}]
    cbz x9, .Lsecondary_reject
    cmp x9, #{max_cpus}
    b.hs .Lsecondary_reject
    ldr x9, [x19, #{stack_top}]
    cbz x9, .Lsecondary_reject
    tst x9, #15
    b.ne .Lsecondary_reject
    ldr x10, [x19, #{stack_bottom}]
    cbz x10, .Lsecondary_reject
    tst x10, #4095
    b.ne .Lsecondary_reject
    subs x11, x9, x10
    b.ls .Lsecondary_reject
    cmp x11, #{stack_size}
    b.ne .Lsecondary_reject
    ldr x12, [x19, #{context}]
    cbz x12, .Lsecondary_reject
    tst x12, #63
    b.ne .Lsecondary_reject
    ldr x10, [x19, #{root}]
    cbz x10, .Lsecondary_reject
    tst x10, #4095
    b.ne .Lsecondary_reject
    msr spsel, #1
    isb
    mov sp, x9
    .global __secondary_stack_ready
__secondary_stack_ready:
    msr tpidr_el2, x12
    adrp x9, __el2_vectors
    add x9, x9, :lo12:__el2_vectors
    msr vbar_el2, x9
"#, include_str!("host_el2_controls.S"), r#"
    ldr x9, [x19, #{mair}]
    msr mair_el2, x9
    ldr x9, [x19, #{tcr}]
    msr tcr_el2, x9
    ldr x9, [x19, #{root}]
    msr ttbr0_el2, x9
    isb
    dsb sy
    tlbi alle2
    ic iallu
    dsb sy
    isb
    ldr x9, [x19, #{sctlr}]
    msr sctlr_el2, x9
    isb
    // Coherency and mappings must be valid before an acquire atomic access.
    add x9, x19, #{mailbox}
    ldar x10, [x9]
    cmp x10, x20
    b.ne .Lsecondary_reject
    // First ordinary shared Rust access is permitted beyond this boundary.
    .global __secondary_rust_boundary
__secondary_rust_boundary:
    mov x0, x19
    mov x1, x21
    mov x2, x22
    mov x3, x23
    mov x4, x24
    bl secondary_main
.Lsecondary_reject:
    .global __secondary_rejected
__secondary_rejected:
    wfe
    b .Lsecondary_reject
    .size __secondary_entry, . - __secondary_entry
"#),
    host_hcr = const tvisor_util::el2_policy::HOST_HCR,
    host_cptr = const tvisor_util::el2_policy::HOST_CPTR,
    host_hstr = const tvisor_util::el2_policy::HOST_HSTR,
    host_vttbr = const tvisor_util::el2_policy::HOST_VTTBR,
    host_cntvoff = const tvisor_util::el2_policy::HOST_CNTVOFF,
    host_cnthctl = const tvisor_util::el2_policy::HOST_CNTHCTL,
    host_timer = const tvisor_util::el2_policy::DISABLED_TIMER_CTL,
    logical_id = const core::mem::offset_of!(SecondaryRecord, logical_id),
    stack_bottom = const core::mem::offset_of!(SecondaryRecord, stack_bottom),
    max_cpus = const tvisor_util::cpu_topology::MAX_CPUS,
    stack_size = const tvisor_util::pcpu::STACK_SIZE,
    affinity = const core::mem::offset_of!(SecondaryRecord, affinity),
    generation = const core::mem::offset_of!(SecondaryRecord, generation),
    stack_top = const core::mem::offset_of!(SecondaryRecord, stack_top),
    context = const core::mem::offset_of!(SecondaryRecord, context),
    mair = const core::mem::offset_of!(SecondaryRecord, mair),
    tcr = const core::mem::offset_of!(SecondaryRecord, tcr),
    root = const core::mem::offset_of!(SecondaryRecord, root),
    sctlr = const core::mem::offset_of!(SecondaryRecord, sctlr),
    mailbox = const core::mem::offset_of!(SecondaryRecord, mailbox),
);

#[unsafe(no_mangle)]
extern "C" fn secondary_main(
    record: &SecondaryRecord,
    entry_sctlr: u64,
    midr: u64,
    cpuectlr: u64,
    mpidr: u64,
) -> ! {
    if record.mailbox.enter(record.generation).is_err() {
        loop {
            unsafe {
                core::arch::asm!("wfe", options(nomem, nostack));
            }
        }
    }
    use tvisor_util::secondary_validation::{EntryState, LocalState, publish_checkpoint, validate};
    macro_rules! read { ($reg:literal) => {{ let value: u64;
        unsafe { core::arch::asm!(concat!("mrs {}, ", $reg), out(reg) value, options(nomem, nostack, preserves_flags)); } value
    }}; }
    let sp: u64;
    unsafe {
        core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags));
    }
    let state = LocalState {
        current_el: read!("CurrentEL"),
        affinity: read!("mpidr_el1"),
        sp: sp,
        tpidr: read!("tpidr_el2"),
        vbar: read!("vbar_el2"),
        mair: read!("mair_el2"),
        tcr: read!("tcr_el2"),
        root: read!("ttbr0_el2"),
        sctlr: read!("sctlr_el2"),
        daif: read!("daif"),
        hcr: read!("hcr_el2"),
        cptr: read!("cptr_el2"),
        hstr: read!("hstr_el2"),
        vttbr: read!("vttbr_el2"),
        cntvoff: read!("cntvoff_el2"),
        cnthctl: read!("cnthctl_el2"),
        cnthp_ctl: read!("cnthp_ctl_el2"),
        cntp_ctl: read!("cntp_ctl_el0"),
        cntv_ctl: read!("cntv_ctl_el0"),
        spsel: read!("spsel"),
    };
    let entry = EntryState {
        sctlr: entry_sctlr,
        midr,
        cpuectlr,
        mpidr,
    };
    unsafe extern "C" {
        static __el2_vectors: u8;
    }
    let context_valid = crate::pcpu::validate_secondary_context(
        record.logical_id,
        record.affinity,
        record.stack_bottom,
        record.stack_top,
        state.tpidr,
    );
    let failures = validate(
        record,
        state,
        entry,
        core::ptr::addr_of!(__el2_vectors) as u64,
        context_valid,
        crate::boot::memory_map().is_some(),
    );
    publish_checkpoint(record, state, entry, failures);
    if failures.0 == 0 {
        #[cfg(feature = "secondary-startup-timeout-test")]
        timeout_test::delay_acknowledgement();
        if record.mailbox.acknowledge(record.generation).is_ok() {
            let _ = record.mailbox.park(record.generation);
        }
    } else {
        let _ = record.mailbox.fail(record.generation);
    }
    loop {
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack));
        }
    }
}

/// One boot attempt under the documented lab-rpi4 firmware trust profile.
pub fn start_one(
    fdt: dtoolkit::fdt::Fdt<'_>,
    policy: tvisor_util::el2_policy::HostTranslation,
) -> Result<tvisor_util::secondary_boot::GuestPermit, &'static str> {
    let attempt = BOOT_ATTEMPT
        .begin()
        .map_err(|_| "secondary boot already attempted")?;
    let result = start_attempt(fdt, policy, attempt);
    if result.is_err() {
        if let Some(record) = SECONDARY_RECORD.get(1) {
            let _ = record.mailbox.finish(1, Outcome::BootAborted);
        }
        #[cfg(feature = "secondary-startup-timeout-test")]
        timeout_test::observe_terminal_failure();
    }
    result
}

fn start_attempt(
    fdt: dtoolkit::fdt::Fdt<'_>,
    policy: tvisor_util::el2_policy::HostTranslation,
    mut attempt: tvisor_util::secondary_boot::Attempt<'_>,
) -> Result<tvisor_util::secondary_boot::GuestPermit, &'static str> {
    let topology = crate::smp::topology().ok_or("missing CPU topology")?;
    let midr: u64;
    let mpidr: u64;
    unsafe {
        core::arch::asm!("mrs {}, midr_el1", out(reg) midr, options(nomem, nostack, preserves_flags));
        core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
    }
    let backend =
        tvisor_util::secondary_backend::LabRpi4Backend::select(fdt, topology, midr, mpidr)
            .map_err(|error| {
                tvisor_util::println!("Phase 11.3 backend rejected: {:?}", error);
                "unsupported lab-rpi4 backend profile"
            })?;
    let entry_contract = backend.contract();
    #[cfg(feature = "secondary-startup-timeout-test")]
    tvisor_util::println!(
        "Phase 11.3 TEST ONLY: secondary-startup-timeout-test; acknowledgement delayed 4 seconds"
    );
    tvisor_util::println!(
        "Phase 11.3 backend=lab-rpi4-armstub MIDR={:#x} MPIDR={:#x}",
        midr,
        mpidr
    );
    use tvisor_util::{
        secondary_entry::{select_secondary, validate_spin_table_target},
        system_info::PhysAddr,
    };
    crate::pcpu::assert_boot_cpu_idle();
    let topology = crate::smp::topology().ok_or("missing CPU topology")?;
    let id = select_secondary(topology.cpus()).map_err(|_| "no eligible secondary")?;
    // Check the platform evidence before making even reversible mapping changes.
    if !entry_contract.el2_mmu_off || !entry_contract.coherent {
        return Err("secondary entry/coherency contract is unverified");
    }
    let address = validate_spin_table_target(topology.cpus(), id)
        .map_err(|_| "invalid secondary release target")?;
    let (affinity, stack_top, context) =
        crate::pcpu::secondary_entry(id).ok_or("secondary resources unavailable")?;
    if !topology
        .cpus()
        .iter()
        .any(|cpu| cpu.enabled && cpu.logical_id == Some(id) && cpu.affinity == affinity)
    {
        return Err("prepared secondary affinity differs from topology");
    }
    let actual_mair: u64;
    let actual_tcr: u64;
    let actual_root: u64;
    let actual_sctlr: u64;
    unsafe {
        core::arch::asm!("mrs {}, mair_el2", out(reg) actual_mair, options(nomem, nostack, preserves_flags));
        core::arch::asm!("mrs {}, tcr_el2", out(reg) actual_tcr, options(nomem, nostack, preserves_flags));
        core::arch::asm!("mrs {}, ttbr0_el2", out(reg) actual_root, options(nomem, nostack, preserves_flags));
        core::arch::asm!("mrs {}, sctlr_el2", out(reg) actual_sctlr, options(nomem, nostack, preserves_flags));
    }
    if (actual_mair, actual_tcr, actual_root, actual_sctlr)
        != (policy.mair(), policy.tcr(), policy.root(), policy.sctlr())
    {
        return Err("active host translation differs from startup policy");
    }
    let stack_bottom = stack_top
        .checked_sub(tvisor_util::pcpu::STACK_SIZE)
        .ok_or("invalid secondary stack")?;
    let frequency: u64;
    unsafe {
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) frequency, options(nomem, nostack, preserves_flags));
    }
    let deadline = tvisor_util::secondary_boot::StartupDeadline::new(frequency, 2)
        .ok_or("invalid startup counter frequency")?;
    let counter_start = physical_counter();
    let mut progressed = false;
    for _ in 0..100_000 {
        if tvisor_util::secondary_boot::StartupDeadline::counter_progresses(
            counter_start,
            physical_counter(),
        ) {
            progressed = true;
            break;
        }
    }
    if !progressed {
        return Err("physical startup counter did not progress");
    }
    tvisor_util::println!(
        "Phase 11.3 selected CPU{} affinity={:#x} cell={:#x}; deadline ticks={} frequency={} Hz",
        id,
        affinity,
        address,
        deadline.ticks(),
        frequency
    );
    crate::mm::map_firmware_release_cell(PhysAddr::new(address))
        .map_err(|_| "release-cell mapping failed")?;
    unsafe extern "C" {
        static __text_start: u8;
        static __text_end: u8;
        static __vectors_start: u8;
        static __vectors_end: u8;
        static __rodata_start: u8;
        static __rodata_end: u8;
        static __writable_start: u8;
        static __writable_end: u8;
        static __el2_stage1_tables_start: u8;
        static __el2_stage1_tables_end: u8;
        fn __secondary_entry();
    }
    let image_start = core::ptr::addr_of!(__text_start) as u64;
    let image_end = core::ptr::addr_of!(__writable_end) as u64;
    let tables_start = core::ptr::addr_of!(__el2_stage1_tables_start) as u64;
    let tables_end = core::ptr::addr_of!(__el2_stage1_tables_end) as u64;
    // Validate every cache-maintenance address while CPU 0 owns all mappings.
    for (start, end) in [
        (image_start, image_end),
        (tables_start, tables_end),
        (stack_bottom, stack_top),
    ] {
        validate_identity_range(start, end)?;
    }
    publish_record(StartupRecord {
        logical_id: id as u64,
        affinity,
        stack_bottom,
        stack_top,
        context,
        mair: policy.mair(),
        tcr: policy.tcr(),
        root: policy.root(),
        sctlr: policy.sctlr(),
        generation: 1,
    })?;
    use tvisor_util::{
        startup_resources::{StartupResources, release_page},
        system_info::PhysRegion,
    };
    let bounds = |start, end| {
        PhysRegion::from_bounds(PhysAddr::new(start), PhysAddr::new(end))
            .map_err(|_| "invalid startup region")
    };
    let page = release_page(
        crate::boot::memory_map().ok_or("memory map unavailable")?,
        address,
    )
    .map_err(|_| "firmware release page ownership failed")?;
    let resources = StartupResources {
        code: bounds(image_start, core::ptr::addr_of!(__text_end) as u64)?,
        vectors: bounds(
            core::ptr::addr_of!(__vectors_start) as u64,
            core::ptr::addr_of!(__vectors_end) as u64,
        )?,
        rodata: bounds(
            core::ptr::addr_of!(__rodata_start) as u64,
            core::ptr::addr_of!(__rodata_end) as u64,
        )?,
        writable: bounds(core::ptr::addr_of!(__writable_start) as u64, image_end)?,
        tables: bounds(tables_start, tables_end)?,
        record: PhysRegion::new(
            PhysAddr::new(core::ptr::addr_of!(SECONDARY_RECORD) as u64),
            core::mem::size_of::<RecordStorage>() as u64,
        )
        .map_err(|_| "invalid record region")?,
        context: PhysRegion::new(
            PhysAddr::new(context),
            core::mem::size_of::<tvisor_util::pcpu::PcpuContext<()>>() as u64,
        )
        .map_err(|_| "invalid context region")?,
        release_page: page,
        stack: tvisor_util::pcpu::stack_layout(
            stack_bottom
                .checked_sub(tvisor_util::pcpu::GUARD_SIZE)
                .ok_or("invalid stack guard")?,
        )
        .map_err(|_| "invalid stack layout")?,
        entry: __secondary_entry as *const () as u64,
        root: policy.root(),
    };
    crate::smp::verify_unreleased_cell(PhysAddr::new(address))
        .map_err(|_| "release cell is not zero or identity mapped")?;
    let stacks = crate::pcpu::prepared_stacks()?;
    let frozen = crate::mm::validate_and_freeze_startup(&resources, &stacks)
        .map_err(|_| "startup mapping validation/freeze failed")?;
    let record = SECONDARY_RECORD
        .get(1)
        .ok_or("startup record unavailable")?;
    let ctr: u64;
    unsafe {
        core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack, preserves_flags));
    }
    let line = 4u64 << ((ctr >> 16) & 15);
    let plan = tvisor_util::startup_publication::PublicationPlan::new(frozen, address, line)
        .map_err(|_| "invalid cache publication plan")?;
    // SAFETY: actual active identity mappings were checked; the frozen plan
    // names retained static/allocated resources and a reserved unique cell.
    let mut io = unsafe { crate::smp::HardwarePublication::new() };
    let published = plan
        .publish(topology.cpus(), id, entry_contract, record, &mut io)
        .map_err(|_| "startup cache publication/preflight failed")?;
    attempt
        .release_attempted()
        .map_err(|_| "invalid secondary release lifecycle")?;
    published
        .release(&mut io)
        .map_err(|_| "secondary release failed; reset required")?;
    let start = io
        .release_start()
        .ok_or("missing secondary release timestamp")?;
    if let Err(error) = wait_for_secondary(1, deadline, start)
        .and_then(|()| crate::smp::verify_release_result(PhysAddr::new(address), resources.entry))
    {
        attempt.wait_failed();
        return Err(error);
    }
    attempt
        .complete(record, 1, crate::pcpu::has_fatal())
        .map_err(|_| "secondary checkpoint cannot permit guest boot")
}

fn validate_identity_range(start: u64, end: u64) -> Result<(), &'static str> {
    if start >= end || start & 4095 != 0 || end & 4095 != 0 {
        return Err("invalid startup mapping range");
    }
    for address in (start..end).step_by(4096) {
        let par: u64;
        unsafe {
            core::arch::asm!("mrs {saved}, par_el1", "at s1e2r, {address}", "isb",
            "mrs {result}, par_el1", "msr par_el1, {saved}", saved=out(reg) _, result=out(reg) par,
            address=in(reg) address, options(nostack, preserves_flags));
        }
        if par & 1 != 0 || par & 0x000f_ffff_ffff_f000 != address {
            return Err("startup mapping is not identity mapped");
        }
    }
    Ok(())
}

fn physical_counter() -> u64 {
    let value: u64;
    unsafe {
        core::arch::asm!("isb", "mrs {}, cntpct_el0", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}
