use core::fmt;

use crate::{
    el2_translation::PAGE_SIZE,
    system_info::{FixedList, PhysAddr, PhysRegion, RegionError},
};

pub const MAX_PHYSICAL_ADDRESS: u64 = 1 << 32;
pub const PAGE_BITMAP_BYTES: usize = (MAX_PHYSICAL_ADDRESS / PAGE_SIZE / 8) as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocatorError {
    Capacity,
    AddressOverflow,
    InvalidRegion(RegionError),
    PhysicalAddressOutOfRange,
    Exhausted,
    UnalignedPage,
    PageNotManaged,
    DoubleFree,
}

impl fmt::Display for AllocatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocatorStats {
    pub total_pages: usize,
    pub allocated_pages: usize,
    pub free_pages: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocatorLayout<const N: usize> {
    regions: FixedList<PhysRegion, N>,
    total_pages: usize,
}

impl<const N: usize> AllocatorLayout<N> {
    pub const fn empty() -> Self {
        Self {
            regions: FixedList::new(),
            total_pages: 0,
        }
    }

    pub fn from_regions_excluding<const U: usize, const E: usize>(
        usable: &FixedList<PhysRegion, U>,
        exclusions: &FixedList<PhysRegion, E>,
    ) -> Result<Self, AllocatorError> {
        let mut regions = FixedList::new();
        let mut total_pages = 0_usize;

        for usable_region in usable {
            let start = align_up(usable_region.start().value(), PAGE_SIZE)
                .ok_or(AllocatorError::AddressOverflow)?;
            let end = align_down(usable_region.end().value(), PAGE_SIZE);
            if start >= end {
                continue;
            }

            let mut cursor = start;
            while cursor < end {
                let next = exclusions
                    .iter()
                    .filter_map(|excluded| {
                        let excluded_start = align_down(excluded.start().value(), PAGE_SIZE);
                        let excluded_end = align_up(excluded.end().value(), PAGE_SIZE)?;
                        (excluded_end > cursor && excluded_start < end)
                            .then_some((excluded_start, excluded_end))
                    })
                    .min_by_key(|(excluded_start, _)| *excluded_start);

                let Some((excluded_start, excluded_end)) = next else {
                    push_region(&mut regions, cursor, end, &mut total_pages)?;
                    break;
                };

                if excluded_start > cursor {
                    push_region(
                        &mut regions,
                        cursor,
                        excluded_start.min(end),
                        &mut total_pages,
                    )?;
                }
                cursor = cursor.max(excluded_end).min(end);
            }
        }

        Ok(Self {
            regions,
            total_pages,
        })
    }

    pub const fn regions(&self) -> &FixedList<PhysRegion, N> {
        &self.regions
    }

    pub const fn total_pages(&self) -> usize {
        self.total_pages
    }

    pub fn contains(&self, address: PhysAddr) -> bool {
        self.regions
            .iter()
            .any(|region| region.contains_address(address))
    }

    pub fn first_page_in<const R: usize>(
        &self,
        candidates: &FixedList<PhysRegion, R>,
    ) -> Option<PhysAddr> {
        for region in &self.regions {
            for candidate in candidates {
                let start = region.start().value().max(candidate.start().value());
                let end = region.end().value().min(candidate.end().value());
                let page = align_up(start, PAGE_SIZE)?;
                if page.checked_add(PAGE_SIZE)? <= end {
                    return Some(PhysAddr::new(page));
                }
            }
        }
        None
    }
}

pub struct PageAllocator<'a, const N: usize> {
    layout: &'a AllocatorLayout<N>,
    bitmap: &'a mut [u8],
    allocated_pages: usize,
}

impl<'a, const N: usize> PageAllocator<'a, N> {
    pub fn new(
        layout: &'a AllocatorLayout<N>,
        bitmap: &'a mut [u8],
    ) -> Result<Self, AllocatorError> {
        let aperture_pages = bitmap
            .len()
            .checked_mul(8)
            .ok_or(AllocatorError::AddressOverflow)?;
        for region in layout.regions() {
            let end_page = usize::try_from(region.end().value() / PAGE_SIZE)
                .map_err(|_| AllocatorError::PhysicalAddressOutOfRange)?;
            if end_page > aperture_pages {
                return Err(AllocatorError::PhysicalAddressOutOfRange);
            }
        }
        bitmap.fill(0);
        Ok(Self {
            layout,
            bitmap,
            allocated_pages: 0,
        })
    }

