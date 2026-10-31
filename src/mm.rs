use core::{
    cell::UnsafeCell,
    fmt,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use tvisor_util::aarch64_reg::{HcrEl2, IdAa64Mmfr0El1};
use tvisor_util::el2_stage1_translation::{
    Mapping, MemoryType, TableSet, TableStorage, TranslationError, pa_bits_from_pa_range,
};
use tvisor_util::gicv2::GicV2Info;
use tvisor_util::memory_map::MemoryMap;
use tvisor_util::page_allocator::{
    AllocatorError, AllocatorStats, PAGE_BITMAP_BYTES, PageAllocator, PageBitmap,
};
use tvisor_util::sync::IrqSpinLock;
use tvisor_util::system_info::{FixedList, PhysAddr, PhysRegion};
use tvisor_util::*;

const MAX_TABLE_PAGES: usize = 16;
const TABLE_ARENA_SIZE: u64 = (MAX_TABLE_PAGES * PAGE_SIZE) as u64;

static TVISOR_TABLES: IrqSpinLock<
    Option<tvisor_util::startup_resources::BootTables<TableSet<'static, MAX_TABLE_PAGES>>>,
> = IrqSpinLock::new(None);

struct GlobalPageAllocator {
    lock: IrqSpinLock<()>,
    managed: UnsafeCell<PageBitmap<PAGE_BITMAP_BYTES>>,
    in_use: UnsafeCell<PageBitmap<PAGE_BITMAP_BYTES>>,
    ram_pages: AtomicUsize,
    initialized: AtomicBool,
}

// SAFETY: the IRQ-save lock serializes every bitmap view, initialization and
// statistics access. Mutable bitmap references never escape its guard.
unsafe impl Sync for GlobalPageAllocator {}

impl GlobalPageAllocator {
    const fn new() -> Self {
        Self {
            lock: IrqSpinLock::new(()),
            managed: UnsafeCell::new(PageBitmap::zeroed()),
            in_use: UnsafeCell::new(PageBitmap::zeroed()),
            ram_pages: AtomicUsize::new(0),
            initialized: AtomicBool::new(false),
        }
    }
}

static PAGE_ALLOCATOR: GlobalPageAllocator = GlobalPageAllocator::new();
static EL2_STAGE1_TABLES_CLAIMED: AtomicBool = AtomicBool::new(false);

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareError {
    AddressOverflow,
    Translation(TranslationError),
    Allocator(AllocatorError),
    Validation,
}

impl From<TranslationError> for PrepareError {
    fn from(error: TranslationError) -> Self {
        Self::Translation(error)
    }
}

impl From<AllocatorError> for PrepareError {
    fn from(error: AllocatorError) -> Self {
        Self::Allocator(error)
    }
}

impl fmt::Display for PrepareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

macro_rules! link_addr {
    ($symbol:ident) => {
        core::ptr::addr_of!($symbol) as u64
    };
}

/// Root and physical-address range of tvisor's runtime EL2 stage-1 page tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct El2Stage1PageTable {
    pub root_pa: u64,
    pub pa_range: u8,
}

