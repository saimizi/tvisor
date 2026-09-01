//! Global Rust heap backed by a fixed identity-mapped physical-page arena.

use core::fmt;

use tvisor_util::{
    el2_translation::PAGE_SIZE,
    heap_allocator::{HeapError, HeapStats, TvisorHeap},
    page_allocator::AllocatorError,
    system_info::PhysAddr,
};

use crate::mm;

/// One MiB is deliberately a policy constant for the first heap iteration.
/// The arena does not grow or return pages to the physical allocator.
pub const INITIAL_HEAP_PAGES: usize = 256;
pub const INITIAL_HEAP_BYTES: usize = INITIAL_HEAP_PAGES * PAGE_SIZE as usize;

#[global_allocator]
static GLOBAL_HEAP: TvisorHeap = TvisorHeap::empty();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeapInitError {
    Allocator(AllocatorError),
    Heap(HeapError),
    AddressOverflow,
}

impl From<AllocatorError> for HeapInitError {
    fn from(error: AllocatorError) -> Self {
        Self::Allocator(error)
    }
}

impl From<HeapError> for HeapInitError {
    fn from(error: HeapError) -> Self {
        Self::Heap(error)
    }
}

impl fmt::Display for HeapInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitializedHeap {
    pub arena_start: PhysAddr,
    pub arena_end: PhysAddr,
    pub stats: HeapStats,
}

/// Reserve the fixed initial arena from the global physical-page allocator and
/// make it available to Rust's `alloc` crate.
///
/// This must run only after takeover under tvisor's identity-mapped EL2 stage-1
/// tables. Consequently, the physical allocation is also a virtually
/// contiguous region at the same numeric address.
pub fn initialize() -> Result<InitializedHeap, HeapInitError> {
    let arena_start = mm::allocate_contiguous_pages(INITIAL_HEAP_PAGES)?;
    let arena_end = arena_start
        .checked_add(INITIAL_HEAP_BYTES as u64)
        .ok_or(HeapInitError::AddressOverflow)?;
    let arena_va =
        usize::try_from(arena_start.value()).map_err(|_| HeapInitError::AddressOverflow)?;

    // SAFETY: the physical allocator exclusively owns all arena pages for the
    // lifetime of tvisor, and the active EL2 identity map covers usable RAM as
    // writable Normal memory. No page is returned to the physical allocator.
    unsafe { GLOBAL_HEAP.initialize(arena_va, INITIAL_HEAP_BYTES)? };

    Ok(InitializedHeap {
        arena_start,
        arena_end,
        stats: GLOBAL_HEAP.stats()?,
    })
}

pub fn stats() -> Result<HeapStats, HeapError> {
    GLOBAL_HEAP.stats()
}
