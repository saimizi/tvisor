use core::{arch::asm, fmt};

use tvisor_util::el2_translation::{
    El2RegisterValues, Mapping, MemoryType, PAGE_SIZE, TableSet, TableStorage, TranslationError,
    pa_bits_from_parange, register_values,
};
use tvisor_util::memory_map::MemoryMap;
use tvisor_util::system_info::PhysRegion;

const MAX_TABLE_PAGES: usize = 16;
const TABLE_ARENA_SIZE: u64 = MAX_TABLE_PAGES as u64 * PAGE_SIZE;

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
    Validation,
}

impl From<TranslationError> for PrepareError {
    fn from(error: TranslationError) -> Self {
        Self::Translation(error)
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
    Ok(PreparedTables {
        registers,
        arena_start,
        arena_end,
        used_pages,
    })
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