pub fn setup_el2_stage1_page_table(
    live_dtb_pages: PhysRegion,
    linux_image_pages: PhysRegion,
    uart_region: PhysRegion,
    gic: GicV2Info,
) -> Result<El2Stage1PageTable, PrepareError> {
    if EL2_STAGE1_TABLES_CLAIMED.swap(true, Ordering::AcqRel) {
        return Err(PrepareError::Validation);
    }

    // Check whether 4 KiB translation granules are supported.
    let Some(mmfr0) = IdAa64Mmfr0El1::dump() else {
        println!("Failed to read IdAa64Mmfr0El1 for PARange");
        return Err(PrepareError::Validation);
    };

    if mmfr0.tgran4() != 0 {
        println!("4 KiB translation-granule is not support.");
        return Err(PrepareError::Validation);
    }

    if let Some(hcr) = HcrEl2::dump() {
        // The initial translation regime suppose
        // * stage 2 translation is disabled
        // * VHE is disabled.
        if hcr.bit_vm() || hcr.bit_e2h() {
            println!(
                "Phase 7 requires HCR_EL2.VM=0 and E2H=0 (VM={} E2H={})",
                hcr.bit_vm(),
                hcr.bit_e2h()
            );
            return Err(PrepareError::Validation);
        }
    } else {
        println!("Phase 7 cannot validate HCR_EL2");
        return Err(PrepareError::Validation);
    };

    let pa_range = mmfr0.pa_range();

    let pt_area_start = link_addr!(__el2_stage1_tables_start);
    let pt_area_end = link_addr!(__el2_stage1_tables_end);

    if !is_page_aligned(pt_area_start as usize) || !is_page_aligned(pt_area_end as usize) {
        return Err(PrepareError::Validation);
    }

    if pt_area_end.checked_sub(pt_area_start) != Some(TABLE_ARENA_SIZE) {
        return Err(PrepareError::Validation);
    }

    // clear page table area
    let storage: &'static mut TableStorage<MAX_TABLE_PAGES> = unsafe {
        core::ptr::write_bytes(pt_area_start as *mut u8, 0, TABLE_ARENA_SIZE as usize);
        &mut *(pt_area_start as *mut TableStorage<MAX_TABLE_PAGES>)
    };

    let mut tables = TableSet::new(storage, pt_area_start, pa_bits_from_pa_range(pa_range)?)?;

    // .text
    map_identity(
        &mut tables,
        link_addr!(__text_start),
        link_addr!(__text_end),
        false,
        true,
    )?;

    // .vectors
    map_identity(
        &mut tables,
        link_addr!(__vectors_start),
        link_addr!(__vectors_end),
        false,
        true,
    )?;

    // .rodata
    map_identity(
        &mut tables,
        link_addr!(__rodata_start),
        link_addr!(__rodata_end),
        false,
        false,
    )?;

    // .data, .bss, .got
    map_identity(
        &mut tables,
        link_addr!(__writable_start),
        link_addr!(__writable_end),
        true,
        false,
    )?;

    // Only the guarded static boot stack is needed before physical allocation.
    tvisor_util::pcpu::map_stack(&mut tables, crate::pcpu::boot_stack())?;

    // .el2_stage1_tables
    map_identity(
        &mut tables,
        link_addr!(__el2_stage1_tables_start),
        link_addr!(__el2_stage1_tables_end),
        true,
        false,
    )?;

    // Complete live DTB pages are mapped read-only Normal.
    let dtb_start = live_dtb_pages.start().value();
    let dtb_end = live_dtb_pages.end().value();
    map_identity(&mut tables, dtb_start, dtb_end, false, false)?;

    // U-Boot loaded the Linux Image separately. Keep this source readable
    // across the private-EL2 switch until it is copied into guest RAM.
    map_identity(
        &mut tables,
        linux_image_pages.start().value(),
        linux_image_pages.end().value(),
        false,
        false,
    )?;

    // UART page is mapped RW Device.
    map_identity_device(&mut tables, uart_region)?;

    // Host GIC topology comes from the U-Boot DTB. The four architecture
    // interfaces are independently mapped because DTB regions need not be
    // physically contiguous.
    for region in [
        gic.distributor(),
        gic.cpu_interface(),
        gic.hypervisor_interface(),
        gic.virtual_cpu_interface(),
    ] {
        map_identity_device(&mut tables, region)?;
    }

    validate_el2_stage1_page_table(&tables, uart_region.start().value(), live_dtb_pages)?;

    println!(
        "Phase 11.1 EL2 stack mapping validation: static boot guard validated; table pages={}/{}",
        tables.used_pages(),
        MAX_TABLE_PAGES
    );
    let root_pa = tables.root_pa();
    *TVISOR_TABLES.lock() = Some(tvisor_util::startup_resources::BootTables::new(tables));

    Ok(El2Stage1PageTable { root_pa, pa_range })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocatorInitResult {
    pub stats: AllocatorStats,
    pub live_dtb: PhysRegion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocatorInitError {
    Allocator(AllocatorError),
    InvalidDtb,
    DtbAllocatable,
}

impl From<AllocatorError> for AllocatorInitError {
    fn from(error: AllocatorError) -> Self {
        Self::Allocator(error)
    }
}

impl fmt::Display for AllocatorInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

/// Initialize the global physical-page allocator after tvisor has installed
/// its private stack, vectors, and EL2 stage-1 translation regime.
///
/// All `usable_ram` pages start unused. U-Boot runtime allocations require no
/// explicit reclamation because they are absent from the permanent platform
/// reservation map. The tvisor image and live DTB remain reserved.
pub fn initialize_allocator_after_takeover(
    memory_map: &MemoryMap,
    live_dtb: PhysRegion,
) -> Result<AllocatorInitResult, AllocatorInitError> {
    let _guard = PAGE_ALLOCATOR.lock.lock();
    if PAGE_ALLOCATOR.initialized.load(Ordering::Acquire) {
        return Err(AllocatorError::DoubleFree.into());
    }

    validate_live_dtb(live_dtb)?;
    let live_dtb_pages =
        PhysRegion::new_aligned(live_dtb.start(), live_dtb.size(), PAGE_SIZE as u64)
            .map_err(|_| AllocatorInitError::InvalidDtb)?;

    // SAFETY: the allocator lock exclusively protects both bitmap views.
    let allocator = unsafe {
        PageAllocator::new(
            memory_map.ram(),
            memory_map.usable_ram(),
            memory_map.usable_ram(),
            &mut *PAGE_ALLOCATOR.managed.get(),
            &mut *PAGE_ALLOCATOR.in_use.get(),
        )?
    };

    let mut page = live_dtb_pages.start().value();
    while page < live_dtb_pages.end().value() {
        if allocator.state(PhysAddr::new(page))? != tvisor_util::page_allocator::PageState::Reserved
        {
            return Err(AllocatorInitError::DtbAllocatable);
        }
        page = page
            .checked_add(PAGE_SIZE as u64)
            .ok_or(AllocatorError::AddressOverflow)?;
    }

    let stats = allocator.stats();
    PAGE_ALLOCATOR
        .ram_pages
        .store(stats.ram_pages, Ordering::Relaxed);
    PAGE_ALLOCATOR.initialized.store(true, Ordering::Release);
    Ok(AllocatorInitResult { stats, live_dtb })
}

pub fn map_usable_ram(
    memory_map: &MemoryMap,
    live_dtb: PhysRegion,
    linux_image_pages: PhysRegion,
    secondary_stacks: &FixedList<
        tvisor_util::pcpu::SecondaryStack,
        { tvisor_util::cpu_topology::MAX_CPUS },
    >,
) -> Result<(), PrepareError> {
    // One DTB exclusion plus at most MAX_CPUS-1 dynamic guards.
    let mut exclusions = FixedList::<PhysRegion, { tvisor_util::cpu_topology::MAX_CPUS }>::new();
    for stack in secondary_stacks {
        exclusions
            .push(
                PhysRegion::new(
                    PhysAddr::new(stack.layout.guard_start),
                    tvisor_util::pcpu::GUARD_SIZE,
                )
                .map_err(|_| PrepareError::Validation)?,
            )
            .map_err(|_| PrepareError::Validation)?;
    }
    let live_dtb_aligned =
        PhysRegion::new_aligned(live_dtb.start(), live_dtb.size(), PAGE_SIZE as u64)
            .map_err(|_| PrepareError::AddressOverflow)?;

    exclusions
        .push(live_dtb_aligned)
        .map_err(|_| PrepareError::Validation)?;

    let used_pages = with_tables(|tables| {
        map_identity_regions_excluding(tables, memory_map.usable_ram(), &exclusions, true, false)?;
        map_identity(
            tables,
            linux_image_pages.start().value(),
            linux_image_pages.end().value(),
            false,
            false,
        )?;
        tvisor_util::pcpu::validate_stack(tables, crate::pcpu::boot_stack())?;
        for stack in secondary_stacks {
            tvisor_util::pcpu::validate_stack(tables, stack.layout)?;
        }
        Ok(tables.used_pages())
    })?;

    println!(
        "Phase 11.1 final EL2 mappings: {} guards unmapped (one static boot); table pages={}/{}",
        secondary_stacks.len() + 1,
        used_pages,
        MAX_TABLE_PAGES
    );

    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "tlbi alle2",
            "dsb ish",
            "isb",
            options(nostack, preserves_flags)
        );
    }

    Ok(())
}