    pub fn from_existing(
        layout: &'a AllocatorLayout<N>,
        bitmap: &'a mut [u8],
        allocated_pages: usize,
    ) -> Result<Self, AllocatorError> {
        if allocated_pages > layout.total_pages() {
            return Err(AllocatorError::AddressOverflow);
        }
        let mut allocator = Self {
            layout,
            bitmap,
            allocated_pages,
        };
        allocator.validate_aperture()?;
        Ok(allocator)
    }

    pub fn allocate(&mut self) -> Result<PhysAddr, AllocatorError> {
        for index in 0..self.layout.regions().len() {
            match self.allocate_in_region(index) {
                Ok(page) => return Ok(page),
                Err(AllocatorError::Exhausted) => {}
                Err(error) => return Err(error),
            }
        }
        Err(AllocatorError::Exhausted)
    }

    pub fn allocate_in_region(&mut self, index: usize) -> Result<PhysAddr, AllocatorError> {
        let region = self
            .layout
            .regions()
            .get(index)
            .ok_or(AllocatorError::PageNotManaged)?;
        self.allocate_in(*region)
    }

    pub fn allocate_in(&mut self, requested: PhysRegion) -> Result<PhysAddr, AllocatorError> {
        for region in self.layout.regions() {
            let start = align_up(
                region.start().value().max(requested.start().value()),
                PAGE_SIZE,
            )
            .ok_or(AllocatorError::AddressOverflow)?;
            let end = region.end().value().min(requested.end().value());
            let mut page = start;
            while page.checked_add(PAGE_SIZE).is_some_and(|next| next <= end) {
                let index = self.bitmap_index(page)?;
                if !bit_is_set(self.bitmap, index) {
                    set_bit(self.bitmap, index, true);
                    self.allocated_pages += 1;
                    return Ok(PhysAddr::new(page));
                }
                page = page
                    .checked_add(PAGE_SIZE)
                    .ok_or(AllocatorError::AddressOverflow)?;
            }
        }
        Err(AllocatorError::Exhausted)
    }

    pub fn free(&mut self, page: PhysAddr) -> Result<(), AllocatorError> {
        if page.value() & (PAGE_SIZE - 1) != 0 {
            return Err(AllocatorError::UnalignedPage);
        }
        if !self.layout.contains(page) {
            return Err(AllocatorError::PageNotManaged);
        }
        let index = self.bitmap_index(page.value())?;
        if !bit_is_set(self.bitmap, index) {
            return Err(AllocatorError::DoubleFree);
        }
        set_bit(self.bitmap, index, false);
        self.allocated_pages -= 1;
        Ok(())
    }

    pub const fn stats(&self) -> AllocatorStats {
        AllocatorStats {
            total_pages: self.layout.total_pages,
            allocated_pages: self.allocated_pages,
            free_pages: self.layout.total_pages - self.allocated_pages,
        }
    }

    fn bitmap_index(&self, address: u64) -> Result<usize, AllocatorError> {
        let page = usize::try_from(address / PAGE_SIZE)
            .map_err(|_| AllocatorError::PhysicalAddressOutOfRange)?;
        if page >= self.bitmap.len() * 8 {
            return Err(AllocatorError::PhysicalAddressOutOfRange);
        }
        Ok(page)
    }

    fn validate_aperture(&mut self) -> Result<(), AllocatorError> {
        let aperture_pages = self
            .bitmap
            .len()
            .checked_mul(8)
            .ok_or(AllocatorError::AddressOverflow)?;
        for region in self.layout.regions() {
            let end_page = usize::try_from(region.end().value() / PAGE_SIZE)
                .map_err(|_| AllocatorError::PhysicalAddressOutOfRange)?;
            if end_page > aperture_pages {
                return Err(AllocatorError::PhysicalAddressOutOfRange);
            }
        }
        Ok(())
    }
}

