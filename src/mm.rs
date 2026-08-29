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
use tvisor_util::memory_map::MAX_USABLE_RAM_REGIONS;
use tvisor_util::memory_map::MemoryMap;
use tvisor_util::page_allocator::{
    AllocatorError, AllocatorLayout, AllocatorStats, PAGE_BITMAP_BYTES, PageAllocator,
};
use tvisor_util::system_info::{FixedList, PhysAddr, PhysRegion};

const MAX_TABLE_PAGES: usize = 16;
const TABLE_ARENA_SIZE: u64 = MAX_TABLE_PAGES as u64 * PAGE_SIZE;
const MAX_ALLOCATOR_REGIONS: usize = MAX_USABLE_RAM_REGIONS + 4;

#[derive(Clone, Copy)]
struct AllocatorPlan {
    layout: AllocatorLayout<MAX_ALLOCATOR_REGIONS>,
    reclaimed_test_page: Option<PhysAddr>,
}

impl AllocatorPlan {
    const fn empty() -> Self {
        Self {
            layout: AllocatorLayout::empty(),
            reclaimed_test_page: None,
        }
    }
}

struct GlobalPageAllocator {
    plan: UnsafeCell<AllocatorPlan>,
    bitmap: UnsafeCell<[u8; PAGE_BITMAP_BYTES]>,
    allocated_pages: AtomicUsize,
    initialized: AtomicBool,
}

// Phase 8 remains single-core with DAIF masked. This Sync implementation
// permits static storage; every mutable bitmap access is serialized by that
// execution policy and wrapped by the functions below.
unsafe impl Sync for GlobalPageAllocator {}

impl GlobalPageAllocator {
    const fn new() -> Self {
        Self {
            plan: UnsafeCell::new(AllocatorPlan::empty()),
            bitmap: UnsafeCell::new([0; PAGE_BITMAP_BYTES]),
            allocated_pages: AtomicUsize::new(0),
            initialized: AtomicBool::new(false),
        }
    }
}

static PAGE_ALLOCATOR: GlobalPageAllocator = GlobalPageAllocator::new();

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
    NoInitialArena,
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
    pub allocator_regions: usize,
    pub allocator_pages: usize,
    pub reclaimed_test_page: Option<PhysAddr>,
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
    let arena_start = select_arena(memory_map.initial_usable_ram())?;
    let arena_end = arena_start
        .checked_add(TABLE_ARENA_SIZE)
        .ok_or(PrepareError::AddressOverflow)?;

    // SAFETY: select_arena proves that the complete aligned storage lies in
    // INITIAL RAM. U-Boot currently identity-maps this RAM, and this function
    // has exclusive ownership of the arena until the no-return takeover.
    let storage = unsafe {
        core::ptr::write_bytes(arena_start as *mut u8, 0, TABLE_ARENA_SIZE as usize);
        &mut *(arena_start as *mut TableStorage<MAX_TABLE_PAGES>)
    };
    let mut tables = TableSet::new(storage, arena_start, pa_bits)?;

    let arena_region =
        PhysRegion::from_bounds(PhysAddr::new(arena_start), PhysAddr::new(arena_end))
            .map_err(|_| PrepareError::Validation)?;
    let mut allocator_exclusions = FixedList::<PhysRegion, 2>::new();
    allocator_exclusions
        .push(arena_region)
        .map_err(|_| PrepareError::Validation)?;
    allocator_exclusions
        .push(live_dtb)
        .map_err(|_| PrepareError::Validation)?;
    let allocator_layout = AllocatorLayout::<MAX_ALLOCATOR_REGIONS>::from_regions_excluding(
        memory_map.usable_ram(),
        &allocator_exclusions,
    )?;
    let reclaimed_test_page = allocator_layout.first_page_in(memory_map.transition_reserved());

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
    map_identity(&mut tables, arena_start, arena_end, true, false)?;
    for region in allocator_layout.regions() {
        map_identity(
            &mut tables,
            region.start().value(),
            region.end().value(),
            true,
            false,
        )?;
    }

    let uart_page = uart_register_base & !(PAGE_SIZE - 1);
    tables.map(Mapping {
        va: uart_page,
        pa: uart_page,
        size: PAGE_SIZE,
        memory_type: MemoryType::Device,
        writable: true,
        executable: false,
    })?;

    validate(&tables, uart_register_base, arena_start, arena_end)?;
    let used_pages = tables.used_pages();
    let registers = register_values(tables.root_pa(), parange)?;
    // Publish every descriptor store before TTBR0_EL2 can expose the tables to
    // the hardware walker. The initial implementation uses coherent Normal
    // WB/WA memory and identity mappings under both translation regimes.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        asm!("dsb ishst", options(nostack, preserves_flags));
    }
    // SAFETY: prepare runs once before the private-EL2 transition. No runtime
    // allocator access is possible until phase8_initialize() sets initialized.
    unsafe {
        *PAGE_ALLOCATOR.plan.get() = AllocatorPlan {
            layout: allocator_layout,
            reclaimed_test_page,
        };
    }
    Ok(PreparedTables {
        registers,
        arena_start,
        arena_end,
        used_pages,
        allocator_regions: allocator_layout.regions().len(),
        allocator_pages: allocator_layout.total_pages(),
        reclaimed_test_page,
    })
}