/// Map a validated firmware release cell while the boot CPU is the only
/// tvisor CPU. The reserved page remains unavailable to the page allocator.
/// This must not be called after a secondary is released: cross-CPU TLB
/// shootdown belongs to a later milestone.
pub fn map_firmware_release_cell(address: PhysAddr) -> Result<(), PrepareError> {
    let contract = crate::smp::startup_contract().ok_or(PrepareError::Validation)?;
    if !contract
        .release_cells()
        .iter()
        .any(|cell| cell.start() == address)
    {
        return Err(PrepareError::Validation);
    }
    let memory_map = crate::boot::memory_map().ok_or(PrepareError::Validation)?;
    let page = tvisor_util::startup_resources::release_page(memory_map, address.value())
        .map_err(|_| PrepareError::Validation)?;
    let start = page.start().value();
    let end = page.end().value();
    with_tables(|tables| {
        match tables.walk(start)? {
            None => map_identity(tables, start, end, true, false)?,
            Some(mapping)
                if mapping.pa == start
                    && mapping.memory_type == MemoryType::Normal
                    && mapping.writable
                    && !mapping.executable => {}
            Some(_) => return Err(PrepareError::Validation),
        }
        let mapping = tables
            .walk(address.value())?
            .ok_or(PrepareError::Validation)?;
        if mapping.pa != address.value()
            || mapping.memory_type != MemoryType::Normal
            || !mapping.writable
            || mapping.executable
        {
            return Err(PrepareError::Validation);
        }
        Ok(())
    })?;
    // Publish the newly installed descriptor before using it on the boot CPU.
    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "tlbi alle2",
            "dsb ish",
            "isb",
            options(nostack, preserves_flags)
        );
    }
    Ok(())
}

