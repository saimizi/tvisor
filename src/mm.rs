use core::{
    cell::UnsafeCell,
    fmt,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use spin::Mutex;
use tvisor_util::aarch64_reg::{HcrEl2, IdAa64Mmfr0El1};
use tvisor_util::el2_translation::{
    Mapping, MemoryType, TableSet, TableStorage, TranslationError, pa_bits_from_pa_range,
};
use tvisor_util::gicv2::GicV2Info;
use tvisor_util::memory_map::MemoryMap;
use tvisor_util::page_allocator::{
    AllocatorError, AllocatorStats, PAGE_BITMAP_BYTES, PageAllocator, PageBitmap, page_covering,
};
use tvisor_util::system_info::{FixedList, PhysAddr, PhysRegion};
use tvisor_util::*;

const MAX_TABLE_PAGES: usize = 16;
const TABLE_ARENA_SIZE: u64 = (MAX_TABLE_PAGES * PAGE_SIZE) as u64;

static TVISOR_TABLES: Mutex<Option<TableSet<'static, MAX_TABLE_PAGES>>> = Mutex::new(None);

struct GlobalPageAllocator {
    managed: UnsafeCell<PageBitmap<PAGE_BITMAP_BYTES>>,
    in_use: UnsafeCell<PageBitmap<PAGE_BITMAP_BYTES>>,
    ram_pages: AtomicUsize,
    initialized: AtomicBool,
}

// Phase 8 remains single-core with DAIF masked. This Sync implementation
// permits static storage; every mutable bitmap access is serialized by that
// execution policy and wrapped by the functions below.
unsafe impl Sync for GlobalPageAllocator {}

impl GlobalPageAllocator {
    const fn new() -> Self {
        Self {
            managed: UnsafeCell::new(PageBitmap::zeroed()),
            in_use: UnsafeCell::new(PageBitmap::zeroed()),
            ram_pages: AtomicUsize::new(0),
            initialized: AtomicBool::new(false),
        }
    }
}

static PAGE_ALLOCATOR: GlobalPageAllocator = GlobalPageAllocator::new();
static BOOTSTRAP_TABLES_CLAIMED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __payload_start: u8;
    static __payload_end: u8;
    static __vectors_start: u8;
    static __vectors_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __writable_start: u8;
    static __writable_end: u8;
    static __boot_stack_guard_start: u8;
    static __boot_stack_guard_end: u8;
    static __boot_stack_bottom: u8;
    static __boot_stack_top: u8;
    static __bootstrap_tables_start: u8;
    static __bootstrap_tables_end: u8;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapPageTable {
    pub root_pa: u64,
    pub pa_range: u8,
}

pub fn setup_bootstrap_page_table(
    live_dtb_pages: PhysRegion,
    uart_region: PhysRegion,
    gic: GicV2Info,
) -> Result<BootstrapPageTable, PrepareError> {
    if BOOTSTRAP_TABLES_CLAIMED.swap(true, Ordering::AcqRel) {
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

    let pt_area_start = link_addr!(__bootstrap_tables_start);
    let pt_area_end = link_addr!(__bootstrap_tables_end);

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

    // .stack
    map_identity(
        &mut tables,
        link_addr!(__boot_stack_bottom),
        link_addr!(__boot_stack_top),
        true,
        false,
    )?;

    // .bootstrap_tables
    map_identity(
        &mut tables,
        link_addr!(__bootstrap_tables_start),
        link_addr!(__bootstrap_tables_end),
        true,
        false,
    )?;

    // __payload_start
    // TODO: This is for test.
    map_identity(
        &mut tables,
        link_addr!(__payload_start),
        link_addr!(__payload_end),
        false,
        false,
    )?;

    // Complete live DTB pages are mapped read-only Normal.
    let dtb_start = live_dtb_pages.start().value();
    let dtb_end = live_dtb_pages.end().value();
    map_identity(&mut tables, dtb_start, dtb_end, false, false)?;

    // UART page is mapped RW Device.
    map_identity_device(&mut tables, uart_region)?;

    // Host GIC topology comes from the U-Boot DTB. The four architecture
    // interfaces are independently mapped because DTB regions need not be
    // physically contiguous.
    for region in [
        gic.distributor,
        gic.cpu_interface,
        gic.hypervisor_interface,
        gic.virtual_cpu_interface,
    ] {
        map_identity_device(&mut tables, region)?;
    }

    validate_bootstrap_page_table(&tables, uart_region.start().value(), live_dtb_pages)?;

    let root_pa = tables.root_pa();
    *TVISOR_TABLES.lock() = Some(tables);

    Ok(BootstrapPageTable { root_pa, pa_range })
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
    if PAGE_ALLOCATOR.initialized.load(Ordering::Acquire) {
        return Err(AllocatorError::DoubleFree.into());
    }

    validate_live_dtb(live_dtb)?;
    let live_dtb_pages = page_covering(live_dtb)?;

    // SAFETY: initialization runs once after takeover on the boot CPU before
    // any allocator client or asynchronous exception can access this state.
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
    live_dtb_pages: PhysRegion,
) -> Result<(), PrepareError> {
    let mut exclusions = FixedList::<PhysRegion, 1>::new();
    exclusions
        .push(live_dtb_pages)
        .map_err(|_| PrepareError::Validation)?;

    with_tables(|tables| {
        map_identity_regions_excluding(tables, memory_map.usable_ram(), &exclusions, true, false)?;
        Ok(())
    })?;

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
    if !PAGE_ALLOCATOR.initialized.load(Ordering::Acquire) {
        return Err(AllocatorError::NotInitialized);
    }
    let ram_pages = PAGE_ALLOCATOR.ram_pages.load(Ordering::Relaxed);
    // SAFETY: Phase 8 is single-core with asynchronous exceptions masked, so
    // allocator operations cannot overlap.
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
    operation(tables)
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
    if !is_page_aligned(region.start().value() as usize)
        || !is_page_aligned(region.end().value() as usize)
    {
        return Err(PrepareError::Validation);
    }
    tables.map(Mapping {
        va: region.start().value(),
        pa: region.start().value(),
        size: region.size(),
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

pub fn validate_bootstrap_page_table<const N: usize>(
    tables: &TableSet<'_, N>,
    uart: u64,
    live_dtb_pages: PhysRegion,
) -> Result<(), PrepareError> {
    let arena_start = link_addr!(__bootstrap_tables_start);
    let arena_end = link_addr!(__bootstrap_tables_end);
    let checks = [
        (link_addr!(__text_start), false, true, MemoryType::Normal),
        (
            link_addr!(__payload_start),
            false,
            false,
            MemoryType::Normal,
        ),
        (
            link_addr!(__payload_end) - 1,
            false,
            false,
            MemoryType::Normal,
        ),
        (link_addr!(__vectors_start), false, true, MemoryType::Normal),
        (link_addr!(__rodata_start), false, false, MemoryType::Normal),
        (
            link_addr!(__writable_start),
            true,
            false,
            MemoryType::Normal,
        ),
        (
            link_addr!(__boot_stack_bottom),
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
    for guard in [
        link_addr!(__boot_stack_guard_start),
        link_addr!(__boot_stack_guard_end) - 1,
    ] {
        if tables.walk(guard)?.is_some() {
            return Err(PrepareError::Validation);
        }
    }
    Ok(())
}
