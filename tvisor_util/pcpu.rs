//! Physical-CPU layout and startup metadata shared with assembly and host tests.
use crate::{
    PAGE_SIZE,
    cpu_topology::{CpuTopology, MAX_CPUS},
    el2_stage1_translation::{Mapping, MemoryType, TableSet, TranslationError},
    page_allocator::{AllocatorError, PageAllocator},
    system_info::{FixedList, PhysAddr},
};
use crate::{
    el2_policy::HostEl2Config,
    secondary_startup::{SecondaryStartup, StartupView},
};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

/// Acquired snapshot of immutable context fields, never persisted as startup storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuIdentity {
    pub logical_id: u64,
    pub affinity: u64,
    pub stack_bottom: u64,
    pub stack_top: u64,
    pub context: u64,
}

#[repr(C, align(64))]
pub struct PcpuSlot<V> {
    context: UnsafeCell<PcpuContext<V>>,
    pub startup: SecondaryStartup,
    prepared: AtomicBool,
    claimed: AtomicBool,
}
// SAFETY: context initialization is claimed once; identity/bounds are then frozen.
// Only the owning CPU may access mutable host_sp/active_vcpu via the raw pointer.
// Shared Rust APIs reference only atomics and individually read immutable fields.
unsafe impl<V> Sync for PcpuSlot<V> {}
impl<V> PcpuSlot<V> {
    pub const fn new() -> Self {
        Self {
            context: UnsafeCell::new(PcpuContext::empty()),
            startup: SecondaryStartup::new(),
            prepared: AtomicBool::new(false),
            claimed: AtomicBool::new(false),
        }
    }
    pub fn prepare(
        &self,
        id: usize,
        affinity: u64,
        stack: StackLayout,
    ) -> Result<(), &'static str> {
        if id >= MAX_CPUS
            || affinity & !crate::cpu_topology::MPIDR_AFFINITY_MASK != 0
            || stack.bottom == 0
            || stack_layout(stack.guard_start).ok() != Some(stack)
        {
            return Err("invalid CPU preparation");
        }
        if self.claimed.swap(true, Ordering::AcqRel) {
            return Err("CPU slot already prepared");
        }
        unsafe {
            self.context.get().write(PcpuContext {
                host_sp: 0,
                active_vcpu: core::ptr::null_mut(),
                logical_id: id as u64,
                affinity,
                stack_bottom: stack.bottom,
                stack_top: stack.top,
                present: 1,
            });
        }
        self.prepared.store(true, Ordering::Release);
        Ok(())
    }
    pub fn is_prepared(&self) -> bool {
        self.prepared.load(Ordering::Acquire)
    }
    pub fn context_ptr(&self) -> *mut PcpuContext<V> {
        self.context.get()
    }
    pub fn identity(&self) -> Option<CpuIdentity> {
        if !self.is_prepared() {
            return None;
        }
        // No reference to the whole context: its owner can mutate other fields.
        let c = self.context.get();
        Some(unsafe {
            CpuIdentity {
                logical_id: core::ptr::addr_of!((*c).logical_id).read(),
                affinity: core::ptr::addr_of!((*c).affinity).read(),
                stack_bottom: core::ptr::addr_of!((*c).stack_bottom).read(),
                stack_top: core::ptr::addr_of!((*c).stack_top).read(),
                context: c as u64,
            }
        })
    }
    pub fn publish_startup(
        &self,
        generation: u64,
        config: &HostEl2Config,
    ) -> Result<(), &'static str> {
        let identity = self.identity().ok_or("CPU slot not prepared")?;
        if identity.logical_id == 0
            || config.mair != crate::el2_policy::HOST_MAIR
            || config.sctlr != crate::el2_policy::HOST_SCTLR
            || !crate::el2_policy::HostTranslation::new(config.root, ((config.tcr >> 16) & 7) as u8)
                .is_ok_and(|p| p.tcr() == config.tcr)
        {
            return Err("invalid startup configuration");
        }
        self.startup
            .mailbox
            .publish(generation)
            .map_err(|_| "startup already published or invalid generation")
    }
    pub fn startup_view<'a>(
        &'a self,
        generation: u64,
        config: &'a HostEl2Config,
    ) -> Option<StartupView<'a>> {
        if !self.startup.mailbox.observe_ready(generation) {
            return None;
        }
        Some(StartupView::new(
            self.identity()?,
            config,
            generation,
            &self.startup,
            self as *const _ as u64,
        ))
    }
}
const _: () = {
    assert!(core::mem::offset_of!(PcpuSlot<()>, context) == 0);
    assert!(core::mem::offset_of!(PcpuSlot<()>, startup) == 64);
    assert!(core::mem::size_of::<PcpuSlot<()>>() == 384);
    assert!(core::mem::align_of::<PcpuSlot<()>>() == 64);
    assert!(core::mem::offset_of!(PcpuContext<()>, logical_id) == 16);
    assert!(core::mem::offset_of!(PcpuContext<()>, affinity) == 24);
    assert!(core::mem::offset_of!(PcpuContext<()>, stack_bottom) == 32);
    assert!(core::mem::offset_of!(PcpuContext<()>, stack_top) == 40);
    assert!(core::mem::offset_of!(PcpuContext<()>, present) == 48);
};