/// Allocate the lowest-addressed unused managed 4 KiB physical page.
///
/// The returned page is changed to `InUse`. Reserved RAM, MMIO, unpopulated
/// addresses, and pages already in use are skipped.
pub fn allocate_page() -> Result<PhysAddr, AllocatorError> {
    with_allocator(|allocator| allocator.allocate())
}

/// Allocate secondary stack ranges using bitmap metadata only. The ranges
/// are not yet accessible until their RAM mappings have been installed.
pub fn allocate_secondary_stacks(
    topology: &tvisor_util::cpu_topology::CpuTopology,
) -> Result<
    FixedList<tvisor_util::pcpu::SecondaryStack, { tvisor_util::cpu_topology::MAX_CPUS }>,
    AllocatorError,
> {
    with_allocator(|allocator| tvisor_util::pcpu::allocate_secondary_stacks(topology, allocator))
}

/// Allocate the lowest-addressed run of `pages` contiguous unused managed
/// physical pages and mark the complete run `InUse` atomically.
///
/// Under tvisor's current identity map, the returned physical range is also a
/// virtually contiguous EL2 range at the same address.
pub fn allocate_contiguous_pages(pages: usize) -> Result<PhysAddr, AllocatorError> {
    with_allocator(|allocator| allocator.allocate_contiguous(pages))
}

pub fn free_page(page: PhysAddr) -> Result<(), AllocatorError> {
    with_allocator(|allocator| allocator.free(page))
}

pub fn allocator_stats() -> Result<AllocatorStats, AllocatorError> {
    with_allocator(|allocator| Ok(allocator.stats()))
}

