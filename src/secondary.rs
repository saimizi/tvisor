//! Stackless secondary entry. This entry is not released until the backend
//! publishes a validated record and completes the spin-table release protocol.
use core::{arch::global_asm, sync::atomic::Ordering};
use tvisor_util::{
    aarch64_reg::{counter_frequency, physical_counter},
    el2_policy::{HostEl2Config, HostEl2ConfigStorage},
    pcpu::{PcpuContext, PcpuSlot},
    secondary_startup::StartupView,
    startup_resources::StartupResources,
};
use tvisor_util::{
    secondary_boot::{self, BootError, BootStatus, StartupDeadline},
    secondary_mailbox::CpuState,
};

#[cfg(any(
    feature = "secondary-startup-timeout-test",
    feature = "all-core-timeout-test"
))]
#[path = "secondary_timeout_test.rs"]
mod timeout_test;

#[unsafe(no_mangle)]
static HOST_EL2_CONFIG: HostEl2ConfigStorage = HostEl2ConfigStorage::new();
fn startup(id: usize, generation: u64) -> Option<StartupView<'static>> {
    crate::pcpu::slot(id)?.startup_view(generation, HOST_EL2_CONFIG.get()?)
}

const MAX_CPUS: usize = tvisor_util::cpu_topology::MAX_CPUS;
static BOOT_STATE: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(BootStatus::Idle as u64);
// A bit means potentially released, not successful arrival. Set before STR.
static RELEASE_ATTEMPTED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
const GENERATION: u64 = 1;
pub fn required_mask() -> u64 {
    crate::smp::topology()
        .and_then(|t| tvisor_util::secondary_entry::secondary_mask(t.cpus()).ok())
        .unwrap_or(0)
}
pub fn boot_status() -> tvisor_util::secondary_boot::BootStatus {
    secondary_boot::boot_status(&BOOT_STATE)
}

/// Boot CPU waits without holding a resource lock. The physical counter is
/// additionally bounded by polls so a broken counter cannot hang startup.
fn wait_for_secondary(id: usize, deadline: StartupDeadline, start: u64) -> Result<(), BootError> {
    let record = startup(id, GENERATION).ok_or(BootError::InvalidCheckpoint)?;
    let result = secondary_boot::wait_for_parked(
        &BOOT_STATE,
        record,
        id,
        GENERATION,
        deadline.ticks(),
        100_000_000,
        || deadline.elapsed(start, physical_counter()),
        crate::pcpu::has_fatal,
    );
    #[cfg(any(
        feature = "secondary-startup-timeout-test",
        feature = "all-core-timeout-test"
    ))]
    tvisor_util::println!(
        "Phase 11.3 timeout-test wait elapsed ticks={} budget={}",
        deadline.elapsed(start, physical_counter()),
        deadline.ticks()
    );
    tvisor_util::diagnostic_println!(
        "Phase 11.3 generation={} wait={:?} state={:?}",
        GENERATION,
        result,
        record.mailbox.state(GENERATION)
    );
    if matches!(
        record.mailbox.state(GENERATION),
        Ok(CpuState::Parked | CpuState::Failed)
    ) && (cfg!(feature = "diagnostics") || result.is_err())
    {
        report_checkpoint(record);
    }
    result
}

