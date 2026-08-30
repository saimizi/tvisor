use core::fmt;

use crate::{
    el2_translation::PAGE_SIZE,
    system_info::{FixedList, PhysAddr, PhysRegion, RegionError},
};

pub const MAX_PHYSICAL_ADDRESS: u64 = 1 << 32;
pub const PAGE_BITMAP_BYTES: usize = (MAX_PHYSICAL_ADDRESS / PAGE_SIZE / 8) as usize;

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
    /// Allocator-controlled RAM pages currently occupied by U-Boot or tvisor,
    /// including active EL2 translation-table pages.
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
/// page is InUse or Unused according to the second bitmap.
pub struct PageAllocator<'a, const BITMAP_BYTES: usize> {
    /// Bitmap covering every page in the complete physical-address aperture,
    /// including RAM, MMIO, and unpopulated addresses. A set bit identifies
    /// RAM that the allocator is permitted to manage. A clear bit identifies
    /// reserved RAM, MMIO, or an unpopulated address and is not an
    /// allocator-managed page.
    managed: &'a mut PageBitmap<BITMAP_BYTES>,
    /// Allocation-state bitmap for pages selected by `managed`. A set bit
    /// means `InUse`, while a clear bit means `Unused`; bits for unavailable
    /// pages are ignored.
    in_use: &'a mut PageBitmap<BITMAP_BYTES>,
    /// Total number of RAM-backed pages described by the system information,
    /// including reserved firmware carve-outs.
    ram_pages: usize,
}

/// A bitmap and its cached set-bit count.
///
/// Raw bitmap storage is private so every bit change passes through
/// `set_bit()`, which updates `set_pages` in the same operation. This preserves
/// `set_pages == popcount(bits)` while the allocator has exclusive access.
pub struct PageBitmap<const BYTES: usize> {
    /// Packed page-state bits covering the configured physical aperture.
    bits: [u8; BYTES],
    /// Cached number of set bits in `bits`.
    set_pages: usize,
}

impl<const BYTES: usize> PageBitmap<BYTES> {
    pub const fn zeroed() -> Self {
        Self {
            bits: [0; BYTES],
            set_pages: 0,
        }
    }

    /// Return whether the bit for `index` is set.
    pub fn is_set(&self, index: usize) -> bool {
        bit_is_set(&self.bits, index)
    }

