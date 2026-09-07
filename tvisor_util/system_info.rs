use core::fmt;

use crate::{align_down, align_up};
use dtoolkit::fdt::Fdt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysAddr(u64);

impl PhysAddr {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    pub const fn checked_add(self, offset: u64) -> Option<Self> {
        match self.0.checked_add(offset) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

impl fmt::Display for PhysAddr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:#018x}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionError {
    Empty,
    EndBeforeStart,
    AddressOverflow,
    InvalidAlignment,
}

impl fmt::Display for RegionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("physical region is empty"),
            Self::EndBeforeStart => formatter.write_str("physical region ends before it starts"),
            Self::AddressOverflow => formatter.write_str("physical region end overflows"),
            Self::InvalidAlignment => formatter.write_str("region alignment is not a power of two"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysRegion {
    start: PhysAddr,
    size: u64,
}

impl PhysRegion {
    pub const fn new(start: PhysAddr, size: u64) -> Result<Self, RegionError> {
        if size == 0 {
            return Err(RegionError::Empty);
        }
        if start.checked_add(size).is_none() {
            return Err(RegionError::AddressOverflow);
        }

        Ok(Self { start, size })
    }

    pub const fn new_aligned(
        start: PhysAddr,
        size: u64,
        alignment: u64,
    ) -> Result<Self, RegionError> {
        if size == 0 {
            return Err(RegionError::Empty);
        }
        if !alignment.is_power_of_two() {
            return Err(RegionError::InvalidAlignment);
        }
        let aligned_start = align_down(start.value(), alignment);
        let Some(end) = start.value().checked_add(size) else {
            return Err(RegionError::AddressOverflow);
        };
        let Some(aligned_end) = align_up(end, alignment) else {
            return Err(RegionError::AddressOverflow);
        };

        Ok(Self {
            start: PhysAddr::new(aligned_start),
            size: aligned_end - aligned_start,
        })
    }

    pub const fn from_bounds(start: PhysAddr, end: PhysAddr) -> Result<Self, RegionError> {
        if end.value() < start.value() {
            return Err(RegionError::EndBeforeStart);
        }
        if end.value() == start.value() {
            return Err(RegionError::Empty);
        }

        Self::new(start, end.value() - start.value())
    }

    pub const fn start(self) -> PhysAddr {
        self.start
    }

    pub const fn size(self) -> u64 {
        self.size
    }

    pub const fn end(self) -> PhysAddr {
        // Construction proves that this addition cannot overflow.
        PhysAddr::new(self.start.value() + self.size)
    }

    pub const fn contains_address(self, address: PhysAddr) -> bool {
        address.value() >= self.start.value() && address.value() < self.end().value()
    }

    pub const fn contains_region(self, other: Self) -> bool {
        other.start.value() >= self.start.value() && other.end().value() <= self.end().value()
    }

    pub const fn overlaps(self, other: Self) -> bool {
        self.start.value() < other.end().value() && other.start.value() < self.end().value()
    }

    pub const fn is_adjacent(self, other: Self) -> bool {
        self.end().value() == other.start.value() || other.end().value() == self.start.value()
    }
}

impl fmt::Display for PhysRegion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "[{}, {})", self.start, self.end())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityError {
    capacity: usize,
}

impl CapacityError {
    pub const fn capacity(self) -> usize {
        self.capacity
    }
}

impl fmt::Display for CapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "fixed-capacity list is full (capacity {})",
            self.capacity
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedList<T: Copy, const N: usize> {
    entries: [Option<T>; N],
    len: usize,
}

impl<T: Copy, const N: usize> FixedList<T, N> {
    pub const fn new() -> Self {
        Self {
            entries: [None; N],
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn capacity(&self) -> usize {
        N
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn is_full(&self) -> bool {
        self.len == N
    }

    pub fn clear(&mut self) {
        for entry in self.entries.iter_mut() {
            *entry = None;
        }
        self.len = 0;
    }

    pub fn push(&mut self, value: T) -> Result<(), CapacityError> {
        if self.is_full() {
            return Err(CapacityError { capacity: N });
        }

        self.entries[self.len] = Some(value);
        self.len += 1;
        Ok(())
    }

    pub fn insert(&mut self, index: usize, value: T) -> Result<(), CapacityError> {
        if self.is_full() {
            return Err(CapacityError { capacity: N });
        }
        if index > self.len {
            return Err(CapacityError { capacity: N });
        }

        for i in (index..self.len).rev() {
            self.entries[i + 1] = self.entries[i].take();
        }
        self.entries[index] = Some(value);
        self.len += 1;
        Ok(())
    }

    pub fn remove(&mut self, index: usize) -> Option<T> {
        if index >= self.len {
            return None;
        }

        let removed = self.entries[index].take();
        for i in index..(self.len - 1) {
            self.entries[i] = self.entries[i + 1].take();
        }
        self.entries[self.len - 1] = None;
        self.len -= 1;
        removed
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }
        self.entries[index].as_ref()
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if index >= self.len {
            return None;
        }
        self.entries[index].as_mut()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &T> + DoubleEndedIterator {
        self.entries[..self.len]
            .iter()
            .map(|entry| entry.as_ref().expect("initialized list entry"))
    }
}

impl<T: Copy, const N: usize> Default for FixedList<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, T: Copy, const N: usize> IntoIterator for &'a FixedList<T, N> {
    type Item = &'a T;
    type IntoIter = core::iter::Map<core::slice::Iter<'a, Option<T>>, fn(&'a Option<T>) -> &'a T>;

    fn into_iter(self) -> Self::IntoIter {
        fn initialized<T>(entry: &Option<T>) -> &T {
            entry.as_ref().expect("initialized list entry")
        }

        self.entries[..self.len].iter().map(initialized::<T>)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleKind {
    MiniUart,
}

impl ConsoleKind {
    pub const fn min_alignment(&self) -> u64 {
        match self {
            ConsoleKind::MiniUart => size_of::<u32>() as u64,
        }
    }

    pub const fn min_register_size(&self) -> u64 {
        match self {
            ConsoleKind::MiniUart => 0x18,
        }
    }

    pub const fn compatible_str(&self) -> &'static str {
        match self {
            ConsoleKind::MiniUart => "brcm,bcm2835-aux-uart",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsoleInfo {
    pub kind: ConsoleKind,
    pub registers: PhysRegion,
}

impl From<Fdt<'_>> for PhysRegion {
    fn from(value: Fdt<'_>) -> Self {
        PhysRegion {
            start: PhysAddr::new(value.data().as_ptr() as usize as u64),
            size: value.data().len() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::format;
    use std::vec::Vec;

    fn region(start: u64, size: u64) -> PhysRegion {
        PhysRegion::new(PhysAddr::new(start), size).unwrap()
    }

    #[test]
    fn constructs_regions_at_low_and_high_addresses() {
        let low = region(0x1000, 0x2000);
        let high = region(0x1_0000_0000, 0x4000);

        assert_eq!(low.end(), PhysAddr::new(0x3000));
        assert_eq!(high.end(), PhysAddr::new(0x1_0000_4000));
    }

    #[test]
    fn rejects_invalid_regions() {
        assert_eq!(
            PhysRegion::new(PhysAddr::new(0x1000), 0),
            Err(RegionError::Empty)
        );
        assert_eq!(
            PhysRegion::from_bounds(PhysAddr::new(0x2000), PhysAddr::new(0x1000)),
            Err(RegionError::EndBeforeStart)
        );
        assert_eq!(
            PhysRegion::from_bounds(PhysAddr::new(0x1000), PhysAddr::new(0x1000)),
            Err(RegionError::Empty)
        );
        assert_eq!(
            PhysRegion::new(PhysAddr::new(u64::MAX), 1),
            Err(RegionError::AddressOverflow)
        );
        assert_eq!(
            PhysRegion::new_aligned(PhysAddr::new(0x1000), 0, 0x1000),
            Err(RegionError::Empty)
        );
        assert_eq!(
            PhysRegion::new_aligned(PhysAddr::new(0x1000), 1, 3),
            Err(RegionError::InvalidAlignment)
        );
    }

    #[test]
    fn rounds_aligned_regions_outward() {
        assert_eq!(
            PhysRegion::new_aligned(PhysAddr::new(0x1fff), 2, 0x1000).unwrap(),
            region(0x1000, 0x2000)
        );
    }

    #[test]
    fn checks_address_and_region_containment_at_boundaries() {
        let outer = region(0x1000, 0x2000);
        let inner = region(0x1800, 0x800);

        assert!(outer.contains_address(PhysAddr::new(0x1000)));
        assert!(outer.contains_address(PhysAddr::new(0x2fff)));
        assert!(!outer.contains_address(PhysAddr::new(0x3000)));
        assert!(outer.contains_region(inner));
        assert!(outer.contains_region(outer));
        assert!(!inner.contains_region(outer));
    }

    #[test]
    fn distinguishes_overlap_and_adjacency() {
        let base = region(0x1000, 0x1000);
        let disjoint = region(0x3000, 0x1000);
        let adjacent = region(0x2000, 0x1000);
        let partial = region(0x1800, 0x1000);
        let contained = region(0x1400, 0x100);

        assert!(!base.overlaps(disjoint));
        assert!(!base.is_adjacent(disjoint));
        assert!(!base.overlaps(adjacent));
        assert!(base.is_adjacent(adjacent));
        assert!(base.overlaps(partial));
        assert!(partial.overlaps(base));
        assert!(base.overlaps(contained));
    }

    #[test]
    fn checked_address_addition_detects_overflow() {
        assert_eq!(
            PhysAddr::new(0x1000).checked_add(0x20),
            Some(PhysAddr::new(0x1020))
        );
        assert_eq!(PhysAddr::new(u64::MAX).checked_add(1), None);
    }

    #[test]
    fn fixed_list_preserves_insertion_order_and_capacity() {
        let mut list = FixedList::<u32, 2>::new();
        assert!(list.is_empty());
        assert_eq!(list.capacity(), 2);

        list.push(10).unwrap();
        list.push(20).unwrap();
        assert!(list.is_full());
        assert_eq!(list.len(), 2);
        assert_eq!(list.get(0), Some(&10));
        assert_eq!(list.get(1), Some(&20));
        assert_eq!(list.get(2), None);
        assert_eq!(list.iter().copied().collect::<Vec<_>>(), [10, 20]);
        assert_eq!((&list).into_iter().copied().collect::<Vec<_>>(), [10, 20]);

        let error = list.push(30).unwrap_err();
        assert_eq!(error.capacity(), 2);
        assert_eq!(list.iter().copied().collect::<Vec<_>>(), [10, 20]);
    }

    #[test]
    fn fixed_list_clear_insert_remove_work() {
        let mut list = FixedList::<u32, 3>::new();
        list.push(10).unwrap();
        list.push(30).unwrap();

        list.insert(1, 20).unwrap();
        assert_eq!(list.iter().copied().collect::<Vec<_>>(), [10, 20, 30]);

        let removed = list.remove(1).unwrap();
        assert_eq!(removed, 20);
        assert_eq!(list.iter().copied().collect::<Vec<_>>(), [10, 30]);

        list.clear();
        assert!(list.is_empty());
        assert_eq!(list.iter().copied().collect::<Vec<_>>(), []);
    }

    #[test]
    fn zero_capacity_list_is_consistently_full() {
        let mut list = FixedList::<u32, 0>::new();
        assert!(list.is_empty());
        assert!(list.is_full());
        assert_eq!(list.push(1), Err(CapacityError { capacity: 0 }));
        assert_eq!(list.iter().len(), 0);
    }

    #[test]
    fn formats_addresses_regions_and_errors_stably() {
        assert_eq!(
            format!("{}", PhysAddr::new(0x0400_0000)),
            "0x0000000004000000"
        );
        assert_eq!(
            format!("{}", region(0x0400_0000, 0x20_0000)),
            "[0x0000000004000000, 0x0000000004200000)"
        );
        assert_eq!(
            format!("{}", RegionError::AddressOverflow),
            "physical region end overflows"
        );
        assert_eq!(
            format!("{}", CapacityError { capacity: 4 }),
            "fixed-capacity list is full (capacity 4)"
        );
    }
}