fn report_checkpoint(record: StartupView<'_>) {
    let c = &record.checkpoint;
    tvisor_util::println!(
        "Phase 11.3 CPU{} checkpoint generation={} EL={:#x} MPIDR={:#x} SP={:#x} TPIDR={:#x} VBAR={:#x}",
        record.identity.logical_id,
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
// Firmware-to-tvisor entry contract for the supported Raspberry Pi 4 Model B backend:
//   Exception level: AArch64 EL2.
//   MMU: disabled (SCTLR_EL2.M = 0).
//   Data cache: disabled (SCTLR_EL2.C = 0).
//   Data endianness: little-endian (SCTLR_EL2.EE = 0).
//   CPU coherency: established by the platform/firmware contract before entry.
//   Entry address: physical address published in the spin-table release cell.
// EL2 and M/C/EE are checked below before record reads or stack installation.
// Cortex-A72 identity precedes the SMPEN check; SMPEN alone does not prove or
// establish coherency. Firmware must provide a clean data-cache history and
// safe instruction fetch; an enabled I-cache must not contain stale entry code.
// CPU0 validates/publishes the physical entry and all startup resources before
// release. No firmware stack or argument-register contents are relied upon.
// Unsupported state enters the holding loop without ordinary shared Rust access.
// See docs/architecture/platform.md and docs/architecture/cpu.md.
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
    // S3_1_C15_C2_1 is the encoded system-register name used for Cortex-A72 CPUECTLR_EL1
    mrs x9, S3_1_C15_C2_1
    mov x23, x9
    // Bit 6 is SMPEN
    tbz x9, #6, .Lsecondary_reject
    mrs x9, mpidr_el1
    mov x24, x9
    // Construct the full affinity mask without a literal-pool read.
    movz x10, #0xffff
    movk x10, #0x00ff, lsl #16
    movk x10, #0x00ff, lsl #32
    and x14, x9, x10
    // Scan the fixed slot array; never use MPIDR as an array index.
    adrp x15, PCPU_SLOTS
    add x15, x15, :lo12:PCPU_SLOTS
    mov x16, #0
    mov x19, xzr
    .global __secondary_lookup
__secondary_lookup:
    ldr x10, [x15, #{present}]
    cmp x10, #1
    b.ne .Lsecondary_next_slot
    ldr x10, [x15, #{affinity}]
    cmp x10, x14
    b.ne .Lsecondary_next_slot
    cbnz x19, .Lsecondary_reject
    mov x19, x15
    mov x18, x16
.Lsecondary_next_slot:
    add x15, x15, #{slot_size}
    add x16, x16, #1
    cmp x16, #{max_cpus}
    b.lo __secondary_lookup
    cbz x19, .Lsecondary_reject
    // Context begins at slot offset zero. The matched slot index must agree
    // with its published logical ID; a secondary can never use the boot slot.
    ldr x9, [x19, #{logical_id}]
    cbz x9, .Lsecondary_reject
    cmp x9, x18
    b.ne .Lsecondary_reject
    // Publication generation only; lifecycle and wait policy are separate.
    ldr x20, [x19, #{mailbox}]
    cbz x20, .Lsecondary_reject
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
    mov x12, x19
    cbz x12, .Lsecondary_reject
    tst x12, #63
    b.ne .Lsecondary_reject
    adrp x17, HOST_EL2_CONFIG
    add x17, x17, :lo12:HOST_EL2_CONFIG
    ldr x10, [x17, #{root}]
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
    ldr x9, [x17, #{mair}]
    msr mair_el2, x9
    ldr x9, [x17, #{tcr}]
    msr tcr_el2, x9
    ldr x9, [x17, #{root}]
    msr ttbr0_el2, x9
    isb
    dsb sy
    tlbi alle2
    ic iallu
    dsb sy
    isb
    ldr x9, [x17, #{sctlr}]
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
    mov x5, x20
    bl secondary_main
.Lsecondary_reject:
    .global __secondary_rejected
__secondary_rejected:
    wfe
    b .Lsecondary_reject
    .size __secondary_entry, . - __secondary_entry
"#),
    host_hcr_lo = const (tvisor_util::el2_policy::HOST_HCR & 0xffff),
    host_hcr_hi = const (tvisor_util::el2_policy::HOST_HCR >> 16),
    host_cptr = const tvisor_util::el2_policy::HOST_CPTR,
    host_hstr = const tvisor_util::el2_policy::HOST_HSTR,
    host_vttbr = const tvisor_util::el2_policy::HOST_VTTBR,
    host_cntvoff = const tvisor_util::el2_policy::HOST_CNTVOFF,
    host_cnthctl = const tvisor_util::el2_policy::HOST_CNTHCTL,
    host_timer = const tvisor_util::el2_policy::DISABLED_TIMER_CTL,
    logical_id = const core::mem::offset_of!(PcpuContext<()>, logical_id),
    stack_bottom = const core::mem::offset_of!(PcpuContext<()>, stack_bottom),
    stack_top = const core::mem::offset_of!(PcpuContext<()>, stack_top),
    affinity = const core::mem::offset_of!(PcpuContext<()>, affinity),
    present = const core::mem::offset_of!(PcpuContext<()>, present),
    max_cpus = const tvisor_util::cpu_topology::MAX_CPUS,
    slot_size = const core::mem::size_of::<PcpuSlot<()>>(),
    stack_size = const tvisor_util::pcpu::STACK_SIZE,
    mair = const core::mem::offset_of!(HostEl2Config, mair),
    tcr = const core::mem::offset_of!(HostEl2Config, tcr),
    root = const core::mem::offset_of!(HostEl2Config, root),
    sctlr = const core::mem::offset_of!(HostEl2Config, sctlr),
    mailbox = const core::mem::offset_of!(PcpuSlot<()>, startup),
);

#[unsafe(no_mangle)]
extern "C" fn secondary_main(
    slot_pointer: *const PcpuSlot<crate::vcpu::Vcpu>,
    entry_sctlr: u64,
    midr: u64,
    cpuectlr: u64,
    mpidr: u64,
    generation: u64,
) -> ! {
    // Assembly acquired the matched slot. Validate the pointer against static
    // storage before obtaining any Rust reference, then acquire the same token.
    let Some(slot) = crate::pcpu::PCPU_SLOTS
        .iter()
        .find(|slot| core::ptr::eq(*slot, slot_pointer))
    else {
        tvisor_util::halt();
    };
    let Some(config) = HOST_EL2_CONFIG.get() else {
        tvisor_util::halt();
    };
    let Some(record) = slot.startup_view(generation, config) else {
        tvisor_util::halt();
    };
    let entry_claim = match record.mailbox.claim_entry(record.generation) {
        Ok(claim) => claim,
        Err(_) => tvisor_util::halt(),
    };
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
        record.identity.logical_id,
        record.identity.affinity,
        record.identity.stack_bottom,
        record.identity.stack_top,
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
        #[cfg(any(
            feature = "secondary-startup-timeout-test",
            feature = "all-core-timeout-test"
        ))]
        if record.identity.logical_id as usize == timeout_test::TEST_CPU {
            timeout_test::delay_parking();
        }
        let _ = entry_claim.park();
    } else {
        let _ = entry_claim.fail();
    }
    if failures.0 == 0 {
        crate::management::worker(record.identity.logical_id as usize, generation);
    }
    tvisor_util::halt();
}

/// One linear, one-shot boot procedure. No release occurs before every slot's
/// preparation, mapping validation/freeze and cache publication has completed.
pub fn start_all(
    fdt: dtoolkit::fdt::Fdt<'_>,
    policy: tvisor_util::el2_policy::HostTranslation,
) -> Result<(), BootError> {
    secondary_boot::begin_boot_once(&BOOT_STATE)?;
    let result = (|| {
        let (contract, deadline) =
            validate_platform(fdt, policy).map_err(BootError::Preparation)?;
        let required = required_mask();
        #[cfg(feature = "secondary-startup-timeout-test")]
        let required = required & 2;
        if required == 0 {
            return Err(BootError::Preparation("no eligible secondary"));
        }
        let resources = prepare_all_cpus(required, policy).map_err(BootError::Preparation)?;
        validate_and_freeze(&resources).map_err(BootError::Preparation)?;
        let line = cache_line_size();
        publish_all_startups(&resources, contract, line).map_err(BootError::Preparation)?;
        secondary_boot::begin_release(&BOOT_STATE)?;
        release_and_wait(required, contract, deadline, line)?;
        verify_all_parked(required)?;
        Ok(())
    })();
    let result = secondary_boot::finalize_boot(&BOOT_STATE, result);
    #[cfg(any(
        feature = "secondary-startup-timeout-test",
        feature = "all-core-timeout-test"
    ))]
    if result.is_err() {
        timeout_test::observe_terminal_failure();
    }
    result
}
fn validate_platform(
    fdt: dtoolkit::fdt::Fdt<'_>,
    policy: tvisor_util::el2_policy::HostTranslation,
) -> Result<(tvisor_util::secondary_entry::EntryContract, StartupDeadline), &'static str> {
    let topology = crate::smp::topology().ok_or("missing CPU topology")?;
    let midr: u64;
    let mpidr: u64;
    unsafe {
        core::arch::asm!("mrs {}, midr_el1", out(reg) midr, options(nomem, nostack, preserves_flags));
        core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
    }
    let backend =
        tvisor_util::secondary_backend::Bcm2711SpinTableBackend::select(fdt, topology, midr, mpidr)
            .map_err(|error| {
                tvisor_util::println!("Phase 11.3 backend rejected: {:?}", error);
                "unsupported Raspberry Pi 4 Model B backend profile"
            })?;
    let entry_contract = backend.contract();
    #[cfg(any(
        feature = "secondary-startup-timeout-test",
        feature = "all-core-timeout-test"
    ))]
    tvisor_util::println!(
        "Phase 11.3 TEST ONLY: secondary-startup-timeout-test; parking delayed 4 seconds"
    );
    tvisor_util::diagnostic_println!(
        "Phase 11.3 backend=bcm2711-armstub MIDR={:#x} MPIDR={:#x}",
        midr,
        mpidr
    );
    crate::pcpu::assert_boot_cpu_idle();
    if !entry_contract.el2_mmu_off || !entry_contract.coherent {
        return Err("unverified entry contract");
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

    let frequency = counter_frequency();
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
    Ok((entry_contract, deadline))
}
/// Boot-local validation inputs, constructed once from canonical state. The
/// descriptors own no memory and never become additional persistent CPU state.
fn prepare_all_cpus(
    required: u64,
    policy: tvisor_util::el2_policy::HostTranslation,
) -> Result<[Option<StartupResources>; MAX_CPUS], &'static str> {
    HOST_EL2_CONFIG.publish(policy)?;
    let config = HOST_EL2_CONFIG.get().ok_or("host config unavailable")?;
    let topology = crate::smp::topology().ok_or("missing topology")?;
    let mut resources = [None; MAX_CPUS];
    for (id, resource) in resources.iter_mut().enumerate().skip(1) {
        if required & (1 << id) == 0 {
            continue;
        }
        let address = tvisor_util::secondary_entry::validate_spin_table_target(topology.cpus(), id)
            .map_err(|_| "invalid target")?;
        crate::mm::map_firmware_release_cell(tvisor_util::system_info::PhysAddr::new(address))
            .map_err(|_| "release mapping failed")?;
        *resource = Some(startup_resources(id, policy)?);
        crate::pcpu::slot(id)
            .ok_or("missing slot")?
            .publish_startup(GENERATION, config)?;
    }
    Ok(resources)
}
fn validate_and_freeze(
    resources: &[Option<StartupResources>; MAX_CPUS],
) -> Result<(), &'static str> {
    let stacks = crate::pcpu::prepared_stacks()?;
    crate::mm::validate_and_freeze_startups(|tables| {
        for resource in resources.iter().flatten() {
            tvisor_util::startup_resources::validate_resources(tables, resource)?;
        }
        tvisor_util::startup_resources::validate_stacks(tables, &stacks)?;
        Ok(())
    })
    .map_err(|_| "startup resource validation/freeze failed")
}
fn cache_line_size() -> u64 {
    let ctr: u64;
    unsafe {
        core::arch::asm!("mrs {}, ctr_el0",out(reg) ctr,options(nomem,nostack,preserves_flags));
    }
    4u64 << ((ctr >> 16) & 15)
}
fn publish_all_startups(
    resources: &[Option<StartupResources>; MAX_CPUS],
    contract: tvisor_util::secondary_entry::EntryContract,
    line: u64,
) -> Result<(), &'static str> {
    use tvisor_util::{
        secondary_hardware::publication_barrier,
        startup_publication::{
            publish_cpu_startup, publish_shared_startup, shared_regions, validate_publication,
        },
    };
    if boot_status() != BootStatus::Preparing || !crate::mm::startup_tables_frozen() {
        return Err("publication outside frozen preparation");
    }
    let topology = crate::smp::topology().ok_or("missing topology")?;
    let shared = resources
        .iter()
        .flatten()
        .next()
        .ok_or("no startup resources")?;
    // Every identity and rounded cache range is checked before any maintenance.
    // All members must name the same shared regions; slots and config are
    // validated subregions of writable, so its single clean covers them too.
    for (id, resource) in resources.iter().enumerate() {
        let Some(resource) = resource else {
            continue;
        };
        if shared_regions(resource) != shared_regions(shared) {
            return Err("inconsistent shared startup regions");
        }
        validate_publication(
            topology.cpus(),
            id,
            contract,
            startup(id, GENERATION).ok_or("missing startup")?,
            resource,
            line,
        )
        .map_err(|_| "invalid publication")?;
    }
    // SAFETY: all mappings are validated/frozen and every generation is already
    // published. CPU0 remains the sole executor until the final barrier below.
    unsafe {
        publish_shared_startup(shared, line).map_err(|_| "shared publication failed")?;
        for resource in resources.iter().flatten() {
            publish_cpu_startup(resource.stack, line).map_err(|_| "stack publication failed")?;
        }
        publication_barrier().map_err(|_| "publication completion failed")?;
    }
    Ok(())
}
fn release_and_wait(
    required: u64,
    contract: tvisor_util::secondary_entry::EntryContract,
    deadline: StartupDeadline,
    line: u64,
) -> Result<(), BootError> {
    if boot_status() != BootStatus::Releasing || !crate::mm::startup_tables_frozen() {
        return Err(BootError::InvalidTransition);
    }
    let topology = crate::smp::topology().ok_or(BootError::InvalidCheckpoint)?;
    unsafe extern "C" {
        fn __secondary_entry();
    }
    let entry = __secondary_entry as *const () as u64;
    for id in 1..MAX_CPUS {
        if required & (1 << id) == 0 {
            continue;
        }
        if crate::pcpu::has_fatal() {
            return Err(BootError::Fatal);
        }
        let record = startup(id, GENERATION).ok_or(BootError::InvalidCheckpoint)?;
        // SAFETY: Releasing follows all publications; mappings remain frozen.
        // This direct procedure records the potential attempt before STR.
        let start = unsafe {
            tvisor_util::startup_publication::release_secondary(
                topology.cpus(),
                contract,
                record,
                entry,
                line,
                &RELEASE_ATTEMPTED,
            )
        }
        .map_err(BootError::Release)?;
        tvisor_util::diagnostic_println!(
            "Phase 11.5 requested CPU{} generation=1 released_mask={:#x}",
            id,
            RELEASE_ATTEMPTED.load(Ordering::Acquire)
        );
        wait_for_secondary(id, deadline, start)?;
        crate::smp::verify_release_set(RELEASE_ATTEMPTED.load(Ordering::Acquire), entry)
            .map_err(|_| BootError::Readback)?;
    }
    Ok(())
}
fn verify_all_parked(required: u64) -> Result<(), BootError> {
    if RELEASE_ATTEMPTED.load(Ordering::Acquire) != required {
        return Err(BootError::InvalidCheckpoint);
    }
    secondary_boot::verify_all_parked(
        &BOOT_STATE,
        required,
        GENERATION,
        (1..MAX_CPUS)
            .filter(|id| required & (1 << id) != 0)
            .filter_map(|id| startup(id, GENERATION).map(|record| (id, record))),
        crate::pcpu::has_fatal(),
    )
}
pub fn ensure_guest_authorized() -> Result<(), BootError> {
    secondary_boot::guest_authorized(&BOOT_STATE, crate::pcpu::has_fatal())
}

