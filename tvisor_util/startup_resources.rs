//! Boot-owned table gate and production startup-resource validation.
use crate::{
    PAGE_SIZE,
    el2_stage1_translation::{MemoryType, TableSet},
    memory_map::MemoryMap,
    pcpu::{StackLayout, validate_stack},
    system_info::{PhysAddr, PhysRegion},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceError {
    Frozen,
    Layout,
    Ownership,
    Mapping,
    Root,
}

/// Used under the boot table lock. Immutable validation and freezing form one
/// critical section; no mutable table handle can be obtained afterwards.
pub struct BootTables<T> {
    tables: T,
    frozen: bool,
}
impl<T> BootTables<T> {
    pub const fn new(tables: T) -> Self {
        Self {
            tables,
            frozen: false,
        }
    }
    pub fn is_frozen(&self) -> bool {
        self.frozen
    }
    pub fn view(&self) -> &T {
        &self.tables
    }
    pub fn edit(&mut self) -> Result<&mut T, ResourceError> {
        if self.frozen {
            Err(ResourceError::Frozen)
        } else {
            Ok(&mut self.tables)
        }
    }
    pub fn validate_and_freeze(
        &mut self,
        validate: impl FnOnce(&T) -> Result<(), ResourceError>,
    ) -> Result<(), ResourceError> {
        if self.frozen {
            return Err(ResourceError::Frozen);
        }
        validate(&self.tables)?;
        self.frozen = true;
        Ok(())
    }
}
/// Boot-local image and translation description shared by every selected CPU.
/// This owns metadata only, not the underlying mappings or physical resources.
#[derive(Clone, Copy, Debug)]
pub struct SharedStartupResources {
    pub code: PhysRegion,
    pub vectors: PhysRegion,
    pub rodata: PhysRegion,
    pub writable: PhysRegion,
    pub tables: PhysRegion,
    pub slots: PhysRegion,
    pub config: PhysRegion,
    pub entry: u64,
    pub root: u64,
}

/// Only the resource description that differs between selected CPUs.
#[derive(Clone, Copy, Debug)]
pub struct CpuStartupResources {
    pub context: PhysRegion,
    pub release_page: PhysRegion,
    pub stack: StackLayout,
}
impl<const N: usize> BootTables<TableSet<'_, N>> {
    /// Narrow runtime exception to the boot freeze: only the reserved scratch
    /// leaf may change. This never exposes an editable table or thaws the gate.
    /// # Safety
    /// Caller owns the all-member update protocol and its architectural sync.
    pub unsafe fn replace_scratch_page(
        &mut self,
        old: u64,
        new: u64,
        sync: impl FnMut(*mut u64) -> Result<(), crate::el2_stage1_translation::TranslationError>,
    ) -> Result<(), ResourceError> {
        if !self.frozen {
            return Err(ResourceError::Frozen);
        }
        unsafe {
            self.tables
                .replace_data_page(crate::tlb_update::SCRATCH_VA, old, new, sync)
        }
        .map_err(|_| ResourceError::Mapping)
    }
}
/// Reject partially reserved pages rather than give writable access to memory
/// whose ownership is not established. PA zero is a valid firmware page.
pub fn release_page(map: &MemoryMap, cell: u64) -> Result<PhysRegion, ResourceError> {
    if cell & 7 != 0 {
        return Err(ResourceError::Layout);
    }
    let page = PhysRegion::new(PhysAddr::new(cell & !4095), PAGE_SIZE as u64)
        .map_err(|_| ResourceError::Layout)?;
    if !map.ram().iter().any(|r| r.contains_region(page))
        || !map.reserved().iter().any(|r| r.contains_region(page))
        || map.usable_ram().iter().any(|r| r.overlaps(page))
        || map.mmio().iter().any(|r| r.overlaps(page))
    {
        return Err(ResourceError::Ownership);
    }
    Ok(page)
}
pub fn validate_range<const N: usize>(
    tables: &TableSet<'_, N>,
    region: PhysRegion,
    writable: bool,
    executable: bool,
) -> Result<(), ResourceError> {
    for address in (region.start().value()..region.end().value())
        .step_by(PAGE_SIZE)
        .chain(core::iter::once(region.end().value() - 1))
    {
        let m = tables
            .walk(address)
            .map_err(|_| ResourceError::Mapping)?
            .ok_or(ResourceError::Mapping)?;
        if m.pa != address
            || m.memory_type != MemoryType::Normal
            || m.writable != writable
            || m.executable != executable
        {
            return Err(ResourceError::Mapping);
        }
    }
    Ok(())
}
/// Validate all private stacks, including disjoint guards, under the table lock.
pub fn validate_stacks<const T: usize, const C: usize>(
    tables: &TableSet<'_, T>,
    stacks: &crate::system_info::FixedList<StackLayout, C>,
) -> Result<(), ResourceError> {
    for (i, stack) in stacks.iter().enumerate() {
        if crate::pcpu::stack_layout(stack.guard_start).ok() != Some(*stack)
            || stacks
                .iter()
                .take(i)
                .any(|other| stack.guard_start < other.top && other.guard_start < stack.top)
        {
            return Err(ResourceError::Layout);
        }
    }
    for stack in stacks {
        validate_stack(tables, *stack).map_err(|_| ResourceError::Mapping)?;
    }
    Ok(())
}
pub fn validate_resources<const N: usize>(
    tables: &TableSet<'_, N>,
    shared: &SharedStartupResources,
    r: &CpuStartupResources,
) -> Result<(), ResourceError> {
    let sections = [
        shared.code,
        shared.vectors,
        shared.rodata,
        shared.writable,
        shared.tables,
        r.release_page,
    ];
    for (i, a) in sections.iter().enumerate() {
        if a.start().value() & 4095 != 0 || a.size() & 4095 != 0 {
            return Err(ResourceError::Layout);
        }
        if sections[i + 1..].iter().any(|b| a.overlaps(*b)) {
            return Err(ResourceError::Layout);
        }
    }
    if shared.entry & 3 != 0
        || !shared.code.contains_address(PhysAddr::new(shared.entry))
        || shared.vectors.size() < 2048
        || !shared.writable.contains_region(shared.slots)
        || !shared.writable.contains_region(r.context)
        || shared.slots.start().value() & 63 != 0
        || r.context.start().value() & 63 != 0
        || shared.slots.size()
            != core::mem::size_of::<[crate::pcpu::PcpuSlot<()>; crate::cpu_topology::MAX_CPUS]>()
                as u64
        || r.context.size() != core::mem::size_of::<crate::pcpu::PcpuContext<()>>() as u64
        || !shared.slots.contains_region(r.context)
        || !shared.writable.contains_region(shared.config)
        || shared.config.start().value() & 63 != 0
        || shared.config.size()
            != core::mem::size_of::<crate::el2_policy::HostEl2ConfigStorage>() as u64
        || shared.config.overlaps(shared.slots)
        || crate::pcpu::stack_layout(r.stack.guard_start).map_err(|_| ResourceError::Layout)?
            != r.stack
    {
        return Err(ResourceError::Layout);
    }
    let allocation = PhysRegion::from_bounds(
        PhysAddr::new(r.stack.guard_start),
        PhysAddr::new(r.stack.top),
    )
    .map_err(|_| ResourceError::Layout)?;
    if sections.iter().any(|section| section.overlaps(allocation)) {
        return Err(ResourceError::Layout);
    }
    if shared.root != tables.root_pa()
        || !shared.tables.contains_address(PhysAddr::new(shared.root))
        || shared.tables.start().value() != shared.root
        || shared.tables.size() < (N * PAGE_SIZE) as u64
    {
        return Err(ResourceError::Root);
    }
    for (region, writable, executable) in [
        (shared.code, false, true),
        (shared.vectors, false, true),
        (shared.rodata, false, false),
        (shared.writable, true, false),
        (shared.tables, true, false),
        (shared.slots, true, false),
        (shared.config, true, false),
        (r.context, true, false),
        (r.release_page, true, false),
    ] {
        validate_range(tables, region, writable, executable)?;
    }
    validate_stack(tables, r.stack).map_err(|_| ResourceError::Mapping)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::el2_stage1_translation::{Mapping, TableStorage};
    fn region(start: u64, size: u64) -> PhysRegion {
        PhysRegion::new(PhysAddr::new(start), size).unwrap()
    }
    fn resources() -> (SharedStartupResources, CpuStartupResources) {
        (
            SharedStartupResources {
                code: region(0x100000, 4096),
                vectors: region(0x101000, 4096),
                rodata: region(0x102000, 4096),
                writable: region(0x103000, 8192),
                tables: region(0x800000, 16 * 4096),
                slots: region(
                    0x103fc0,
                    core::mem::size_of::<[crate::pcpu::PcpuSlot<()>; crate::cpu_topology::MAX_CPUS]>(
                    ) as u64,
                ),
                config: region(0x103000, 128),
                entry: 0x100000,
                root: 0x800000,
            },
            CpuStartupResources {
                context: region(0x104140, 64),
                release_page: region(0, 4096),
                stack: crate::pcpu::stack_layout(0x200000).unwrap(),
            },
        )
    }
    fn tables<'a>(
        storage: &'a mut TableStorage<16>,
        shared: &SharedStartupResources,
        resources: &CpuStartupResources,
        change: Option<(usize, bool, bool, MemoryType, bool)>,
    ) -> TableSet<'a, 16> {
        let mut t = TableSet::new(storage, shared.root, 44).unwrap();
        for (i, (r, w, x)) in [
            (shared.code, false, true),
            (shared.vectors, false, true),
            (shared.rodata, false, false),
            (shared.writable, true, false),
            (shared.tables, true, false),
            (resources.release_page, true, false),
        ]
        .into_iter()
        .enumerate()
        {
            let (w, x, kind, shifted) = match change {
                Some((j, w, x, k, s)) if i == j => (w, x, k, s),
                _ => (w, x, MemoryType::Normal, false),
            };
            t.map(Mapping {
                va: r.start().value(),
                pa: r.start().value() + if shifted { 4096 } else { 0 },
                size: r.size(),
                memory_type: kind,
                writable: w,
                executable: x,
            })
            .unwrap();
        }
        crate::pcpu::map_stack(&mut t, resources.stack).unwrap();
        t
    }
    #[test]
    fn failed_batch_validation_keeps_editing_open_then_freezes_once() {
        let (shared, r) = resources();
        let mut bad = r;
        bad.context = shared.slots;
        let mut storage = TableStorage::<16>::zeroed();
        let mut boot = BootTables::new(tables(&mut storage, &shared, &r, None));
        assert!(
            boot.validate_and_freeze(|t| {
                validate_resources(t, &shared, &r)?;
                validate_resources(t, &shared, &bad)
            })
            .is_err()
        );
        assert!(boot.edit().is_ok());
        boot.validate_and_freeze(|t| validate_resources(t, &shared, &r))
            .unwrap();
        assert!(boot.edit().is_err());
        assert_eq!(
            boot.validate_and_freeze(|_| Ok(())),
            Err(ResourceError::Frozen)
        );
    }
    #[test]
    fn validates_real_page_tables_and_freezes_all_mutation() {
        let (shared, r) = resources();
        let mut storage = TableStorage::<16>::zeroed();
        let mut boot = BootTables::new(tables(&mut storage, &shared, &r, None));
        boot.validate_and_freeze(|t| validate_resources(t, &shared, &r))
            .unwrap();
        assert!(boot.edit().is_err());
        assert_eq!(
            boot.validate_and_freeze(|_| Ok(())),
            Err(ResourceError::Frozen)
        );
        assert!(validate_resources(boot.view(), &shared, &r).is_ok());
    }
    #[test]
    fn rejects_wrong_permissions_device_attributes_and_physical_aliases() {
        let (shared, r) = resources();
        for fault in [
            (0, true, true, MemoryType::Normal, false),
            (0, false, false, MemoryType::Normal, false),
            (1, false, false, MemoryType::Normal, false),
            (2, true, false, MemoryType::Normal, false),
            (3, false, false, MemoryType::Normal, false),
            (4, true, true, MemoryType::Normal, false),
            (5, true, false, MemoryType::Device, false),
            (5, true, false, MemoryType::Normal, true),
        ] {
            let mut storage = TableStorage::<16>::zeroed();
            let t = tables(&mut storage, &shared, &r, Some(fault));
            assert_eq!(
                validate_resources(&t, &shared, &r),
                Err(ResourceError::Mapping),
                "{fault:?}"
            );
        }
    }
    #[test]
    fn rejects_layout_root_guard_and_object_boundary_errors_without_freezing() {
        let (shared, r) = resources();
        let mut storage = TableStorage::<16>::zeroed();
        let mut boot = BootTables::new(tables(&mut storage, &shared, &r, None));
        for (bad_shared, bad_cpu) in [
            (
                SharedStartupResources {
                    root: shared.root + 4096,
                    ..shared
                },
                r,
            ),
            (
                SharedStartupResources {
                    entry: shared.code.end().value(),
                    ..shared
                },
                r,
            ),
            (
                SharedStartupResources {
                    slots: region(0x104fc0, shared.slots.size()),
                    ..shared
                },
                r,
            ),
            (
                shared,
                CpuStartupResources {
                    context: shared.slots,
                    ..r
                },
            ),
            (
                SharedStartupResources {
                    config: region(shared.config.start().value(), 64),
                    ..shared
                },
                r,
            ),
            (
                SharedStartupResources {
                    config: region(shared.slots.start().value(), 128),
                    ..shared
                },
                r,
            ),
            (
                SharedStartupResources {
                    slots: region(shared.slots.start().value(), 384),
                    ..shared
                },
                r,
            ),
            (
                SharedStartupResources {
                    tables: shared.writable,
                    ..shared
                },
                r,
            ),
            (
                shared,
                CpuStartupResources {
                    release_page: shared.rodata,
                    ..r
                },
            ),
        ] {
            assert!(
                boot.validate_and_freeze(|t| validate_resources(t, &bad_shared, &bad_cpu))
                    .is_err()
            );
            assert!(boot.edit().is_ok());
        }
        let t = boot.edit().unwrap();
        t.map(Mapping {
            va: r.stack.guard_start,
            pa: r.stack.guard_start,
            size: 4096,
            memory_type: MemoryType::Normal,
            writable: true,
            executable: false,
        })
        .unwrap();
        assert_eq!(
            boot.validate_and_freeze(|t| validate_resources(t, &shared, &r)),
            Err(ResourceError::Mapping)
        );
    }
    #[test]
    fn slots_crossing_a_missing_page_is_rejected() {
        let mut storage = TableStorage::<16>::zeroed();
        let mut t = TableSet::new(&mut storage, 0x800000, 44).unwrap();
        t.map(Mapping {
            va: 0x103000,
            pa: 0x103000,
            size: 4096,
            memory_type: MemoryType::Normal,
            writable: true,
            executable: false,
        })
        .unwrap();
        assert_eq!(
            validate_range(&t, resources().0.slots, true, false),
            Err(ResourceError::Mapping)
        );
    }
    #[test]
    fn firmware_page_requires_complete_reserved_ram_ownership_including_zero() {
        let mut map = MemoryMap::empty();
        map.insert_ram(region(0, 0x10000)).unwrap();
        map.insert_reserved(region(0, 4096)).unwrap();
        map.finalize().unwrap();
        assert_eq!(release_page(&map, 0).unwrap(), region(0, 4096));
        assert_eq!(release_page(&map, 0xe0).unwrap(), region(0, 4096));
        assert_eq!(release_page(&map, 1), Err(ResourceError::Layout));
        assert_eq!(release_page(&map, 0x1000), Err(ResourceError::Ownership));
        assert!(release_page(&map, u64::MAX - 7).is_err());
        let mut partial = MemoryMap::empty();
        partial.insert_ram(region(0, 0x10000)).unwrap();
        partial.insert_reserved(region(0xe0, 8)).unwrap();
        partial.finalize().unwrap();
        assert_eq!(release_page(&partial, 0xe0), Err(ResourceError::Ownership));
    }
}

#[cfg(test)]
mod stack_set_tests {
    use super::*;
    use crate::{el2_stage1_translation::TableStorage, system_info::FixedList};
    #[test]
    fn duplicate_and_partially_overlapping_stack_ownership_is_rejected_before_mapping_checks() {
        let mut storage = TableStorage::<16>::zeroed();
        let tables = TableSet::new(&mut storage, 0x800000, 44).unwrap();
        for second in [0x900000, 0x901000] {
            let mut stacks = FixedList::<StackLayout, 2>::new();
            stacks
                .push(crate::pcpu::stack_layout(0x900000).unwrap())
                .unwrap();
            stacks
                .push(crate::pcpu::stack_layout(second).unwrap())
                .unwrap();
            assert_eq!(
                validate_stacks(&tables, &stacks),
                Err(ResourceError::Layout)
            );
        }
    }
}
