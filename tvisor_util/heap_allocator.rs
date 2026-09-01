//! Synchronized wrapper around the byte-oriented Rust heap allocator.
//!
//! The wrapper deliberately knows nothing about physical memory. Its caller
//! must provide one writable, virtually contiguous region with a `'static`
//! lifetime. Tvisor's initial implementation satisfies that contract with a
//! contiguous physical allocation under the EL2 identity map.

use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::{NonNull, null_mut},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use linked_list_allocator::Heap;
use spin::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeapError {
    AlreadyInitialized,
    InvalidRegion,
    NotInitialized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapStats {
    /// Start of the virtually contiguous heap arena.
    pub arena_start: usize,
    /// Number of bytes owned by the underlying heap.
    pub arena_bytes: usize,
    /// Bytes currently charged to live allocations, including alignment
    /// padding retained by the underlying allocator.
    pub used_bytes: usize,
    /// Bytes currently available for allocation.
    pub free_bytes: usize,
    /// Number of successful allocations that have not been deallocated.
    pub live_allocations: usize,
    /// Number of allocation attempts rejected because no suitable hole was
    /// available or because the heap was not initialized yet.
    pub failed_allocations: usize,
}

struct HeapState {
    heap: Heap,
    arena_start: usize,
}

impl HeapState {
    const fn empty() -> Self {
        Self {
            heap: Heap::empty(),
            arena_start: 0,
        }
    }
}

/// Tvisor's global Rust heap implementation.
///
/// The spin mutex makes allocator metadata safe against future concurrent CPU
/// access, but it does not mask local interrupts. Allocation therefore remains
/// forbidden in tvisor exception/interrupt handlers: interrupt-side re-entry
/// while interrupted code owns this lock would deadlock.
pub struct TvisorHeap {
    state: Mutex<HeapState>,
    initialized: AtomicBool,
    live_allocations: AtomicUsize,
    failed_allocations: AtomicUsize,
}

impl TvisorHeap {
    pub const fn empty() -> Self {
        Self {
            state: Mutex::new(HeapState::empty()),
            initialized: AtomicBool::new(false),
            live_allocations: AtomicUsize::new(0),
            failed_allocations: AtomicUsize::new(0),
        }
    }

    /// Initialize the heap over one writable, virtually contiguous region.
    ///
    /// # Safety
    ///
    /// `[arena_start, arena_start + arena_bytes)` must be exclusively owned by
    /// this heap, mapped writable for the remainder of execution, and unused
    /// by every other allocator and object.
    pub unsafe fn initialize(
        &self,
        arena_start: usize,
        arena_bytes: usize,
    ) -> Result<(), HeapError> {
        if arena_start == 0
            || arena_bytes < 3 * size_of::<usize>()
            || arena_start.checked_add(arena_bytes).is_none()
        {
            return Err(HeapError::InvalidRegion);
        }

        let mut state = self.state.lock();
        if self.initialized.load(Ordering::Relaxed) {
            return Err(HeapError::AlreadyInitialized);
        }

        // SAFETY: upheld by the caller; the mutex provides exclusive access
        // while linked_list_allocator installs its in-arena metadata.
        unsafe {
            state.heap.init(arena_start as *mut u8, arena_bytes);
        }
        state.arena_start = arena_start;
        self.initialized.store(true, Ordering::Release);
        Ok(())
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    pub fn stats(&self) -> Result<HeapStats, HeapError> {
        if !self.is_initialized() {
            return Err(HeapError::NotInitialized);
        }
        let state = self.state.lock();
        Ok(HeapStats {
            arena_start: state.arena_start,
            arena_bytes: state.heap.size(),
            used_bytes: state.heap.used(),
            free_bytes: state.heap.free(),
            live_allocations: self.live_allocations.load(Ordering::Relaxed),
            failed_allocations: self.failed_allocations.load(Ordering::Relaxed),
        })
    }
}

// SAFETY: every allocation and deallocation is serialized through `state`.
// Initialization publishes the arena with Release ordering before allocation
// can observe it with Acquire ordering.
unsafe impl GlobalAlloc for TvisorHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !self.is_initialized() {
            self.failed_allocations.fetch_add(1, Ordering::Relaxed);
            return null_mut();
        }
        let mut state = self.state.lock();
        match state.heap.allocate_first_fit(layout) {
            Ok(pointer) => {
                self.live_allocations.fetch_add(1, Ordering::Relaxed);
                pointer.as_ptr()
            }
            Err(()) => {
                self.failed_allocations.fetch_add(1, Ordering::Relaxed);
                null_mut()
            }
        }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let Some(pointer) = NonNull::new(pointer) else {
            return;
        };
        if !self.is_initialized() {
            return;
        }
        let mut state = self.state.lock();
        // SAFETY: GlobalAlloc requires the caller to pass a live pointer from
        // this allocator with the same layout used for allocation.
        unsafe { state.heap.deallocate(pointer, layout) };
        self.live_allocations.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use core::alloc::{GlobalAlloc, Layout};

    use super::{HeapError, TvisorHeap};

    #[repr(align(4096))]
    struct TestArena([u8; 64 * 1024]);

    fn arena() -> &'static mut TestArena {
        std::boxed::Box::leak(std::boxed::Box::new(TestArena([0; 64 * 1024])))
    }

    #[test]
    fn rejects_allocation_before_initialization() {
        let heap = TvisorHeap::empty();
        let layout = Layout::from_size_align(64, 16).unwrap();
        // SAFETY: Directly exercising the GlobalAlloc contract.
        assert!(unsafe { heap.alloc(layout) }.is_null());
        assert_eq!(heap.stats(), Err(HeapError::NotInitialized));
    }

    #[test]
    fn allocates_aligned_memory_and_restores_free_space() {
        let heap = TvisorHeap::empty();
        let memory = arena();
        // SAFETY: the leaked arena is exclusive and valid for the test process.
        unsafe {
            heap.initialize(memory.0.as_mut_ptr() as usize, memory.0.len())
                .unwrap();
        }
        let baseline = heap.stats().unwrap();
        let layout = Layout::from_size_align(257, 64).unwrap();
        // SAFETY: Directly exercising the GlobalAlloc contract.
        let pointer = unsafe { heap.alloc(layout) };
        assert!(!pointer.is_null());
        assert_eq!(pointer as usize & 63, 0);
        let active = heap.stats().unwrap();
        assert_eq!(active.live_allocations, 1);
        assert!(active.used_bytes >= layout.size());

        // SAFETY: `pointer` was returned above for this exact layout.
        unsafe { heap.dealloc(pointer, layout) };
        let final_stats = heap.stats().unwrap();
        assert_eq!(final_stats.live_allocations, 0);
        assert_eq!(final_stats.used_bytes, baseline.used_bytes);
        assert_eq!(final_stats.free_bytes, baseline.free_bytes);
    }

    #[test]
    fn coalesces_adjacent_freed_allocations() {
        let heap = TvisorHeap::empty();
        let memory = arena();
        // SAFETY: the leaked arena is exclusive and valid for the test process.
        unsafe {
            heap.initialize(memory.0.as_mut_ptr() as usize, memory.0.len())
                .unwrap();
        }
        let small = Layout::from_size_align(4096, 16).unwrap();
        let mut pointers = [core::ptr::null_mut(); 3];
        for pointer in &mut pointers {
            // SAFETY: Directly exercising the GlobalAlloc contract.
            *pointer = unsafe { heap.alloc(small) };
            assert!(!pointer.is_null());
        }
        // SAFETY: all pointers were allocated with `small`; freeing adjacent
        // allocations allows the underlying address-ordered list to merge.
        unsafe {
            heap.dealloc(pointers[0], small);
            heap.dealloc(pointers[1], small);
        }
        let combined = Layout::from_size_align(7000, 16).unwrap();
        // SAFETY: Directly exercising the GlobalAlloc contract.
        let merged = unsafe { heap.alloc(combined) };
        assert!(!merged.is_null());
        assert_eq!(merged, pointers[0]);
        // SAFETY: all remaining pointers use their original layouts.
        unsafe {
            heap.dealloc(merged, combined);
            heap.dealloc(pointers[2], small);
        }
        assert_eq!(heap.stats().unwrap().live_allocations, 0);
    }
}