fn push_region<const N: usize>(
    output: &mut FixedList<PhysRegion, N>,
    start: u64,
    end: u64,
    total_pages: &mut usize,
) -> Result<(), AllocatorError> {
    if start >= end {
        return Ok(());
    }
    let region = PhysRegion::from_bounds(PhysAddr::new(start), PhysAddr::new(end))
        .map_err(AllocatorError::InvalidRegion)?;
    output.push(region).map_err(|_| AllocatorError::Capacity)?;
    let pages =
        usize::try_from(region.size() / PAGE_SIZE).map_err(|_| AllocatorError::AddressOverflow)?;
    *total_pages = total_pages
        .checked_add(pages)
        .ok_or(AllocatorError::AddressOverflow)?;
    Ok(())
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

fn set_bit(bitmap: &mut [u8], index: usize, value: bool) {
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
    fn allocates_across_discontiguous_regions_and_reuses_freed_page() {
        let mut usable = FixedList::<_, 2>::new();
        usable.push(region(0x1000, 0x2000)).unwrap();
        usable.push(region(0x8000, 0x1000)).unwrap();
        let layout = AllocatorLayout::<4>::from_regions_excluding(
            &usable,
            &FixedList::<PhysRegion, 0>::new(),
        )
        .unwrap();
        let mut bitmap = [0_u8; 2];
        let mut allocator = PageAllocator::new(&layout, &mut bitmap).unwrap();

        assert_eq!(allocator.allocate(), Ok(PhysAddr::new(0x1000)));
        assert_eq!(allocator.allocate(), Ok(PhysAddr::new(0x2000)));
        assert_eq!(allocator.allocate(), Ok(PhysAddr::new(0x8000)));
        assert_eq!(allocator.allocate(), Err(AllocatorError::Exhausted));
        allocator.free(PhysAddr::new(0x2000)).unwrap();
        assert_eq!(allocator.allocate(), Ok(PhysAddr::new(0x2000)));
        assert_eq!(allocator.stats().allocated_pages, 3);
    }

    #[test]
    fn rounds_exclusions_outward_and_usable_regions_inward() {
        let mut usable = FixedList::<_, 1>::new();
        usable.push(region(0x1001, 0x6fff)).unwrap();
        let mut excluded = FixedList::<_, 1>::new();
        excluded.push(region(0x3100, 0x100)).unwrap();
        let layout = AllocatorLayout::<4>::from_regions_excluding(&usable, &excluded).unwrap();
        assert_eq!(
            layout
                .regions()
                .iter()
                .copied()
                .collect::<std::vec::Vec<_>>(),
            [region(0x2000, 0x1000), region(0x4000, 0x4000)]
        );
        assert_eq!(layout.total_pages(), 5);
    }

    #[test]
    fn rejects_invalid_and_double_frees() {
        let mut usable = FixedList::<_, 1>::new();
        usable.push(region(0x2000, 0x2000)).unwrap();
        let layout = AllocatorLayout::<2>::from_regions_excluding(
            &usable,
            &FixedList::<PhysRegion, 0>::new(),
        )
        .unwrap();
        let mut bitmap = [0_u8; 1];
        let mut allocator = PageAllocator::new(&layout, &mut bitmap).unwrap();
        let page = allocator.allocate().unwrap();

        assert_eq!(
            allocator.free(PhysAddr::new(page.value() + 1)),
            Err(AllocatorError::UnalignedPage)
        );
        assert_eq!(
            allocator.free(PhysAddr::new(0x1000)),
            Err(AllocatorError::PageNotManaged)
        );
        allocator.free(page).unwrap();
        assert_eq!(allocator.free(page), Err(AllocatorError::DoubleFree));
    }

    #[test]
    fn rejects_layout_outside_bitmap_aperture() {
        let mut usable = FixedList::<_, 1>::new();
        usable.push(region(0x8000, 0x1000)).unwrap();
        let layout = AllocatorLayout::<1>::from_regions_excluding(
            &usable,
            &FixedList::<PhysRegion, 0>::new(),
        )
        .unwrap();
        let mut bitmap = [0_u8; 1];
        assert!(matches!(
            PageAllocator::new(&layout, &mut bitmap),
            Err(AllocatorError::PhysicalAddressOutOfRange)
        ));
    }

    #[test]
    fn allocates_inside_requested_region_and_finds_reclaimed_page() {
        let mut usable = FixedList::<_, 2>::new();
        usable.push(region(0x1000, 0x4000)).unwrap();
        usable.push(region(0x10_0000, 0x2000)).unwrap();
        let layout = AllocatorLayout::<4>::from_regions_excluding(
            &usable,
            &FixedList::<PhysRegion, 0>::new(),
        )
        .unwrap();
        let mut reclaimed = FixedList::<_, 1>::new();
        reclaimed.push(region(0x3000, 0x2000)).unwrap();
        assert_eq!(
            layout.first_page_in(&reclaimed),
            Some(PhysAddr::new(0x3000))
        );

        let mut bitmap = [0_u8; 33];
        let mut allocator = PageAllocator::new(&layout, &mut bitmap).unwrap();
        assert_eq!(
            allocator.allocate_in(region(0x10_0000, 0x2000)),
            Ok(PhysAddr::new(0x10_0000))
        );
    }
}