pub fn phase8_initialize() -> Result<AllocatorStats, AllocatorError> {
    if PAGE_ALLOCATOR.initialized.load(Ordering::Acquire) {
        return allocator_stats();
    }
    // SAFETY: the no-return path is single-core with asynchronous exceptions
    // masked. prepare() installed the plan before the page-table transition.
    let allocator = unsafe {
        PageAllocator::new(
            &(*PAGE_ALLOCATOR.plan.get()).layout,
            &mut *PAGE_ALLOCATOR.bitmap.get(),
        )?
    };
    let stats = allocator.stats();
    PAGE_ALLOCATOR
        .allocated_pages
        .store(stats.allocated_pages, Ordering::Relaxed);
    PAGE_ALLOCATOR.initialized.store(true, Ordering::Release);
    Ok(stats)
}

pub fn allocate_page() -> Result<PhysAddr, AllocatorError> {
    with_allocator(|allocator| allocator.allocate())
}

pub fn allocate_page_in(region: PhysRegion) -> Result<PhysAddr, AllocatorError> {
    with_allocator(|allocator| allocator.allocate_in(region))
}

pub fn free_page(page: PhysAddr) -> Result<(), AllocatorError> {
    with_allocator(|allocator| allocator.free(page))
}

pub fn allocator_stats() -> Result<AllocatorStats, AllocatorError> {
    with_allocator(|allocator| Ok(allocator.stats()))
}

pub fn allocator_region(index: usize) -> Option<PhysRegion> {
    if !PAGE_ALLOCATOR.initialized.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: plan is immutable after phase8 initialization.
    unsafe {
        (*PAGE_ALLOCATOR.plan.get())
            .layout
            .regions()
            .get(index)
            .copied()
    }
}

pub fn reclaimed_test_page() -> Option<PhysAddr> {
    if !PAGE_ALLOCATOR.initialized.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: plan is immutable after phase8 initialization.
    unsafe { (*PAGE_ALLOCATOR.plan.get()).reclaimed_test_page }
}

fn with_allocator<T>(
    operation: impl FnOnce(&mut PageAllocator<'_, MAX_ALLOCATOR_REGIONS>) -> Result<T, AllocatorError>,
) -> Result<T, AllocatorError> {
    if !PAGE_ALLOCATOR.initialized.load(Ordering::Acquire) {
        return Err(AllocatorError::PageNotManaged);
    }
    let allocated_pages = PAGE_ALLOCATOR.allocated_pages.load(Ordering::Relaxed);
    // SAFETY: Phase 8 is single-core with asynchronous exceptions masked, so
    // allocator operations cannot overlap. The plan is immutable.
    let mut allocator = unsafe {
        PageAllocator::from_existing(
            &(*PAGE_ALLOCATOR.plan.get()).layout,
            &mut *PAGE_ALLOCATOR.bitmap.get(),
            allocated_pages,
        )?
    };
    let result = operation(&mut allocator);
    PAGE_ALLOCATOR
        .allocated_pages
        .store(allocator.stats().allocated_pages, Ordering::Relaxed);
    result
}

fn select_arena<const N: usize>(
    regions: &tvisor_util::system_info::FixedList<PhysRegion, N>,
) -> Result<u64, PrepareError> {
    for region in regions {
        let start =
            align_up(region.start().value(), PAGE_SIZE).ok_or(PrepareError::AddressOverflow)?;
        let end = start
            .checked_add(TABLE_ARENA_SIZE)
            .ok_or(PrepareError::AddressOverflow)?;
        if end <= region.end().value() {
            return Ok(start);
        }
    }
    Err(PrepareError::NoInitialArena)
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

fn validate<const N: usize>(
    tables: &TableSet<'_, N>,
    uart: u64,
    arena_start: u64,
    arena_end: u64,
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
