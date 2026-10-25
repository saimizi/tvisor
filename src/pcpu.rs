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
    pcpu::{CpuStartupState, PcpuContext, STACK_ARENA_SIZE, stack_layout},
    println,
};

type PcpuState = PcpuContext<Vcpu>;
const _: () = assert!(core::mem::size_of::<PcpuState>() == 64);
const _: () = assert!(core::mem::offset_of!(PcpuState, host_sp) == 0);
const _: () = assert!(core::mem::offset_of!(PcpuState, active_vcpu) == 8);

#[repr(C, align(64))]
struct PcpuSlot {
    context: UnsafeCell<PcpuState>,
    startup: CpuStartupState,
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
            startup: CpuStartupState::new(),
        }
    }
}
static CPUS: [PcpuSlot; MAX_CPUS] = [const { PcpuSlot::new() }; MAX_CPUS];
static PREPARATION_CLAIMED: AtomicBool = AtomicBool::new(false);
unsafe extern "C" {
    static __pcpu_stacks_start: u8;
    static __pcpu_stacks_end: u8;
}
pub fn stacks_base() -> u64 {
    core::ptr::addr_of!(__pcpu_stacks_start) as u64
}

pub fn prepare(topology: &CpuTopology) -> Result<(), &'static str> {
    if PREPARATION_CLAIMED.swap(true, Ordering::AcqRel) {
        return Err("per-CPU state already prepared");
    }
    let base = stacks_base();
    if (core::ptr::addr_of!(__pcpu_stacks_end) as u64).checked_sub(base) != Some(STACK_ARENA_SIZE) {
        return Err("linker per-CPU stack arena size mismatch");
    }
    for cpu in topology.cpus().iter().filter(|cpu| cpu.enabled) {
        let id = cpu.logical_id.ok_or("enabled CPU has no logical ID")?;
        let slot = CPUS.get(id).ok_or("CPU slot capacity exceeded")?;
        let stack = stack_layout(base, id).map_err(|_| "invalid per-CPU stack layout")?;
        let context = PcpuState {
            host_sp: 0,
            active_vcpu: core::ptr::null_mut(),
            logical_id: id as u64,
            affinity: cpu.affinity,
            stack_bottom: stack.bottom,
            stack_top: stack.top,
            present: 1,
        };
        // SAFETY: one-time early boot initialization; no CPU can access slots yet.
        unsafe {
            slot.context.get().write(context);
        }
        slot.startup
            .publish_prepared()
            .map_err(|_| "CPU slot already published")?;
    }
    if !CPUS[0].startup.is_prepared() {
        return Err("boot CPU was not prepared");
    }
    Ok(())
}

pub fn boot_entry() -> (*mut PcpuState, u64) {
    assert!(CPUS[0].startup.is_prepared());
    let pointer = CPUS[0].context.get();
    // SAFETY: bounds are immutable after release publication above.
    let top = unsafe { (*pointer).stack_top };
    (pointer, top)
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
        assert!(slot.startup.mailbox_idle());
        // SAFETY: workers are not released in 11.1. Only the boot CPU is active,
        // and this checkpoint is outside the guest world-switch assembly.
        let context = slot.context.get();
        unsafe {
            assert!(
                core::ptr::addr_of!((*context).active_vcpu)
                    .read_volatile()
                    .is_null()
            );
            if slot.startup.is_prepared() {
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
