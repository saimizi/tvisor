//! Stable physical-CPU storage. Phase 11.1 installs only boot CPU TPIDR_EL2.
use crate::vcpu::Vcpu;
use core::{
    arch::asm,
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};
use tvisor_util::{
    aarch64_reg::{MpidrEl1, Sp},
    cpu_topology::{CpuTopology, MAX_CPUS, MPIDR_AFFINITY_MASK},
    pcpu::{CpuPreparation, PcpuContext, STACK_ALLOCATION_SIZE, StackLayout, stack_layout},
    println,
};

type PcpuState = PcpuContext<Vcpu>;
const _: () = assert!(core::mem::size_of::<PcpuState>() == 64);
const _: () = assert!(core::mem::offset_of!(PcpuState, host_sp) == 0);
const _: () = assert!(core::mem::offset_of!(PcpuState, active_vcpu) == 8);

#[repr(C, align(64))]
struct PcpuSlot {
    context: UnsafeCell<PcpuState>,
    preparation: CpuPreparation,
}
// SAFETY: preparation runs once on the boot CPU while every other CPU remains
// in firmware. Context mutation thereafter belongs exclusively to that slot's
// physical CPU via TPIDR_EL2. No Rust references to the mutable context escape;
// world-switch assembly writes only host_sp/active_vcpu. Other CPUs may consume
// published immutable identity/bounds and the separate atomic startup fields,
// never the owner's mutable fields. 11.1 releases no CPUs at all.
unsafe impl Sync for PcpuSlot {}
impl PcpuSlot {
    const fn new() -> Self {
        Self {
            context: UnsafeCell::new(PcpuState::empty()),
            preparation: CpuPreparation::new(),
        }
    }
}
static CPUS: [PcpuSlot; MAX_CPUS] = [const { PcpuSlot::new() }; MAX_CPUS];
static BOOT_PREPARATION_CLAIMED: AtomicBool = AtomicBool::new(false);
unsafe extern "C" {
    static __boot_stack_guard_start: u8;
    static __boot_stack_top: u8;
}
pub fn boot_stack() -> StackLayout {
    stack_layout(core::ptr::addr_of!(__boot_stack_guard_start) as u64)
        .expect("invalid boot stack layout")
}

pub fn prepare_boot_cpu(topology: &CpuTopology) -> Result<(), &'static str> {
    if BOOT_PREPARATION_CLAIMED.swap(true, Ordering::AcqRel) {
        return Err("boot CPU already prepared");
    }
    let stack = boot_stack();
    if (core::ptr::addr_of!(__boot_stack_top) as u64).checked_sub(stack.guard_start)
        != Some(STACK_ALLOCATION_SIZE)
    {
        return Err("linker boot stack size mismatch");
    }
    let boot = topology
        .cpus()
        .iter()
        .find(|cpu| cpu.enabled && cpu.logical_id == Some(0))
        .ok_or("boot CPU absent")?;
    prepare_slot(0, boot.affinity, stack)
}

fn prepare_slot(id: usize, affinity: u64, stack: StackLayout) -> Result<(), &'static str> {
    let slot = CPUS.get(id).ok_or("CPU slot capacity exceeded")?;
    let context = PcpuState {
        host_sp: 0,
        active_vcpu: core::ptr::null_mut(),
        logical_id: id as u64,
        affinity,
        stack_bottom: stack.bottom,
        stack_top: stack.top,
        present: 1,
    };
    // SAFETY: the boot CPU owns one-time preparation; no secondary is released.
    // Bounds are published only after their mappings and guard are established.
    unsafe {
        slot.context.get().write(context);
    }
    slot.preparation
        .publish_prepared()
        .map_err(|_| "CPU slot already published")
}

#[derive(Debug)]
pub enum SecondaryPrepareError {
    Allocator(tvisor_util::page_allocator::AllocatorError),
    Mapping(crate::mm::PrepareError),
    State(&'static str),
}
impl core::fmt::Display for SecondaryPrepareError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Allocator(error) => write!(f, "secondary stack allocation: {error}"),
            Self::Mapping(error) => write!(f, "secondary stack mapping: {error}"),
            Self::State(error) => f.write_str(error),
        }
    }
}
static SECONDARY_PREPARATION_CLAIMED: AtomicBool = AtomicBool::new(false);

