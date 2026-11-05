use crate::*;
use core::fmt;

use crate::{
    PAGE_SIZE,
    system_info::{FixedList, PhysAddr, PhysRegion, RegionError},
};

pub const MAX_PHYSICAL_ADDRESS: u64 = 1 << 32;
pub const PAGE_BITMAP_BYTES: usize = (MAX_PHYSICAL_ADDRESS as usize) / PAGE_SIZE / 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageState {
    Reserved,
    InUse,
    Unused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocatorError {
    AddressOverflow,
    NotInitialized,
    InvalidRegion(RegionError),
    PhysicalAddressOutOfRange,
    Exhausted,
    UnalignedPage,
    ReservedPage,
    DoubleFree,
    AlreadyAllocated,
    InvalidateParameter,
    InvalidIpa,
    OverlappingIpa,
    Unexpected,
}

impl fmt::Display for AllocatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocatorStats {
    /// Total number of page-aligned 4 KiB pages in `MemoryMap::ram()`, including
    /// firmware carve-outs but excluding MMIO and unpopulated addresses.
    pub ram_pages: usize,
    /// RAM pages that are permanently unavailable to the allocator, such as
    /// firmware carve-outs, the tvisor image, and other non-reclaimable
    /// reservations.
    pub reserved_pages: usize,
    /// Allocator-controlled RAM pages currently allocated to tvisor objects,
    /// such as guest backing pages and stage-2 translation tables.
    pub in_use_pages: usize,
    /// Allocator-controlled RAM pages that can be returned by an allocation.
    pub unused_pages: usize,
}

/// A flat three-state physical-page allocator.
///
/// Both bitmaps cover the complete configured physical aperture. A managed
/// bit distinguishes allocator-controlled RAM from unavailable addresses. The
/// latter include reserved RAM, MMIO, firmware carve-outs, and unpopulated
/// space; their semantic classification remains in `MemoryMap`. A managed
/// page is InUse or Unused according to the second bitmap. The allocator owns
/// its storage; callers provide synchronization when sharing it across CPUs.
pub struct PageAllocator<const BITMAP_BYTES: usize> {
    /// Bitmap covering every page in the complete physical-address aperture,
    /// including RAM, MMIO, and unpopulated addresses. A set bit identifies
    /// RAM that the allocator is permitted to manage. A clear bit identifies
    /// reserved RAM, MMIO, or an unpopulated address and is not an
    /// allocator-managed page.
    managed: PageBitmap<BITMAP_BYTES>,
    /// Allocation-state bitmap for pages selected by `managed`. A set bit
    /// means `InUse`, while a clear bit means `Unused`; bits for unavailable
    /// pages are ignored.
    in_use: PageBitmap<BITMAP_BYTES>,
    /// Total number of RAM-backed pages described by the system information,
    /// including reserved firmware carve-outs.
    ram_pages: usize,
    initialized: bool,
}

/// A bitmap and its cached set-bit count.
///
/// Raw bitmap storage is private so every bit change passes through
/// `set_bit()`, which updates `set_pages` in the same operation. This preserves
/// `set_pages == popcount(bits)` while the allocator has exclusive access.
struct PageBitmap<const BYTES: usize> {
    /// Packed page-state bits covering the configured physical aperture.
    bits: [u8; BYTES],
    /// Cached number of set bits in `bits`.
    set_pages: usize,
}

impl<const BYTES: usize> PageBitmap<BYTES> {
    const fn zeroed() -> Self {
        Self {
            bits: [0; BYTES],
            set_pages: 0,
        }
    }

    /// Return whether the bit for `index` is set.
    fn is_set(&self, index: usize) -> bool {
        bit_is_set(&self.bits, index)
    }

    /// Return the cached number of set bits.
    const fn pages(&self) -> usize {
        self.set_pages
    }

    fn clear(&mut self) {
        self.bits.fill(0);
        self.set_pages = 0;
    }

    fn set_bit(&mut self, index: usize, value: bool) {
        let old = self.is_set(index);
        if old == value {
            return;
        }
        write_bit(&mut self.bits, index, value);
        if value {
            self.set_pages += 1;
        } else {
            self.set_pages -= 1;
        }
    }
}

impl<const BITMAP_BYTES: usize> PageAllocator<BITMAP_BYTES> {
    /// Construct static storage at compile time. Initialize large allocators
    /// in place rather than constructing a replacement on a runtime stack.
    pub const fn uninitialized() -> Self {
        Self {
            managed: PageBitmap::zeroed(),
            in_use: PageBitmap::zeroed(),
            ram_pages: 0,
            initialized: false,
        }
    }

    pub const fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Populate the owned bitmaps in place. A failed attempt stays unavailable
    /// and can be retried; a successful initialization cannot be repeated.
    pub fn initialize<const R: usize, const A: usize, const U: usize>(
        &mut self,
        ram: &FixedList<PhysRegion, R>,
        allocatable: &FixedList<PhysRegion, A>,
        initially_unused: &FixedList<PhysRegion, U>,
    ) -> Result<(), AllocatorError> {
        if self.initialized {
            // Preserve the global allocator's existing repeated-init error.
            return Err(AllocatorError::DoubleFree);
        }
        if BITMAP_BYTES == 0 {
            return Err(AllocatorError::PhysicalAddressOutOfRange);
        }
        self.managed.clear();
        self.in_use.clear();
        self.ram_pages = count_pages(ram)?;
        for region in allocatable {
            self.mark_managed(*region)?;
        }
        self.mark_all_managed_in_use();
        for region in initially_unused {
            self.release_managed(*region)?;
        }
        if self.ram_pages > BITMAP_BYTES * 8
            || self.managed.pages() > self.ram_pages
            || self.in_use.pages() > self.managed.pages()
        {
            return Err(AllocatorError::AddressOverflow);
        }
        self.initialized = true;
        Ok(())
    }

    pub fn allocate(&mut self) -> Result<PhysAddr, AllocatorError> {
        self.allocate_contiguous(1)
    }

    /// Allocate the highest-addressed currently unused managed page.
    pub fn allocate_high(&mut self) -> Result<PhysAddr, AllocatorError> {
        self.ensure_initialized()?;
        for index in (0..BITMAP_BYTES * 8).rev() {
            if self.managed.is_set(index) && !self.in_use.is_set(index) {
                self.in_use.set_bit(index, true);
                return Ok(PhysAddr::new((index * PAGE_SIZE) as u64));
            }
        }
        Err(AllocatorError::Exhausted)
    }

    pub fn allocate_contiguous(&mut self, pages: usize) -> Result<PhysAddr, AllocatorError> {
        self.ensure_initialized()?;
        if pages == 0 {
            return Err(AllocatorError::Exhausted);
        }
        let (mut run_start, mut run_len) = (0, 0);
        for index in 0..BITMAP_BYTES * 8 {
            if self.managed.is_set(index) && !self.in_use.is_set(index) {
                if run_len == 0 {
                    run_start = index;
                }
                run_len += 1;
                if run_len == pages {
                    for page in run_start..run_start + pages {
                        self.in_use.set_bit(page, true);
                    }
                    return Ok(PhysAddr::new((run_start * PAGE_SIZE) as u64));
                }
            } else {
                run_len = 0;
            }
        }
        Err(AllocatorError::Exhausted)
    }

    pub fn allocate_in(&mut self, requested: PhysRegion) -> Result<PhysAddr, AllocatorError> {
        self.ensure_initialized()?;
        let start = align_up(requested.start().value() as usize, PAGE_SIZE)
            .ok_or(AllocatorError::AddressOverflow)?;
        let end = align_down(requested.end().value() as usize, PAGE_SIZE)
            .ok_or(AllocatorError::AddressOverflow)?
            .min(self.aperture_end() as usize);
        let mut page = start;
        while page < end {
            if self.state(PhysAddr::new(page as u64))? == PageState::Unused {
                let index = self.bitmap_index(page as u64)?;
                self.in_use.set_bit(index, true);
                return Ok(PhysAddr::new(page as u64));
            }
            page = page
                .checked_add(PAGE_SIZE)
                .ok_or(AllocatorError::AddressOverflow)?;
        }
        Err(AllocatorError::Exhausted)
    }

    pub fn free(&mut self, page: PhysAddr) -> Result<(), AllocatorError> {
        match self.state(page)? {
            PageState::Reserved => Err(AllocatorError::ReservedPage),
            PageState::Unused => Err(AllocatorError::DoubleFree),
            PageState::InUse => {
                let index = self.bitmap_index(page.value())?;
                self.in_use.set_bit(index, false);
                Ok(())
            }
        }
    }

    /// Change managed pages fully covered by `region` to Unused.
    pub fn release(&mut self, region: PhysRegion) -> Result<usize, AllocatorError> {
        self.ensure_initialized()?;
        self.release_managed(region)
    }

    fn release_managed(&mut self, region: PhysRegion) -> Result<usize, AllocatorError> {
        let start = align_up(region.start().value() as usize, PAGE_SIZE)
            .ok_or(AllocatorError::AddressOverflow)?;
        let end = align_down(region.end().value() as usize, PAGE_SIZE)
            .ok_or(AllocatorError::AddressOverflow)?
            .min(self.aperture_end() as usize);
        let mut released = 0;
        let mut page = start;
        while page < end {
            let index = self.bitmap_index(page as u64)?;
            if self.managed.is_set(index) && self.in_use.is_set(index) {
                self.in_use.set_bit(index, false);
                released += 1;
            }
            page = page
                .checked_add(PAGE_SIZE)
                .ok_or(AllocatorError::AddressOverflow)?;
        }
        Ok(released)
    }

    pub fn state(&self, page: PhysAddr) -> Result<PageState, AllocatorError> {
        self.ensure_initialized()?;
        if !is_page_aligned(page.value() as usize) {
            return Err(AllocatorError::UnalignedPage);
        }
        let index = self.bitmap_index(page.value())?;
        if !self.managed.is_set(index) {
            Ok(PageState::Reserved)
        } else if self.in_use.is_set(index) {
            Ok(PageState::InUse)
        } else {
            Ok(PageState::Unused)
        }
    }

    pub fn stats(&self) -> Result<AllocatorStats, AllocatorError> {
        self.ensure_initialized()?;
        Ok(AllocatorStats {
            ram_pages: self.ram_pages,
            reserved_pages: self.ram_pages - self.managed.pages(),
            in_use_pages: self.in_use.pages(),
            unused_pages: self.managed.pages() - self.in_use.pages(),
        })
    }

    fn ensure_initialized(&self) -> Result<(), AllocatorError> {
        if self.initialized {
            Ok(())
        } else {
            Err(AllocatorError::NotInitialized)
        }
    }

    fn mark_managed(&mut self, region: PhysRegion) -> Result<(), AllocatorError> {
        let start = align_up(region.start().value() as usize, PAGE_SIZE)
            .ok_or(AllocatorError::AddressOverflow)?;
        let end = align_down(region.end().value() as usize, PAGE_SIZE)
            .ok_or(AllocatorError::AddressOverflow)?;
        if end > self.aperture_end() as usize {
            return Err(AllocatorError::PhysicalAddressOutOfRange);
        }
        let mut page = start;
        while page < end {
            let index = self.bitmap_index(page as u64)?;
            self.managed.set_bit(index, true);
            page = page
                .checked_add(PAGE_SIZE)
                .ok_or(AllocatorError::AddressOverflow)?;
        }
        Ok(())
    }

    fn mark_all_managed_in_use(&mut self) {
        for index in 0..BITMAP_BYTES * 8 {
            if self.managed.is_set(index) {
                self.in_use.set_bit(index, true);
            }
        }
    }

    fn aperture_end(&self) -> u64 {
        (BITMAP_BYTES * 8 * PAGE_SIZE) as u64
    }

    fn bitmap_index(&self, address: u64) -> Result<usize, AllocatorError> {
        let page = usize::try_from(address / PAGE_SIZE as u64)
            .map_err(|_| AllocatorError::PhysicalAddressOutOfRange)?;
        if page >= BITMAP_BYTES * 8 {
            return Err(AllocatorError::PhysicalAddressOutOfRange);
        }
        Ok(page)
    }
}

fn count_pages<const N: usize>(
    regions: &FixedList<PhysRegion, N>,
) -> Result<usize, AllocatorError> {
    let mut pages = 0_usize;
    for region in regions {
        let start = align_up(region.start().value() as usize, PAGE_SIZE)
            .ok_or(AllocatorError::AddressOverflow)?;
        let end = align_down(region.end().value() as usize, PAGE_SIZE)
            .ok_or(AllocatorError::AddressOverflow)?;
        if end as u64 > MAX_PHYSICAL_ADDRESS {
            return Err(AllocatorError::PhysicalAddressOutOfRange);
        }
        pages = pages
            .checked_add((end - start) / PAGE_SIZE)
            .ok_or(AllocatorError::AddressOverflow)?;
    }
    Ok(pages)
}

fn bit_is_set(bitmap: &[u8], index: usize) -> bool {
    bitmap[index / 8] & (1 << (index % 8)) != 0
}

fn write_bit(bitmap: &mut [u8], index: usize, value: bool) {
    let mask = 1 << (index % 8);
    if value {
        bitmap[index / 8] |= mask;
    } else {
        bitmap[index / 8] &= !mask;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(start: u64, size: u64) -> PhysRegion {
        PhysRegion::new(PhysAddr::new(start), size).unwrap()
    }

    #[test]
    fn uninitialized_allocator_rejects_every_page_operation() {
        let mut allocator = PageAllocator::<1>::uninitialized();
        let page = PhysAddr::new(0);
        assert!(!allocator.is_initialized());
        assert_eq!(allocator.allocate(), Err(AllocatorError::NotInitialized));
        assert_eq!(
            allocator.allocate_high(),
            Err(AllocatorError::NotInitialized)
        );
        assert_eq!(
            allocator.allocate_contiguous(0),
            Err(AllocatorError::NotInitialized)
        );
        assert_eq!(
            allocator.allocate_in(region(0, 4096)),
            Err(AllocatorError::NotInitialized)
        );
        assert_eq!(allocator.free(page), Err(AllocatorError::NotInitialized));
        assert_eq!(
            allocator.release(region(0, 4096)),
            Err(AllocatorError::NotInitialized)
        );
        assert_eq!(allocator.state(page), Err(AllocatorError::NotInitialized));
        assert_eq!(allocator.stats(), Err(AllocatorError::NotInitialized));
    }

    #[test]
    fn failed_initialization_stays_unavailable_and_retry_clears_partial_state() {
        let mut ram = FixedList::<_, 1>::new();
        ram.push(region(0, 0x8000)).unwrap();
        let mut invalid = FixedList::<_, 2>::new();
        invalid.push(region(0, 0x2000)).unwrap();
        invalid.push(region(0x8000, 0x1000)).unwrap();
        let mut allocator = PageAllocator::<1>::uninitialized();
        assert_eq!(
            allocator.initialize(&ram, &invalid, &ram),
            Err(AllocatorError::PhysicalAddressOutOfRange)
        );
        assert!(!allocator.is_initialized());
        assert_eq!(allocator.allocate(), Err(AllocatorError::NotInitialized));
        assert_eq!(allocator.stats(), Err(AllocatorError::NotInitialized));

        let mut usable = FixedList::<_, 1>::new();
        usable.push(region(0x4000, 0x1000)).unwrap();
        allocator.initialize(&ram, &usable, &usable).unwrap();
        assert_eq!(allocator.state(PhysAddr::new(0)), Ok(PageState::Reserved));
        assert_eq!(allocator.stats().unwrap().unused_pages, 1);
        assert_eq!(allocator.allocate(), Ok(PhysAddr::new(0x4000)));
        assert_eq!(allocator.allocate(), Err(AllocatorError::Exhausted));
    }

    #[test]
    fn repeated_initialization_preserves_live_allocations() {
        let mut ram = FixedList::<_, 1>::new();
        ram.push(region(0, 0x8000)).unwrap();
        let mut allocator = PageAllocator::<1>::uninitialized();
        allocator.initialize(&ram, &ram, &ram).unwrap();
        let page = allocator.allocate().unwrap();
        let before = allocator.stats().unwrap();
        assert_eq!(
            allocator.initialize(&ram, &ram, &ram),
            Err(AllocatorError::DoubleFree)
        );
        assert!(allocator.is_initialized());
        assert_eq!(allocator.stats().unwrap(), before);
        assert_eq!(allocator.state(page), Ok(PageState::InUse));
        assert_eq!(allocator.allocate(), Ok(PhysAddr::new(4096)));
    }

    #[test]
    fn invalid_capacity_and_counts_cannot_publish_an_allocator() {
        let mut ram = FixedList::<_, 1>::new();
        ram.push(region(0, 0x9000)).unwrap();
        let empty = FixedList::<PhysRegion, 0>::new();
        let mut zero = PageAllocator::<0>::uninitialized();
        assert_eq!(
            zero.initialize(&ram, &empty, &empty),
            Err(AllocatorError::PhysicalAddressOutOfRange)
        );
        assert_eq!(zero.stats(), Err(AllocatorError::NotInitialized));
        let mut allocator = PageAllocator::<1>::uninitialized();
        assert_eq!(
            allocator.initialize(&ram, &empty, &empty),
            Err(AllocatorError::AddressOverflow)
        );
        assert_eq!(allocator.stats(), Err(AllocatorError::NotInitialized));

        let mut usable = FixedList::<_, 1>::new();
        usable.push(region(0, 4096)).unwrap();
        assert_eq!(
            allocator.initialize(&empty, &usable, &usable),
            Err(AllocatorError::AddressOverflow)
        );
        assert_eq!(allocator.stats(), Err(AllocatorError::NotInitialized));
    }

    #[test]
    fn counted_bitmap_keeps_set_page_count_consistent() {
        let mut bitmap = PageBitmap::<1>::zeroed();
        assert!(!bitmap.is_set(3));
        assert_eq!(bitmap.pages(), 0);

        bitmap.set_bit(3, true);
        assert!(bitmap.is_set(3));
        assert_eq!(bitmap.pages(), 1);

        bitmap.set_bit(3, true);
        assert_eq!(bitmap.pages(), 1);

        bitmap.set_bit(3, false);
        assert!(!bitmap.is_set(3));
        assert_eq!(bitmap.pages(), 0);
    }

    #[test]
    fn represents_reserved_in_use_and_unused_pages_in_flat_aperture() {
        let mut ram = FixedList::<_, 2>::new();
        ram.push(region(0x1000, 0x5000)).unwrap();
        let mut allocatable = FixedList::<_, 1>::new();
        allocatable.push(region(0x2000, 0x4000)).unwrap();
        let mut unused = FixedList::<_, 1>::new();
        unused.push(region(0x2000, 0x2000)).unwrap();
        let mut allocator = PageAllocator::<1>::uninitialized();
        allocator.initialize(&ram, &allocatable, &unused).unwrap();

        assert_eq!(allocator.state(PhysAddr::new(0)), Ok(PageState::Reserved));
        assert_eq!(
            allocator.state(PhysAddr::new(0x1000)),
            Ok(PageState::Reserved)
        );
        assert_eq!(
            allocator.state(PhysAddr::new(0x2000)),
            Ok(PageState::Unused)
        );
        assert_eq!(allocator.state(PhysAddr::new(0x4000)), Ok(PageState::InUse));
        assert_eq!(
            allocator.stats().unwrap(),
            AllocatorStats {
                ram_pages: 5,
                reserved_pages: 1,
                in_use_pages: 2,
                unused_pages: 2
            }
        );
    }

    #[test]
    fn reserved_holes_stop_contiguous_allocations() {
        let mut ram = FixedList::<_, 1>::new();
        ram.push(region(0, 0x8000)).unwrap();
        let mut allocatable = FixedList::<_, 2>::new();
        allocatable.push(region(0x1000, 0x2000)).unwrap();
        allocatable.push(region(0x4000, 0x3000)).unwrap();
        let unused = allocatable;
        let mut allocator = PageAllocator::<1>::uninitialized();
        allocator.initialize(&ram, &allocatable, &unused).unwrap();

        assert_eq!(allocator.allocate_contiguous(3), Ok(PhysAddr::new(0x4000)));
        assert_eq!(allocator.allocate_contiguous(2), Ok(PhysAddr::new(0x1000)));
        assert_eq!(allocator.allocate(), Err(AllocatorError::Exhausted));
    }

    #[test]
    fn allocates_highest_unused_managed_page() {
        let mut ram = FixedList::<_, 1>::new();
        ram.push(region(0, 0x8000)).unwrap();
        let mut allocatable = FixedList::<_, 2>::new();
        allocatable.push(region(0x1000, 0x2000)).unwrap();
        allocatable.push(region(0x5000, 0x2000)).unwrap();
        let unused = allocatable;
        let mut allocator = PageAllocator::<1>::uninitialized();
        allocator.initialize(&ram, &allocatable, &unused).unwrap();

        assert_eq!(allocator.allocate_high(), Ok(PhysAddr::new(0x6000)));
        assert_eq!(allocator.allocate_high(), Ok(PhysAddr::new(0x5000)));
    }

    #[test]
    fn reclaims_in_use_pages_but_not_unavailable_addresses() {
        let mut ram = FixedList::<_, 1>::new();
        ram.push(region(0x1000, 0x5000)).unwrap();
        let mut allocatable = FixedList::<_, 1>::new();
        allocatable.push(region(0x2000, 0x4000)).unwrap();
        let mut allocator = PageAllocator::<1>::uninitialized();
        allocator
            .initialize(&ram, &allocatable, &FixedList::<PhysRegion, 0>::new())
            .unwrap();

        assert_eq!(allocator.release(region(0x1000, 0x3000)), Ok(2));
        assert_eq!(
            allocator.state(PhysAddr::new(0x1000)),
            Ok(PageState::Reserved)
        );
        assert_eq!(
            allocator.state(PhysAddr::new(0x2000)),
            Ok(PageState::Unused)
        );
        assert_eq!(allocator.state(PhysAddr::new(0x4000)), Ok(PageState::InUse));
    }

    #[test]
    fn rejects_invalid_and_double_frees() {
        let mut ram = FixedList::<_, 1>::new();
        ram.push(region(0x1000, 0x3000)).unwrap();
        let mut allocatable = FixedList::<_, 1>::new();
        allocatable.push(region(0x2000, 0x2000)).unwrap();
        let mut unused = FixedList::<_, 1>::new();
        unused.push(region(0x2000, 0x1000)).unwrap();
        let mut allocator = PageAllocator::<1>::uninitialized();
        allocator.initialize(&ram, &allocatable, &unused).unwrap();

        assert_eq!(
            allocator.free(PhysAddr::new(0x2001)),
            Err(AllocatorError::UnalignedPage)
        );
        assert_eq!(
            allocator.free(PhysAddr::new(0x1000)),
            Err(AllocatorError::ReservedPage)
        );
        assert_eq!(
            allocator.free(PhysAddr::new(0x2000)),
            Err(AllocatorError::DoubleFree)
        );
        assert_eq!(allocator.free(PhysAddr::new(0x3000)), Ok(()));
    }

    #[test]
    fn rejects_addresses_outside_bitmap_aperture() {
        let mut ram = FixedList::<_, 1>::new();
        ram.push(region(0, 0x1000)).unwrap();
        let mut allocator = PageAllocator::<1>::uninitialized();
        allocator.initialize(&ram, &ram, &ram).unwrap();
        assert_eq!(
            allocator.state(PhysAddr::new(0x8000)),
            Err(AllocatorError::PhysicalAddressOutOfRange)
        );
    }
}