    /// Return the cached number of set bits.
    pub const fn pages(&self) -> usize {
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

impl<'a, const BITMAP_BYTES: usize> PageAllocator<'a, BITMAP_BYTES> {
    pub fn new<const R: usize, const A: usize, const U: usize>(
        ram: &FixedList<PhysRegion, R>,
        allocatable: &FixedList<PhysRegion, A>,
        initially_unused: &FixedList<PhysRegion, U>,
        managed: &'a mut PageBitmap<BITMAP_BYTES>,
        in_use: &'a mut PageBitmap<BITMAP_BYTES>,
    ) -> Result<Self, AllocatorError> {
        validate_bitmaps(managed, in_use)?;
        managed.clear();
        in_use.clear();
        let ram_pages = count_pages(ram)?;
        let mut allocator = Self {
            managed,
            in_use,
            ram_pages,
        };
        for region in allocatable {
            allocator.mark_managed(*region)?;
        }
        allocator.mark_all_managed_in_use();
        for region in initially_unused {
            allocator.release(*region)?;
        }
        Ok(allocator)
    }

    pub fn from_existing(
        managed: &'a mut PageBitmap<BITMAP_BYTES>,
        in_use: &'a mut PageBitmap<BITMAP_BYTES>,
        ram_pages: usize,
    ) -> Result<Self, AllocatorError> {
        validate_bitmaps(managed, in_use)?;
        if ram_pages > BITMAP_BYTES * 8
            || managed.pages() > ram_pages
            || in_use.pages() > managed.pages()
        {
            return Err(AllocatorError::AddressOverflow);
        }
        Ok(Self {
            managed,
            in_use,
            ram_pages,
        })
    }

    pub fn allocate(&mut self) -> Result<PhysAddr, AllocatorError> {
        self.allocate_contiguous(1)
    }

    /// Allocate the highest-addressed currently unused managed page.
    pub fn allocate_high(&mut self) -> Result<PhysAddr, AllocatorError> {
        for index in (0..BITMAP_BYTES * 8).rev() {
            if self.managed.is_set(index) && !self.in_use.is_set(index) {
                self.in_use.set_bit(index, true);
                return Ok(PhysAddr::new(index as u64 * PAGE_SIZE));
            }
        }
        Err(AllocatorError::Exhausted)
    }

    pub fn allocate_contiguous(&mut self, pages: usize) -> Result<PhysAddr, AllocatorError> {
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
                    return Ok(PhysAddr::new(run_start as u64 * PAGE_SIZE));
                }
            } else {
                run_len = 0;
            }
        }
        Err(AllocatorError::Exhausted)
    }

    pub fn allocate_in(&mut self, requested: PhysRegion) -> Result<PhysAddr, AllocatorError> {
        let start = align_up(requested.start().value(), PAGE_SIZE)
            .ok_or(AllocatorError::AddressOverflow)?;
        let end = align_down(requested.end().value(), PAGE_SIZE).min(self.aperture_end());
        let mut page = start;
        while page < end {
            if self.state(PhysAddr::new(page))? == PageState::Unused {
                let index = self.bitmap_index(page)?;
                self.in_use.set_bit(index, true);
                return Ok(PhysAddr::new(page));
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
        let start =
            align_up(region.start().value(), PAGE_SIZE).ok_or(AllocatorError::AddressOverflow)?;
        let end = align_down(region.end().value(), PAGE_SIZE).min(self.aperture_end());
        let mut released = 0;
        let mut page = start;
        while page < end {
            let index = self.bitmap_index(page)?;
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
        if page.value() & (PAGE_SIZE - 1) != 0 {
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

    pub const fn stats(&self) -> AllocatorStats {
        AllocatorStats {
            ram_pages: self.ram_pages,
            reserved_pages: self.ram_pages - self.managed.pages(),
            in_use_pages: self.in_use.pages(),
            unused_pages: self.managed.pages() - self.in_use.pages(),
        }
    }

    fn mark_managed(&mut self, region: PhysRegion) -> Result<(), AllocatorError> {
        let start =
            align_up(region.start().value(), PAGE_SIZE).ok_or(AllocatorError::AddressOverflow)?;
        let end = align_down(region.end().value(), PAGE_SIZE);
        if end > self.aperture_end() {
            return Err(AllocatorError::PhysicalAddressOutOfRange);
        }
        let mut page = start;
        while page < end {
            let index = self.bitmap_index(page)?;
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
        BITMAP_BYTES as u64 * 8 * PAGE_SIZE
    }

    fn bitmap_index(&self, address: u64) -> Result<usize, AllocatorError> {
        let page = usize::try_from(address / PAGE_SIZE)
            .map_err(|_| AllocatorError::PhysicalAddressOutOfRange)?;
        if page >= BITMAP_BYTES * 8 {
            return Err(AllocatorError::PhysicalAddressOutOfRange);
        }
        Ok(page)
    }
}

fn validate_bitmaps<const BYTES: usize>(
    _managed: &PageBitmap<BYTES>,
    _in_use: &PageBitmap<BYTES>,
) -> Result<(), AllocatorError> {
    if BYTES == 0 {
        Err(AllocatorError::PhysicalAddressOutOfRange)
    } else {
        Ok(())
    }
}

fn count_pages<const N: usize>(
    regions: &FixedList<PhysRegion, N>,
) -> Result<usize, AllocatorError> {
    let mut pages = 0_usize;
    for region in regions {
        let start =
            align_up(region.start().value(), PAGE_SIZE).ok_or(AllocatorError::AddressOverflow)?;
        let end = align_down(region.end().value(), PAGE_SIZE);
        if end > MAX_PHYSICAL_ADDRESS {
            return Err(AllocatorError::PhysicalAddressOutOfRange);
        }
        pages = pages
            .checked_add(
                usize::try_from((end - start) / PAGE_SIZE)
                    .map_err(|_| AllocatorError::AddressOverflow)?,
            )
            .ok_or(AllocatorError::AddressOverflow)?;
    }
    Ok(pages)
}

pub fn page_covering(region: PhysRegion) -> Result<PhysRegion, AllocatorError> {
    let start = align_down(region.start().value(), PAGE_SIZE);
    let end = align_up(region.end().value(), PAGE_SIZE).ok_or(AllocatorError::AddressOverflow)?;
    if end > MAX_PHYSICAL_ADDRESS {
        return Err(AllocatorError::PhysicalAddressOutOfRange);
    }
    PhysRegion::from_bounds(PhysAddr::new(start), PhysAddr::new(end))
        .map_err(AllocatorError::InvalidRegion)
}

fn align_down(value: u64, alignment: u64) -> u64 {
    value & !(alignment - 1)
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    value
        .checked_add(alignment - 1)
        .map(|value| align_down(value, alignment))
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
        let (mut managed, mut in_use) = (PageBitmap::<1>::zeroed(), PageBitmap::<1>::zeroed());
        let allocator =
            PageAllocator::new(&ram, &allocatable, &unused, &mut managed, &mut in_use).unwrap();

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
            allocator.stats(),
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
        let (mut managed, mut in_use) = (PageBitmap::<1>::zeroed(), PageBitmap::<1>::zeroed());
        let mut allocator =
            PageAllocator::new(&ram, &allocatable, &unused, &mut managed, &mut in_use).unwrap();

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
        let (mut managed, mut in_use) = (PageBitmap::<1>::zeroed(), PageBitmap::<1>::zeroed());
        let mut allocator =
            PageAllocator::new(&ram, &allocatable, &unused, &mut managed, &mut in_use).unwrap();

        assert_eq!(allocator.allocate_high(), Ok(PhysAddr::new(0x6000)));
        assert_eq!(allocator.allocate_high(), Ok(PhysAddr::new(0x5000)));
    }

    #[test]
    fn reclaims_in_use_pages_but_not_unavailable_addresses() {
        let mut ram = FixedList::<_, 1>::new();
        ram.push(region(0x1000, 0x5000)).unwrap();
        let mut allocatable = FixedList::<_, 1>::new();
        allocatable.push(region(0x2000, 0x4000)).unwrap();
        let (mut managed, mut in_use) = (PageBitmap::<1>::zeroed(), PageBitmap::<1>::zeroed());
        let mut allocator = PageAllocator::new(
            &ram,
            &allocatable,
            &FixedList::<PhysRegion, 0>::new(),
            &mut managed,
            &mut in_use,
        )
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
        let (mut managed, mut in_use) = (PageBitmap::<1>::zeroed(), PageBitmap::<1>::zeroed());
        let mut allocator =
            PageAllocator::new(&ram, &allocatable, &unused, &mut managed, &mut in_use).unwrap();

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
        let (mut managed, mut in_use) = (PageBitmap::<1>::zeroed(), PageBitmap::<1>::zeroed());
        let allocator = PageAllocator::new(&ram, &ram, &ram, &mut managed, &mut in_use).unwrap();
        assert_eq!(
            allocator.state(PhysAddr::new(0x8000)),
            Err(AllocatorError::PhysicalAddressOutOfRange)
        );
    }

    #[test]
    fn page_cover_rounds_outward() {
        assert_eq!(
            page_covering(region(0x3100, 0x100)).unwrap(),
            region(0x3000, 0x1000)
        );
    }
}