fn startup_resources(
    id: usize,
    policy: tvisor_util::el2_policy::HostTranslation,
) -> Result<tvisor_util::startup_resources::StartupResources, &'static str> {
    use tvisor_util::{secondary_entry::validate_spin_table_target, system_info::PhysAddr};
    let topology = crate::smp::topology().ok_or("missing topology")?;
    let address =
        validate_spin_table_target(topology.cpus(), id).map_err(|_| "invalid release target")?;
    let slot = crate::pcpu::slot(id).ok_or("secondary slot unavailable")?;
    let identity = slot.identity().ok_or("secondary resources unavailable")?;
    if !tvisor_util::pcpu::find_slot_by_affinity(&crate::pcpu::PCPU_SLOTS, identity.affinity)
        .is_some_and(|matched| core::ptr::eq(matched, slot))
    {
        return Err("secondary affinity is not uniquely routed to its slot");
    }
    let (affinity, stack_bottom, stack_top, context) = (
        identity.affinity,
        identity.stack_bottom,
        identity.stack_top,
        identity.context,
    );
    if !topology
        .cpus()
        .iter()
        .any(|cpu| cpu.enabled && cpu.logical_id == Some(id) && cpu.affinity == affinity)
    {
        return Err("prepared secondary affinity differs from topology");
    }
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
        slots: PhysRegion::new(
            PhysAddr::new(core::ptr::addr_of!(crate::pcpu::PCPU_SLOTS) as u64),
            core::mem::size_of_val(&crate::pcpu::PCPU_SLOTS) as u64,
        )
        .map_err(|_| "invalid slot array region")?,
        config: PhysRegion::new(
            PhysAddr::new(core::ptr::addr_of!(HOST_EL2_CONFIG) as u64),
            core::mem::size_of::<HostEl2ConfigStorage>() as u64,
        )
        .map_err(|_| "invalid shared host config region")?,
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
    Ok(resources)
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
