use core::{
    arch::asm,
    cell::UnsafeCell,
    fmt,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use tvisor_util::el2_translation::{
    El2RegisterValues, Mapping, MemoryType, PAGE_SIZE, TableSet, TableStorage, TranslationError,
    pa_bits_from_parange, register_values,
};
use tvisor_util::memory_map::{MAX_NORMALIZED_RESERVED_REGIONS, MemoryMap};
use tvisor_util::page_allocator::{
    AllocatorError, AllocatorStats, PAGE_BITMAP_BYTES, PageAllocator, PageBitmap, PageState,
    page_covering,
};
use tvisor_util::system_info::{FixedList, PhysAddr, PhysRegion};

const MAX_TABLE_PAGES: usize = 16;
const TABLE_ARENA_SIZE: u64 = MAX_TABLE_PAGES as u64 * PAGE_SIZE;

#[derive(Clone, Copy)]
struct ReclaimMemoryInfo {
    /// U-Boot handoff regions eligible for reclamation after takeover.
    reclaimable_regions: FixedList<PhysRegion, MAX_NORMALIZED_RESERVED_REGIONS>,
    /// Exact live DTB byte range used for post-takeover validation.
    live_dtb: PhysRegion,
    /// Page-rounded DTB range that must remain allocated during reclamation.
    retained_dtb_pages: PhysRegion,
}

struct GlobalReclaimMemoryInfo {
    value: UnsafeCell<Option<ReclaimMemoryInfo>>,
}

// Phase 8 remains single-core with DAIF masked. The value is installed before
// allocator initialization is published and consumed exactly once afterward.
unsafe impl Sync for GlobalReclaimMemoryInfo {}

impl GlobalReclaimMemoryInfo {
    const fn empty() -> Self {
        Self {
            value: UnsafeCell::new(None),
        }
    }
}

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
static RECLAIM_MEMORY_INFO: GlobalReclaimMemoryInfo = GlobalReclaimMemoryInfo::empty();

unsafe extern "C" {
    static __text_start: u8;
    static __text_end: u8;
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

#[derive(Debug, Clone, Copy)]
pub struct PreparedTables {
    pub registers: El2RegisterValues,
    pub arena_start: u64,
    pub arena_end: u64,
    pub used_pages: usize,
    pub allocator_stats: AllocatorStats,
    pub live_dtb_pages: PhysRegion,
}

macro_rules! link_addr {
    ($symbol:ident) => {
        core::ptr::addr_of!($symbol) as u64
    };
}

pub fn prepare(
    memory_map: &MemoryMap,
    uart_register_base: u64,
    parange: u8,
    live_dtb: PhysRegion,
) -> Result<PreparedTables, PrepareError> {
    let pa_bits = pa_bits_from_parange(parange)?;
    initialize_allocator(memory_map, live_dtb)?;
    let arena_start =
        with_allocator(|allocator| allocator.allocate_contiguous(MAX_TABLE_PAGES))?.value();
    let arena_end = arena_start
        .checked_add(TABLE_ARENA_SIZE)
        .ok_or(PrepareError::AddressOverflow)?;

    // SAFETY: the global allocator returned MAX_TABLE_PAGES contiguous pages
    // from INITIAL RAM and marked all of them InUse. U-Boot identity-maps this
    // RAM, and TableSet has exclusive access to the allocation.
    let storage = unsafe {
        core::ptr::write_bytes(arena_start as *mut u8, 0, TABLE_ARENA_SIZE as usize);
        &mut *(arena_start as *mut TableStorage<MAX_TABLE_PAGES>)
    };
    let mut tables = TableSet::new(storage, arena_start, pa_bits)?;

    let live_dtb_pages = page_covering(live_dtb)?;
    let mut mapping_exclusions = FixedList::<PhysRegion, 1>::new();
    mapping_exclusions
        .push(live_dtb_pages)
        .map_err(|_| PrepareError::Validation)?;

    map_identity(
        &mut tables,
        link_addr!(__text_start),
        link_addr!(__text_end),
        false,
        true,
    )?;
    map_identity(
        &mut tables,
        link_addr!(__vectors_start),
        link_addr!(__vectors_end),
        false,
        true,
    )?;
    map_identity(
        &mut tables,
        link_addr!(__rodata_start),
        link_addr!(__rodata_end),
        false,
        false,
    )?;
    map_identity(
        &mut tables,
        link_addr!(__writable_start),
        link_addr!(__writable_end),
        true,
        false,
    )?;
    map_identity(
        &mut tables,
        link_addr!(__boot_stack_bottom),
        link_addr!(__boot_stack_top),
        true,
        false,
    )?;
    map_identity_regions_excluding(
        &mut tables,
        memory_map.usable_ram(),
        &mapping_exclusions,
        true,
        false,
    )?;
    map_identity(
        &mut tables,
        live_dtb_pages.start().value(),
        live_dtb_pages.end().value(),
        false,
        false,
    )?;

    let uart_page = uart_register_base & !(PAGE_SIZE - 1);
    tables.map(Mapping {
        va: uart_page,
        pa: uart_page,
        size: PAGE_SIZE,
        memory_type: MemoryType::Device,
        writable: true,
        executable: false,
    })?;

    validate(
        &tables,
        uart_register_base,
        arena_start,
        arena_end,
        live_dtb_pages,
    )?;
    let used_pages = tables.used_pages();
    let registers = register_values(tables.root_pa(), parange)?;
    // Publish every descriptor store before TTBR0_EL2 can expose the tables to
    // the hardware walker. The initial implementation uses coherent Normal
    // WB/WA memory and identity mappings under both translation regimes.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        asm!("dsb ishst", options(nostack, preserves_flags));
    }
    Ok(PreparedTables {
        registers,
        arena_start,
        arena_end,
        used_pages,
        allocator_stats: allocator_stats()?,
        live_dtb_pages,
    })
}

fn initialize_allocator(
    memory_map: &MemoryMap,
    live_dtb: PhysRegion,
) -> Result<(), AllocatorError> {
    if PAGE_ALLOCATOR.initialized.load(Ordering::Acquire) {
        return Ok(());
    }
    let live_dtb_pages = page_covering(live_dtb)?;
    // SAFETY: initialization runs once on the boot CPU before any allocator
    // client or asynchronous exception can access this static state.
    let allocator = unsafe {
        PageAllocator::new(
            memory_map.ram(),
            memory_map.usable_ram(),
            memory_map.initial_usable_ram(),
            &mut *PAGE_ALLOCATOR.managed.get(),
            &mut *PAGE_ALLOCATOR.in_use.get(),
        )?
    };
    let stats = allocator.stats();
    // SAFETY: reclamation information is installed before allocator
    // initialization is published and is consumed only after takeover.
    unsafe {
        *RECLAIM_MEMORY_INFO.value.get() = Some(ReclaimMemoryInfo {
            reclaimable_regions: *memory_map.transition_reserved(),
            live_dtb,
            retained_dtb_pages: live_dtb_pages,
        });
    }
    PAGE_ALLOCATOR
        .ram_pages
        .store(stats.ram_pages, Ordering::Relaxed);
    PAGE_ALLOCATOR.initialized.store(true, Ordering::Release);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TakeoverResult {
    pub released_pages: usize,
    pub stats: AllocatorStats,
    pub live_dtb: PhysRegion,
    /// An unused page from reclaimed U-Boot memory for hardware validation.
    pub reclaimed_test_page: Option<PhysAddr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeoverError {
    Allocator(AllocatorError),
    ReclaimMemoryInfoUnavailable,
    InvalidDtb,
}

impl From<AllocatorError> for TakeoverError {
    fn from(error: AllocatorError) -> Self {
        Self::Allocator(error)
    }
}

/// Reclaim U-Boot handoff RAM while retaining the page-rounded live DTB.
/// This is valid only after tvisor has crossed its no-return boundary.
pub fn complete_takeover() -> Result<TakeoverResult, TakeoverError> {
    let info = take_reclaim_memory_info()?;
    let live_dtb = info.live_dtb;
    let live_dtb_pages = info.retained_dtb_pages;
    let mut released_pages = 0;
    for region in &info.reclaimable_regions {
        if region.overlaps(live_dtb_pages) {
            if region.start() < live_dtb_pages.start() {
                let before = PhysRegion::from_bounds(region.start(), live_dtb_pages.start())
                    .map_err(AllocatorError::InvalidRegion)?;
                released_pages += with_allocator(|allocator| allocator.release(before))?;
            }
            if region.end() > live_dtb_pages.end() {
                let after = PhysRegion::from_bounds(live_dtb_pages.end(), region.end())
                    .map_err(AllocatorError::InvalidRegion)?;
                released_pages += with_allocator(|allocator| allocator.release(after))?;
            }
        } else {
            released_pages += with_allocator(|allocator| allocator.release(*region))?;
        }
    }
    let mut page = live_dtb_pages.start().value();
    while page < live_dtb_pages.end().value() {
        let state = with_allocator(|allocator| allocator.state(PhysAddr::new(page)))?;
        if state == PageState::Unused {
            return Err(AllocatorError::DoubleFree.into());
        }
        page = page
            .checked_add(PAGE_SIZE)
            .ok_or(AllocatorError::AddressOverflow)?;
    }
    validate_live_dtb(live_dtb)?;
    let reclaimed_test_page = first_unused_page_in(&info.reclaimable_regions, live_dtb_pages);
    Ok(TakeoverResult {
        released_pages,
        stats: allocator_stats()?,
        live_dtb,
        reclaimed_test_page,
    })
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

/// Allocate the highest-addressed unused managed 4 KiB physical page.
///
/// The returned page is changed to `InUse`. This reverse-search API currently
/// supports hardware validation of allocator coverage near the top of RAM.
pub fn allocate_high_page() -> Result<PhysAddr, AllocatorError> {
    with_allocator(|allocator| allocator.allocate_high())
}

/// Allocate the lowest-addressed unused managed 4 KiB page inside `region`.
///
/// The search uses only complete pages covered by `region`: its start is
/// rounded up and its end is rounded down to page boundaries. The region does
/// not grant allocator ownership; reserved and already-in-use pages within it
/// are skipped. The returned page is changed to `InUse`.
pub fn allocate_page_in(region: PhysRegion) -> Result<PhysAddr, AllocatorError> {
    with_allocator(|allocator| allocator.allocate_in(region))
}

pub fn free_page(page: PhysAddr) -> Result<(), AllocatorError> {
    with_allocator(|allocator| allocator.free(page))
}

pub fn allocator_stats() -> Result<AllocatorStats, AllocatorError> {
    with_allocator(|allocator| Ok(allocator.stats()))
}

fn first_unused_page_in<const N: usize>(
    regions: &FixedList<PhysRegion, N>,
    retained_pages: PhysRegion,
) -> Option<PhysAddr> {
    for region in regions {
        let start = align_up(region.start().value(), PAGE_SIZE)?;
        let end = region.end().value() & !(PAGE_SIZE - 1);
        let mut page = start;
        while page < end {
            let address = PhysAddr::new(page);
            if !retained_pages.contains_address(address)
                && with_allocator(|allocator| allocator.state(address)).ok()? == PageState::Unused
            {
                return Some(address);
            }
            page = page.checked_add(PAGE_SIZE)?;
        }
    }
    None
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

fn take_reclaim_memory_info() -> Result<ReclaimMemoryInfo, TakeoverError> {
    if !PAGE_ALLOCATOR.initialized.load(Ordering::Acquire) {
        return Err(AllocatorError::NotInitialized.into());
    }
    // SAFETY: Phase 8 is single-core with asynchronous exceptions masked.
    // Initialization published Some(info), and taking it makes reclamation a
    // one-shot operation.
    unsafe { (*RECLAIM_MEMORY_INFO.value.get()).take() }
        .ok_or(TakeoverError::ReclaimMemoryInfoUnavailable)
}

fn validate_live_dtb(region: PhysRegion) -> Result<(), TakeoverError> {
    if region.size() < 8 {
        return Err(TakeoverError::InvalidDtb);
    }
    let base = region.start().value() as *const u8;
    // SAFETY: prepare mapped the validated live-DTB region read-only before
    // switching tables, and complete_takeover retained all covering pages.
    let read_be32 = |offset: usize| unsafe {
        u32::from_be_bytes([
            core::ptr::read_volatile(base.add(offset)),
            core::ptr::read_volatile(base.add(offset + 1)),
            core::ptr::read_volatile(base.add(offset + 2)),
            core::ptr::read_volatile(base.add(offset + 3)),
        ])
    };
    if read_be32(0) != 0xd00d_feed || u64::from(read_be32(4)) != region.size() {
        return Err(TakeoverError::InvalidDtb);
    }
    Ok(())
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
}

// Create a mapping which VA is same to PA
fn map_identity<const N: usize>(
    tables: &mut TableSet<'_, N>,
    start: u64,
    end: u64,
    writable: bool,
    executable: bool,
) -> Result<(), PrepareError> {
    if start & (PAGE_SIZE - 1) != 0 || end & (PAGE_SIZE - 1) != 0 || start >= end {
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

fn map_identity_regions_excluding<const T: usize, const R: usize, const E: usize>(
    tables: &mut TableSet<'_, T>,
    regions: &FixedList<PhysRegion, R>,
    exclusions: &FixedList<PhysRegion, E>,
    writable: bool,
    executable: bool,
) -> Result<(), PrepareError> {
    for region in regions {
        let start =
            align_up(region.start().value(), PAGE_SIZE).ok_or(PrepareError::AddressOverflow)?;
        let end = region.end().value() & !(PAGE_SIZE - 1);
        let mut cursor = start;
        while cursor < end {
            let next = exclusions
                .iter()
                .filter_map(|excluded| {
                    let excluded_start = excluded.start().value() & !(PAGE_SIZE - 1);
                    let excluded_end = align_up(excluded.end().value(), PAGE_SIZE)?;
                    (excluded_end > cursor && excluded_start < end)
                        .then_some((excluded_start, excluded_end))
                })
                .min_by_key(|(excluded_start, _)| *excluded_start);
            let Some((excluded_start, excluded_end)) = next else {
                map_identity(tables, cursor, end, writable, executable)?;
                break;
            };
            if excluded_start > cursor {
                map_identity(
                    tables,
                    cursor,
                    excluded_start.min(end),
                    writable,
                    executable,
                )?;
            }
            cursor = cursor.max(excluded_end).min(end);
        }
    }
    Ok(())
}

fn validate<const N: usize>(
    tables: &TableSet<'_, N>,
    uart: u64,
    arena_start: u64,
    arena_end: u64,
    live_dtb_pages: PhysRegion,
) -> Result<(), PrepareError> {
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