/// Bounded lookup over immutable prepared identities, mirroring entry assembly.
/// Full affinities may be sparse; duplicate matches are never routed arbitrarily.
pub fn find_slot_by_affinity<V>(slots: &[PcpuSlot<V>], mpidr: u64) -> Option<&PcpuSlot<V>> {
    if slots.len() > MAX_CPUS {
        return None;
    }
    let affinity = mpidr & crate::cpu_topology::MPIDR_AFFINITY_MASK;
    let mut found = None;
    for slot in slots {
        if slot
            .identity()
            .is_some_and(|identity| identity.affinity == affinity)
        {
            if found.is_some() {
                return None;
            }
            found = Some(slot);
        }
    }
    found
}

pub const STACK_SIZE: u64 = 64 * 1024;
pub const GUARD_SIZE: u64 = PAGE_SIZE as u64;
pub const STACK_ALLOCATION_SIZE: u64 = GUARD_SIZE + STACK_SIZE;
pub const STACK_ALLOCATION_PAGES: usize = (STACK_ALLOCATION_SIZE / PAGE_SIZE as u64) as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackLayout {
    pub guard_start: u64,
    pub bottom: u64,
    pub top: u64,
}
pub fn stack_layout(base: u64) -> Result<StackLayout, TranslationError> {
    if base & (PAGE_SIZE as u64 - 1) != 0 {
        return Err(TranslationError::InvalidateParameter);
    }
    let guard_start = base;
    let bottom = guard_start
        .checked_add(GUARD_SIZE)
        .ok_or(TranslationError::AddressOverflow)?;
    let top = bottom
        .checked_add(STACK_SIZE)
        .ok_or(TranslationError::AddressOverflow)?;
    Ok(StackLayout {
        guard_start,
        bottom,
        top,
    })
}
pub fn map_stack<const N: usize>(
    tables: &mut TableSet<'_, N>,
    stack: StackLayout,
) -> Result<(), TranslationError> {
    tables.map(Mapping {
        va: stack.bottom,
        pa: stack.bottom,
        size: STACK_SIZE,
        memory_type: MemoryType::Normal,
        writable: true,
        executable: false,
    })?;
    validate_stack(tables, stack)
}
pub fn validate_stack<const N: usize>(
    tables: &TableSet<'_, N>,
    stack: StackLayout,
) -> Result<(), TranslationError> {
    for address in (stack.guard_start..stack.bottom).step_by(PAGE_SIZE) {
        if tables.walk(address)?.is_some() {
            return Err(TranslationError::Unexpected);
        }
    }
    for address in (stack.bottom..stack.top)
        .step_by(PAGE_SIZE)
        .chain(core::iter::once(stack.top - 1))
    {
        let mapping = tables.walk(address)?.ok_or(TranslationError::Unexpected)?;
        if mapping.pa != address
            || !mapping.writable
            || mapping.executable
            || mapping.memory_type != MemoryType::Normal
        {
            return Err(TranslationError::Unexpected);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecondaryStack {
    pub logical_id: usize,
    pub affinity: u64,
    pub layout: StackLayout,
}

/// Allocate only enabled secondary CPUs. Allocation touches bitmap metadata,
/// not stack RAM, so it is safe before broad EL2 RAM mappings exist. Roll back
/// a partial allocation failure before any guard is excluded or CPU published.
pub fn allocate_secondary_stacks<const B: usize>(
    topology: &CpuTopology,
    allocator: &mut PageAllocator<'_, B>,
) -> Result<FixedList<SecondaryStack, MAX_CPUS>, AllocatorError> {
    let mut stacks = FixedList::new();
    let result = (|| {
        for cpu in topology
            .cpus()
            .iter()
            .filter(|cpu| cpu.enabled && cpu.logical_id != Some(0))
        {
            let id = cpu.logical_id.ok_or(AllocatorError::InvalidateParameter)?;
            let base = allocator.allocate_contiguous(STACK_ALLOCATION_PAGES)?;
            // Allocator pages are aligned and bounded by its finite aperture.
            let layout =
                stack_layout(base.value()).expect("allocator returned invalid stack range");
            stacks
                .push(SecondaryStack {
                    logical_id: id,
                    affinity: cpu.affinity,
                    layout,
                })
                .expect("topology capacity exceeded");
        }
        Ok(())
    })();
    if let Err(error) = result {
        for stack in &stacks {
            for address in (stack.layout.guard_start..stack.layout.top).step_by(PAGE_SIZE) {
                allocator.free(PhysAddr::new(address))?;
            }
        }
        return Err(error);
    }
    Ok(stacks)
}

/// Owner-only mutable world-switch context. Not Sync: other CPUs may not touch
/// host_sp/active_vcpu. Identity and bounds are immutable after preparation.
#[repr(C, align(64))]
pub struct PcpuContext<V> {
    pub host_sp: u64,
    pub active_vcpu: *mut V,
    pub logical_id: u64,
    pub affinity: u64,
    pub stack_bottom: u64,
    pub stack_top: u64,
    pub present: u64,
}
impl<V> PcpuContext<V> {
    /// Owner-CPU validation only: never borrow another CPU's mutable context.
    pub fn matches_secondary(&self, id: u64, affinity: u64, bottom: u64, top: u64) -> bool {
        id > 0
            && id < MAX_CPUS as u64
            && self.present == 1
            && self.logical_id == id
            && self.affinity == affinity
            && self.stack_bottom == bottom
            && self.stack_top == top
            && self.host_sp == 0
            && self.active_vcpu.is_null()
    }
    pub const fn empty() -> Self {
        Self {
            host_sp: 0,
            active_vcpu: core::ptr::null_mut(),
            logical_id: 0,
            affinity: 0,
            stack_bottom: 0,
            stack_top: 0,
            present: 0,
        }
    }
}
const _: () = assert!(core::mem::size_of::<PcpuContext<()>>() == 64);
const _: () = assert!(core::mem::align_of::<PcpuContext<()>>() == 64);
const _: () = assert!(core::mem::offset_of!(PcpuContext<()>, host_sp) == 0);
const _: () = assert!(core::mem::offset_of!(PcpuContext<()>, active_vcpu) == 8);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::el2_stage1_translation::TableStorage;
    #[test]
    fn secondary_context_validation_rejects_identity_and_guest_ownership_mismatches() {
        for fault in 0..9 {
            let mut c = PcpuContext::<()>::empty();
            c.present = 1;
            c.logical_id = 1;
            c.affinity = 0x100;
            c.stack_bottom = 0x800000;
            c.stack_top = 0x810000;
            match fault {
                1 => c.present = 0,
                2 => c.logical_id = 0,
                3 => c.affinity = 1,
                4 => c.stack_bottom += 4096,
                5 => c.stack_top += 4096,
                6 => c.host_sp = 0x80ff00,
                7 => c.active_vcpu = core::ptr::dangling_mut(),
                8 => c.logical_id = MAX_CPUS as u64,
                _ => {}
            }
            assert_eq!(
                c.matches_secondary(1, 0x100, 0x800000, 0x810000),
                fault == 0
            );
        }
    }
    #[test]
    fn guarded_stack_layout_checks_alignment_and_overflow() {
        for base in [0, 0x4100000, 0x8000000] {
            let s = stack_layout(base).unwrap();
            assert_eq!(s.guard_start, base);
            assert_eq!(s.bottom - base, 4096);
            assert_eq!(s.top - s.bottom, 65536);
            assert_eq!(s.top & 15, 0);
        }
        assert!(stack_layout(1).is_err());
        assert!(stack_layout(u64::MAX & !4095).is_err());
    }
    #[test]
    fn separate_stack_mappings_preserve_guards_and_reject_guard_remapping() {
        let mut storage = TableStorage::<16>::zeroed();
        let mut tables = TableSet::new(&mut storage, 0x8000000, 44).unwrap();
        for base in [0x41f0000, 0x4500000, 0x6010000] {
            let s = stack_layout(base).unwrap();
            map_stack(&mut tables, s).unwrap();
            validate_stack(&tables, s).unwrap();
        }
        let s = stack_layout(0x4500000).unwrap();
        tables
            .map(Mapping {
                va: s.guard_start,
                pa: s.guard_start,
                size: GUARD_SIZE,
                memory_type: MemoryType::Normal,
                writable: true,
                executable: false,
            })
            .unwrap();
        assert!(validate_stack(&tables, s).is_err());
    }
    #[test]
    fn insufficient_table_storage_fails_explicitly() {
        let mut storage = TableStorage::<2>::zeroed();
        let mut tables = TableSet::new(&mut storage, 0x8000000, 44).unwrap();
        assert_eq!(
            map_stack(&mut tables, stack_layout(0x4100000).unwrap()),
            Err(TranslationError::TableExhausted)
        );
    }
    #[test]
    fn context_starts_without_an_active_vcpu_and_startup_is_not_online() {
        let context = PcpuContext::<()>::empty();
        assert!(context.active_vcpu.is_null());
        assert_eq!(context.host_sp, 0);
        let state = PcpuSlot::<()>::new();
        assert!(!state.is_prepared());
        let layout = stack_layout(0x7ff000).unwrap();
        state.prepare(1, 0x100, layout).unwrap();
        assert!(state.is_prepared());
        assert!(state.prepare(1, 0x100, layout).is_err());
    }
    fn topology(enabled: &[bool]) -> CpuTopology {
        use dtoolkit::{
            fdt::Fdt,
            model::{DeviceTree, DeviceTreeNode, DeviceTreeProperty},
        };
        let mut tree = DeviceTree::new();
        let mut cpus = DeviceTreeNode::new_unchecked("cpus");
        cpus.add_property(DeviceTreeProperty::new_unchecked(
            "#address-cells",
            1u32.to_be_bytes().to_vec(),
        ));
        cpus.add_property(DeviceTreeProperty::new_unchecked(
            "#size-cells",
            0u32.to_be_bytes().to_vec(),
        ));
        for (id, enabled) in enabled.iter().enumerate() {
            let mut cpu = DeviceTreeNode::new_unchecked(alloc::format!("cpu@{id}"));
            cpu.add_property(DeviceTreeProperty::new_unchecked(
                "device_type",
                b"cpu\0".to_vec(),
            ));
            cpu.add_property(DeviceTreeProperty::new_unchecked(
                "reg",
                (id as u32).to_be_bytes().to_vec(),
            ));
            if !enabled {
                cpu.add_property(DeviceTreeProperty::new_unchecked(
                    "status",
                    b"disabled\0".to_vec(),
                ));
            }
            cpus.add_child(cpu);
        }
        tree.root.add_child(cpus);
        let bytes = tree.to_dtb();
        crate::cpu_topology::discover_cpu_topology(Fdt::new(&bytes).unwrap(), 0).unwrap()
    }
    #[test]
    fn allocates_only_enabled_secondaries_and_retains_every_guard_page() {
        use crate::{
            page_allocator::{PageBitmap, PageState},
            system_info::PhysRegion,
        };
        let mut ram = FixedList::<_, 1>::new();
        ram.push(PhysRegion::new(PhysAddr::new(0), 64 * 4096).unwrap())
            .unwrap();
        let mut usable = FixedList::<_, 1>::new();
        usable
            .push(PhysRegion::new(PhysAddr::new(4096), 63 * 4096).unwrap())
            .unwrap();
        let (mut managed, mut in_use) = (PageBitmap::<8>::zeroed(), PageBitmap::<8>::zeroed());
        let mut allocator =
            PageAllocator::new(&ram, &usable, &usable, &mut managed, &mut in_use).unwrap();
        let stacks =
            allocate_secondary_stacks(&topology(&[true, false, true, true]), &mut allocator)
                .unwrap();
        assert_eq!(stacks.len(), 2);
        assert_eq!(allocator.stats().in_use_pages, 2 * STACK_ALLOCATION_PAGES);
        assert_eq!(stacks.get(0).unwrap().affinity, 2);
        assert_eq!(stacks.get(0).unwrap().logical_id, 1);
        assert_eq!(stacks.get(1).unwrap().affinity, 3);
        assert!(stacks.get(0).unwrap().layout.top <= stacks.get(1).unwrap().layout.guard_start);
        for stack in &stacks {
            for address in (stack.layout.guard_start..stack.layout.top).step_by(PAGE_SIZE) {
                assert_eq!(
                    allocator.state(PhysAddr::new(address)),
                    Ok(PageState::InUse)
                );
            }
        }
    }
    #[test]
    fn boot_only_topology_does_not_allocate_any_secondary_stack() {
        use crate::{page_allocator::PageBitmap, system_info::PhysRegion};
        let mut ram = FixedList::<_, 1>::new();
        ram.push(PhysRegion::new(PhysAddr::new(0), 4096).unwrap())
            .unwrap();
        let (mut managed, mut in_use) = (PageBitmap::<1>::zeroed(), PageBitmap::<1>::zeroed());
        let mut allocator =
            PageAllocator::new(&ram, &ram, &ram, &mut managed, &mut in_use).unwrap();
        let before = allocator.stats();
        assert!(
            allocate_secondary_stacks(&topology(&[true, false, false, false]), &mut allocator)
                .unwrap()
                .is_empty()
        );
        assert_eq!(allocator.stats(), before);
    }
    #[test]
    fn partial_allocation_failure_rolls_back_before_mapping_or_publication() {
        use crate::{page_allocator::PageBitmap, system_info::PhysRegion};
        let mut ram = FixedList::<_, 1>::new();
        ram.push(PhysRegion::new(PhysAddr::new(0), 20 * 4096).unwrap())
            .unwrap();
        let (mut managed, mut in_use) = (PageBitmap::<4>::zeroed(), PageBitmap::<4>::zeroed());
        let mut allocator =
            PageAllocator::new(&ram, &ram, &ram, &mut managed, &mut in_use).unwrap();
        let before = allocator.stats();
        assert_eq!(
            allocate_secondary_stacks(&topology(&[true, true, true]), &mut allocator),
            Err(AllocatorError::Exhausted)
        );
        assert_eq!(allocator.stats(), before);
        assert!(allocator.allocate_contiguous(20).is_ok());
    }
}