/// Physical allocation is ready, but broad RAM mappings have not been installed.
/// Allocate only enabled secondaries and exclude their guards from that first
/// mapping. No live block descriptor must be split or broken/replaced.
pub fn prepare_secondary_cpus(
    memory_map: &tvisor_util::memory_map::MemoryMap,
    live_dtb: tvisor_util::system_info::PhysRegion,
    linux_image: tvisor_util::system_info::PhysRegion,
) -> Result<(), SecondaryPrepareError> {
    assert_boot_cpu_idle();
    if SECONDARY_PREPARATION_CLAIMED.swap(true, Ordering::AcqRel) {
        return Err(SecondaryPrepareError::State(
            "secondary preparation already attempted",
        ));
    }
    let topology =
        crate::smp::topology().ok_or(SecondaryPrepareError::State("CPU topology unavailable"))?;
    let stacks =
        crate::mm::allocate_secondary_stacks(topology).map_err(SecondaryPrepareError::Allocator)?;
    // On mapping failure, retain all allocated ranges and halt boot. They must
    // not be reused beneath a partially installed mapping/preparation attempt.
    crate::mm::map_usable_ram(memory_map, live_dtb, linux_image, &stacks)
        .map_err(SecondaryPrepareError::Mapping)?;
    for stack in &stacks {
        // SAFETY: exclusive physical allocations, now identity-mapped RW/NX;
        // only usable stack bytes are cleared, never the unmapped guard.
        unsafe {
            core::ptr::write_bytes(
                stack.layout.bottom as *mut u8,
                0,
                (stack.layout.top - stack.layout.bottom) as usize,
            );
        }
    }
    // Publish only after all stacks are mapped, validated, zeroed and barriers
    // have completed. Allocation failure rolled back before any mappings.
    for stack in &stacks {
        prepare_slot(stack.logical_id, stack.affinity, stack.layout)
            .map_err(SecondaryPrepareError::State)?;
    }
    println!(
        "Phase 11.1 secondary stack allocations: {} enabled CPUs, {} pages each including guard",
        stacks.len(),
        tvisor_util::pcpu::STACK_ALLOCATION_PAGES
    );
    report_prepared();
    Ok(())
}

pub fn boot_entry() -> (*mut PcpuState, u64) {
    assert!(CPUS[0].preparation.is_prepared());
    let pointer = CPUS[0].context.get();
    // SAFETY: bounds are immutable after release publication above.
    let top = unsafe { (*pointer).stack_top };
    (pointer, top)
}

/// Return immutable startup values for a prepared secondary. Only the boot
/// CPU calls this, before releasing the selected CPU.
pub fn secondary_entry(id: usize) -> Option<(u64, u64, u64)> {
    if id == 0 {
        return None;
    }
    let slot = CPUS.get(id)?;
    if !slot.preparation.is_prepared() {
        return None;
    }
    let ptr = slot.context.get();
    // SAFETY: the boot CPU initialized this slot and no secondary has been
    // released yet. Identity and stack bounds stay immutable after publication.
    unsafe {
        if !(*ptr).matches_secondary(
            id as u64,
            (*ptr).affinity,
            (*ptr).stack_bottom,
            (*ptr).stack_top,
        ) {
            return None;
        }
        Some(((*ptr).affinity, (*ptr).stack_top, ptr as u64))
    }
}

pub fn prepared_stacks()
-> Result<tvisor_util::system_info::FixedList<StackLayout, MAX_CPUS>, &'static str> {
    assert_boot_cpu_idle();
    let mut result = tvisor_util::system_info::FixedList::new();
    for slot in &CPUS {
        if slot.preparation.is_prepared() {
            // SAFETY: boot CPU, before release; only immutable prepared bounds.
            let c = unsafe { &*slot.context.get() };
            let guard = c
                .stack_bottom
                .checked_sub(tvisor_util::pcpu::GUARD_SIZE)
                .ok_or("invalid prepared guard")?;
            let layout = stack_layout(guard).map_err(|_| "invalid prepared stack")?;
            if layout.top != c.stack_top || c.present != 1 {
                return Err("inconsistent prepared stack");
            }
            result
                .push(layout)
                .map_err(|_| "prepared stack capacity exceeded")?;
        }
    }
    Ok(result)
}

