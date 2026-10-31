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
#[derive(Clone, Copy, Debug)]
pub struct StartupResources {
    pub code: PhysRegion,
    pub vectors: PhysRegion,
    pub rodata: PhysRegion,
    pub writable: PhysRegion,
    pub tables: PhysRegion,
    pub record: PhysRegion,
    pub context: PhysRegion,
    pub release_page: PhysRegion,
    pub stack: StackLayout,
    pub entry: u64,
    pub root: u64,
}
/// Proof that this table set passed resource validation and was frozen.
/// No public constructor, Clone, mutation or thaw operation.
pub struct FrozenStartup {
    resources: StartupResources,
}
impl FrozenStartup {
    pub fn resources(&self) -> &StartupResources {
        &self.resources
    }
}
impl<const N: usize> BootTables<TableSet<'_, N>> {
    pub fn freeze_startup(
        &mut self,
        resources: StartupResources,
        stacks: impl IntoIterator<Item = StackLayout>,
    ) -> Result<FrozenStartup, ResourceError> {
        self.validate_and_freeze(|tables| {
            validate_resources(tables, &resources)?;
            for stack in stacks {
                validate_stack(tables, stack).map_err(|_| ResourceError::Mapping)?;
            }
            Ok(())
        })?;
        Ok(FrozenStartup { resources })
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
pub fn validate_resources<const N: usize>(
    tables: &TableSet<'_, N>,
    r: &StartupResources,
) -> Result<(), ResourceError> {
    let sections = [
        r.code,
        r.vectors,
        r.rodata,
        r.writable,
        r.tables,
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
    if r.entry & 3 != 0
        || !r.code.contains_address(PhysAddr::new(r.entry))
        || r.vectors.size() < 2048
        || !r.writable.contains_region(r.record)
        || !r.writable.contains_region(r.context)
        || r.record.start().value() & 63 != 0
        || r.context.start().value() & 63 != 0
        || r.record.size() != core::mem::size_of::<crate::secondary_record::RecordStorage>() as u64
        || r.context.size() != core::mem::size_of::<crate::pcpu::PcpuContext<()>>() as u64
        || r.record.overlaps(r.context)
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
    if r.root != tables.root_pa()
        || !r.tables.contains_address(PhysAddr::new(r.root))
        || r.tables.start().value() != r.root
        || r.tables.size() < (N * PAGE_SIZE) as u64
    {
        return Err(ResourceError::Root);
    }
    for (region, writable, executable) in [
        (r.code, false, true),
        (r.vectors, false, true),
        (r.rodata, false, false),
        (r.writable, true, false),
        (r.tables, true, false),
        (r.record, true, false),
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
    fn resources() -> StartupResources {
        StartupResources {
            code: region(0x100000, 4096),
            vectors: region(0x101000, 4096),
            rodata: region(0x102000, 4096),
            writable: region(0x103000, 8192),
            tables: region(0x800000, 16 * 4096),
            record: region(
                0x103fc0,
                core::mem::size_of::<crate::secondary_record::RecordStorage>() as u64,
            ),
            context: region(0x104200, 64),
            release_page: region(0, 4096),
            stack: crate::pcpu::stack_layout(0x200000).unwrap(),
            entry: 0x100000,
            root: 0x800000,
        }
    }
    fn tables<'a>(
        storage: &'a mut TableStorage<16>,
        resources: &StartupResources,
        change: Option<(usize, bool, bool, MemoryType, bool)>,
    ) -> TableSet<'a, 16> {
        let mut t = TableSet::new(storage, resources.root, 44).unwrap();
        for (i, (r, w, x)) in [
            (resources.code, false, true),
            (resources.vectors, false, true),
            (resources.rodata, false, false),
            (resources.writable, true, false),
            (resources.tables, true, false),
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
    fn validates_real_page_tables_and_freezes_all_mutation() {
        let r = resources();
        let mut storage = TableStorage::<16>::zeroed();
        let mut boot = BootTables::new(tables(&mut storage, &r, None));
        boot.validate_and_freeze(|t| validate_resources(t, &r))
            .unwrap();
        assert!(boot.edit().is_err());
        assert_eq!(
            boot.validate_and_freeze(|_| Ok(())),
            Err(ResourceError::Frozen)
        );
        assert!(validate_resources(boot.view(), &r).is_ok());
    }
    #[test]
    fn rejects_wrong_permissions_device_attributes_and_physical_aliases() {
        let r = resources();
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
            let t = tables(&mut storage, &r, Some(fault));
            assert_eq!(
                validate_resources(&t, &r),
                Err(ResourceError::Mapping),
                "{fault:?}"
            );
        }
    }
    #[test]
    fn rejects_layout_root_guard_and_object_boundary_errors_without_freezing() {
        let r = resources();
        let mut storage = TableStorage::<16>::zeroed();
        let mut boot = BootTables::new(tables(&mut storage, &r, None));
        for bad in [
            StartupResources {
                root: r.root + 4096,
                ..r
            },
            StartupResources {
                entry: r.code.end().value(),
                ..r
            },
            StartupResources {
                record: region(0x104fc0, r.record.size()),
                ..r
            },
            StartupResources {
                context: r.record,
                ..r
            },
            StartupResources {
                tables: r.writable,
                ..r
            },
            StartupResources {
                release_page: r.rodata,
                ..r
            },
        ] {
            assert!(
                boot.validate_and_freeze(|t| validate_resources(t, &bad))
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
            boot.validate_and_freeze(|t| validate_resources(t, &r)),
            Err(ResourceError::Mapping)
        );
    }
    #[test]
    fn record_crossing_a_missing_page_is_rejected() {
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
            validate_range(&t, resources().record, true, false),
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