fn with_allocator<T>(
    operation: impl FnOnce(&mut PageAllocator<'_, PAGE_BITMAP_BYTES>) -> Result<T, AllocatorError>,
) -> Result<T, AllocatorError> {
    let _guard = PAGE_ALLOCATOR.lock.lock();
    if !PAGE_ALLOCATOR.initialized.load(Ordering::Acquire) {
        return Err(AllocatorError::NotInitialized);
    }
    let ram_pages = PAGE_ALLOCATOR.ram_pages.load(Ordering::Relaxed);
    // SAFETY: the IRQ-save guard exclusively owns both bitmap views.
    let mut allocator = unsafe {
        PageAllocator::from_existing(
            &mut *PAGE_ALLOCATOR.managed.get(),
            &mut *PAGE_ALLOCATOR.in_use.get(),
            ram_pages,
        )?
    };
    operation(&mut allocator)
}

fn with_tables<T>(
    operation: impl FnOnce(&mut TableSet<'static, MAX_TABLE_PAGES>) -> Result<T, PrepareError>,
) -> Result<T, PrepareError> {
    let mut guard = TVISOR_TABLES.lock();
    let tables = guard.as_mut().ok_or(PrepareError::Validation)?;
    operation(tables.edit().map_err(|_| PrepareError::Validation)?)
}

/// Validate and seal under the same lock; later writer requests are rejected.
pub fn validate_and_freeze_startup(
    resources: &tvisor_util::startup_resources::StartupResources,
    stacks: &FixedList<tvisor_util::pcpu::StackLayout, { tvisor_util::cpu_topology::MAX_CPUS }>,
) -> Result<tvisor_util::startup_resources::FrozenStartup, PrepareError> {
    let mut guard = TVISOR_TABLES.lock();
    let tables = guard.as_mut().ok_or(PrepareError::Validation)?;
    tables
        .freeze_startup(*resources, stacks.into_iter().copied())
        .map_err(|_| PrepareError::Validation)
}

fn validate_live_dtb(region: PhysRegion) -> Result<(), AllocatorInitError> {
    if region.size() < 8 {
        return Err(AllocatorInitError::InvalidDtb);
    }
    let base = region.start().value() as *const u8;
    // SAFETY: prepare mapped the validated live-DTB region read-only before
    // switching tables, and the permanent reservation keeps all covering
    // pages outside the allocator.
    let read_be32 = |offset: usize| unsafe {
        u32::from_be_bytes([
            core::ptr::read_volatile(base.add(offset)),
            core::ptr::read_volatile(base.add(offset + 1)),
            core::ptr::read_volatile(base.add(offset + 2)),
            core::ptr::read_volatile(base.add(offset + 3)),
        ])
    };
    if read_be32(0) != 0xd00d_feed || u64::from(read_be32(4)) != region.size() {
        return Err(AllocatorInitError::InvalidDtb);
    }
    Ok(())
}

// Create a mapping which VA is same to PA
pub fn map_identity<const N: usize>(
    tables: &mut TableSet<'_, N>,
    start: u64,
    end: u64,
    writable: bool,
    executable: bool,
) -> Result<(), PrepareError> {
    if !is_page_aligned(start as usize) || !is_page_aligned(end as usize) || start >= end {
        return Err(PrepareError::Validation);
    }
    tables.map(Mapping {
        va: start,
        pa: start,
        size: end - start,
        memory_type: MemoryType::Normal,
        writable,
        executable,
    })?;
    Ok(())
}

/// Identity-map a page-aligned MMIO region using Device-nGnRE attributes.
pub fn map_identity_device<const N: usize>(
    tables: &mut TableSet<'_, N>,
    region: PhysRegion,
) -> Result<(), PrepareError> {
    let aligned_region = PhysRegion::new_aligned(region.start(), region.size(), PAGE_SIZE as u64)
        .map_err(|_| PrepareError::AddressOverflow)?;

    tables.map(Mapping {
        va: aligned_region.start().value(),
        pa: aligned_region.start().value(),
        size: aligned_region.size(),
        memory_type: MemoryType::Device,
        writable: true,
        executable: false,
    })?;
    Ok(())
}

// Identity-map regions but excluding the exclusions part.
pub fn map_identity_regions_excluding<const T: usize, const R: usize, const E: usize>(
    tables: &mut TableSet<'_, T>,
    regions: &FixedList<PhysRegion, R>,
    exclusions: &FixedList<PhysRegion, E>,
    writable: bool,
    executable: bool,
) -> Result<(), PrepareError> {
    for region in regions {
        let start = align_up(region.start().value() as usize, PAGE_SIZE)
            .ok_or(PrepareError::AddressOverflow)?;
        let end = page_address(region.end().value() as usize);
        let mut cursor = start;
        while cursor < end {
            let next = exclusions
                .iter()
                .filter_map(|excluded| {
                    let excluded_start = page_address(excluded.start().value() as usize);
                    let excluded_end = align_up(excluded.end().value() as usize, PAGE_SIZE)?;
                    (excluded_end > cursor && excluded_start < end)
                        .then_some((excluded_start, excluded_end))
                })
                .min_by_key(|(excluded_start, _)| *excluded_start);
            let Some((excluded_start, excluded_end)) = next else {
                map_identity(tables, cursor as u64, end as u64, writable, executable)?;
                break;
            };
            if excluded_start > cursor {
                map_identity(
                    tables,
                    cursor as u64,
                    excluded_start.min(end) as u64,
                    writable,
                    executable,
                )?;
            }
            cursor = cursor.max(excluded_end).min(end);
        }
    }
    Ok(())
}

pub fn validate_el2_stage1_page_table<const N: usize>(
    tables: &TableSet<'_, N>,
    uart: u64,
    live_dtb_pages: PhysRegion,
) -> Result<(), PrepareError> {
    let arena_start = link_addr!(__el2_stage1_tables_start);
    let arena_end = link_addr!(__el2_stage1_tables_end);
    let checks = [
        (link_addr!(__text_start), false, true, MemoryType::Normal),
        (link_addr!(__vectors_start), false, true, MemoryType::Normal),
        (link_addr!(__rodata_start), false, false, MemoryType::Normal),
        (
            link_addr!(__writable_start),
            true,
            false,
            MemoryType::Normal,
        ),
        (arena_start, true, false, MemoryType::Normal),
        (arena_end - 1, true, false, MemoryType::Normal),
        (
            live_dtb_pages.start().value(),
            false,
            false,
            MemoryType::Normal,
        ),
        (
            live_dtb_pages.end().value() - 1,
            false,
            false,
            MemoryType::Normal,
        ),
        (uart, true, false, MemoryType::Device),
    ];
    for (va, writable, executable, memory_type) in checks {
        let translation = tables.walk(va)?.ok_or(PrepareError::Validation)?;
        if translation.pa != va
            || translation.writable != writable
            || translation.executable != executable
            || translation.memory_type != memory_type
        {
            return Err(PrepareError::Validation);
        }
    }
    tvisor_util::pcpu::validate_stack(tables, crate::pcpu::boot_stack())?;
    Ok(())
}