/// Called by a secondary only after establishing its published TPIDR and
/// crossing the coherent Rust boundary. The slot is private to that CPU.
pub fn validate_secondary_context(
    id: u64,
    affinity: u64,
    bottom: u64,
    top: u64,
    pointer: u64,
) -> bool {
    let Ok(index) = usize::try_from(id) else {
        return false;
    };
    let Some(slot) = CPUS.get(index) else {
        return false;
    };
    if index == 0 || slot.context.get() as u64 != pointer || !slot.preparation.is_prepared() {
        return false;
    }
    // SAFETY: the pointer matches the selected static slot and this CPU owns
    // the context after release; boot preparation completed before publication.
    let context = unsafe { &*slot.context.get() };
    context.matches_secondary(id, affinity, bottom, top)
}

/// Called only outside a world switch on its owning boot CPU. No references
/// to mutable CPU context persist across guest entry/exit or exception entry.
pub fn assert_boot_cpu_idle() {
    let pointer: u64;
    unsafe {
        asm!("mrs {value}, TPIDR_EL2", value=out(reg) pointer, options(nomem, nostack, preserves_flags));
    }
    assert_eq!(
        pointer,
        CPUS[0].context.get() as u64,
        "wrong boot CPU TPIDR_EL2"
    );
    let state = CPUS[0].context.get();
    let affinity = MpidrEl1::dump().unwrap().value & MPIDR_AFFINITY_MASK;
    // SAFETY: known lifetime-stable slot, owner CPU, no active assembly writer.
    unsafe {
        assert_eq!((*state).logical_id, 0);
        assert_eq!((*state).affinity, affinity);
        assert_eq!((*state).present, 1);
        assert!(
            core::ptr::addr_of!((*state).active_vcpu)
                .read_volatile()
                .is_null()
        );
        let saved_sp = core::ptr::addr_of!((*state).host_sp).read_volatile();
        assert!(
            saved_sp == 0
                || (saved_sp >= (*state).stack_bottom
                    && saved_sp < (*state).stack_top
                    && saved_sp & 15 == 0),
            "saved host SP outside boot CPU stack"
        );
        let sp = Sp::dump().value as u64;
        assert!(
            sp >= (*state).stack_bottom && sp < (*state).stack_top,
            "SP outside boot CPU stack"
        );
    }
}

pub fn report_prepared() {
    assert_boot_cpu_idle();
    for (id, slot) in CPUS.iter().enumerate() {
        // SAFETY: workers are not released in 11.1. Only the boot CPU is active,
        // and this checkpoint is outside the guest world-switch assembly.
        let context = slot.context.get();
        unsafe {
            assert!(
                core::ptr::addr_of!((*context).active_vcpu)
                    .read_volatile()
                    .is_null()
            );
            if slot.preparation.is_prepared() {
                println!(
                    "Phase 11.1 CPU{} prepared: affinity={:#x} context={:#x} stack=[{:#x}, {:#x}) active_vcpu=null{}",
                    id,
                    (*context).affinity,
                    context as u64,
                    (*context).stack_bottom,
                    (*context).stack_top,
                    if id == 0 {
                        " TPIDR_EL2 installed"
                    } else {
                        " (not released)"
                    }
                );
            }
        }
    }
}

// One record per logical CPU plus one for faults before TPIDR installation.
static FATAL: tvisor_util::failure::FatalState<{ MAX_CPUS + 1 }> =
    tvisor_util::failure::FatalState::new();

pub fn record_fatal(failure: tvisor_util::failure::Failure) {
    let pointer: usize;
    unsafe {
        asm!("mrs {}, tpidr_el2", out(reg) pointer, options(nomem, nostack, preserves_flags));
    }
    // Compare addresses only: inherited TPIDR may be unset or arbitrary.
    let cpu = CPUS
        .iter()
        .position(|slot| slot.context.get() as usize == pointer)
        .unwrap_or(MAX_CPUS);
    FATAL.record(cpu, failure);
}

/// Acquired global fatal evidence; no logging or locks.
pub fn has_fatal() -> bool {
    FATAL.fatal.load(Ordering::Acquire)
}
