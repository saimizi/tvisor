use core::fmt;

use crate::system_info::{FixedList, PhysRegion, RegionError};

pub const MAX_RAM_REGIONS: usize = 8;
pub const MAX_MMIO_REGIONS: usize = 64;
pub const MAX_NORMALIZED_RESERVED_REGIONS: usize = 192;
pub const MAX_USABLE_RAM_REGIONS: usize = MAX_RAM_REGIONS + MAX_NORMALIZED_RESERVED_REGIONS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryMapError {
    Capacity { capacity: usize },
    InvalidRegion(RegionError),
    RamMmioConflict { ram: PhysRegion, mmio: PhysRegion },
}

impl fmt::Display for MemoryMapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capacity { capacity } => write!(
                formatter,
                "normalized memory-map capacity is insufficient (capacity {})",
                capacity
            ),
            Self::InvalidRegion(error) => write!(formatter, "invalid normalized region: {error}"),
            Self::RamMmioConflict { ram, mmio } => {
                write!(formatter, "RAM {ram} overlaps MMIO {mmio}")
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
/// Final physical-address classification built in place during DTB discovery.
///
/// Each list remains sorted and merged as entries are inserted. This permits
/// the boot path to construct one statically stored map without raw-list or
/// normalized-map temporaries.
pub struct MemoryMap {
    /// All physical RAM reported by the platform, including firmware
    /// carve-outs that are represented separately as permanent reservations.
    ram: FixedList<PhysRegion, MAX_RAM_REGIONS>,
    /// RAM that tvisor must not allocate after takeover.
    permanent_reserved: FixedList<PhysRegion, MAX_NORMALIZED_RESERVED_REGIONS>,
    /// Physical device windows. These addresses are not allocator input.
    mmio: FixedList<PhysRegion, MAX_MMIO_REGIONS>,
    /// `ram` minus every permanent reservation, rebuilt by `finalize()`.
    usable_ram: FixedList<PhysRegion, MAX_USABLE_RAM_REGIONS>,
}

impl MemoryMap {
    pub const fn empty() -> Self {
        Self {
            ram: FixedList::new(),
            permanent_reserved: FixedList::new(),
            mmio: FixedList::new(),
            usable_ram: FixedList::new(),
        }
    }

    pub fn clear(&mut self) {
        self.ram.clear();
        self.permanent_reserved.clear();
        self.mmio.clear();
        self.usable_ram.clear();
    }

    pub fn insert_ram(&mut self, region: PhysRegion) -> Result<(), MemoryMapError> {
        insert_sorted_merged(&mut self.ram, region)
    }

    pub fn insert_reserved(&mut self, region: PhysRegion) -> Result<(), MemoryMapError> {
        insert_sorted_merged(&mut self.permanent_reserved, region)
    }

    pub fn insert_mmio(&mut self, region: PhysRegion) -> Result<(), MemoryMapError> {
        insert_sorted_merged(&mut self.mmio, region)
    }

    pub fn finalize(&mut self) -> Result<(), MemoryMapError> {
        for ram in &self.ram {
            for mmio in &self.mmio {
                if ram.overlaps(*mmio) {
                    return Err(MemoryMapError::RamMmioConflict {
                        ram: *ram,
                        mmio: *mmio,
                    });
                }
            }
        }

        self.usable_ram.clear();
        for ram in &self.ram {
            subtract_all(*ram, &self.permanent_reserved, &mut self.usable_ram)?;
        }

        Ok(())
    }

    pub const fn ram(&self) -> &FixedList<PhysRegion, MAX_RAM_REGIONS> {
        &self.ram
    }
    pub const fn reserved(&self) -> &FixedList<PhysRegion, MAX_NORMALIZED_RESERVED_REGIONS> {
        &self.permanent_reserved
    }
    pub const fn mmio(&self) -> &FixedList<PhysRegion, MAX_MMIO_REGIONS> {
        &self.mmio
    }
    pub const fn usable_ram(&self) -> &FixedList<PhysRegion, MAX_USABLE_RAM_REGIONS> {
        &self.usable_ram
    }
}

fn merge_regions(a: PhysRegion, b: PhysRegion) -> Result<PhysRegion, MemoryMapError> {
    let start = if a.start().value() < b.start().value() {
        a.start()
    } else {
        b.start()
    };
    let end = if a.end().value() > b.end().value() {
        a.end()
    } else {
        b.end()
    };
    PhysRegion::from_bounds(start, end).map_err(MemoryMapError::InvalidRegion)
}

fn insert_sorted_merged<const N: usize>(
    list: &mut FixedList<PhysRegion, N>,
    region: PhysRegion,
) -> Result<(), MemoryMapError> {
    let mut index = 0;
    while index < list.len() && list.get(index).unwrap().start().value() < region.start().value() {
        index += 1;
    }

    if index > 0 {
        let prev = *list.get(index - 1).unwrap();
        if prev.overlaps(region) || prev.is_adjacent(region) {
            let merged = merge_regions(prev, region)?;
            *list.get_mut(index - 1).unwrap() = merged;
            while index < list.len() {
                let previous = *list.get(index - 1).unwrap();
                let current = *list.get(index).unwrap();
                if previous.overlaps(current) || previous.is_adjacent(current) {
                    let new_merged = merge_regions(previous, current)?;
                    *list.get_mut(index - 1).unwrap() = new_merged;
                    list.remove(index);
                } else {
                    break;
                }
            }
            return Ok(());
        }
    }

    if index < list.len() {
        let current = *list.get(index).unwrap();
        if current.overlaps(region) || current.is_adjacent(region) {
            let merged = merge_regions(current, region)?;
            *list.get_mut(index).unwrap() = merged;

            let next_index = index + 1;
            while next_index < list.len() {
                let previous = *list.get(next_index - 1).unwrap();
                let current = *list.get(next_index).unwrap();
                if previous.overlaps(current) || previous.is_adjacent(current) {
                    let new_merged = merge_regions(previous, current)?;
                    *list.get_mut(next_index - 1).unwrap() = new_merged;
                    list.remove(next_index);
                } else {
                    break;
                }
            }
            return Ok(());
        }
    }

    list.insert(index, region)
        .map_err(|_| MemoryMapError::Capacity { capacity: N })
}

fn subtract_all(
    ram: PhysRegion,
    reserved: &FixedList<PhysRegion, MAX_NORMALIZED_RESERVED_REGIONS>,
    output: &mut FixedList<PhysRegion, MAX_USABLE_RAM_REGIONS>,
) -> Result<(), MemoryMapError> {
    let mut cursor = ram.start();
    for reservation in reserved {
        if reservation.end() <= cursor || reservation.start() >= ram.end() {
            continue;
        }
        if reservation.start() > cursor {
            output
                .push(
                    PhysRegion::from_bounds(cursor, reservation.start())
                        .map_err(MemoryMapError::InvalidRegion)?,
                )
                .map_err(|_| MemoryMapError::Capacity {
                    capacity: output.capacity(),
                })?;
        }
        if reservation.end() >= ram.end() {
            return Ok(());
        }
        cursor = reservation.end();
    }
    if cursor < ram.end() {
        output
            .push(
                PhysRegion::from_bounds(cursor, ram.end())
                    .map_err(MemoryMapError::InvalidRegion)?,
            )
            .map_err(|_| MemoryMapError::Capacity {
                capacity: output.capacity(),
            })?;
    }
    Ok(())
}

impl fmt::Display for MemoryMap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "Normalized memory map:")?;
        for region in &self.ram {
            writeln!(formatter, "  RAM        {region}")?;
        }
        for region in &self.permanent_reserved {
            writeln!(formatter, "    RESERVED   {region}")?;
        }
        for region in &self.usable_ram {
            writeln!(formatter, "    USABLE     {region}")?;
        }
        for region in &self.mmio {
            writeln!(formatter, "  MMIO       {region}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system_info::PhysAddr;

    fn region(start: u64, size: u64) -> PhysRegion {
        PhysRegion::new(PhysAddr::new(start), size).unwrap()
    }

    #[test]
    fn sorts_merges_and_subtracts_all_overlap_shapes() {
        let mut map = MemoryMap::empty();
        map.insert_ram(region(0x1000, 0x9000)).unwrap();
        map.insert_reserved(region(0x7000, 0x1000)).unwrap();
        map.insert_reserved(region(0x3000, 0x1000)).unwrap();
        map.insert_reserved(region(0x3800, 0x1800)).unwrap();
        map.insert_reserved(region(0x9000, 0x2000)).unwrap();

        map.finalize().unwrap();

        assert_eq!(map.reserved().len(), 3);
        assert_eq!(
            map.usable_ram()
                .iter()
                .copied()
                .collect::<std::vec::Vec<_>>(),
            [
                region(0x1000, 0x2000),
                region(0x5000, 0x2000),
                region(0x8000, 0x1000)
            ]
        );
    }

    #[test]
    fn inventories_firmware_carveout_but_excludes_it_from_usable_ram() {
        let mut map = MemoryMap::empty();
        map.insert_ram(region(0, 0x8000)).unwrap();
        map.insert_ram(region(0x8000, 0x1000)).unwrap();
        map.insert_reserved(region(0x2000, 0x3000)).unwrap();
        map.insert_reserved(region(0x8000, 0x1000)).unwrap();

        map.finalize().unwrap();

        assert_eq!(
            map.ram().iter().copied().collect::<std::vec::Vec<_>>(),
            [region(0, 0x9000)]
        );
        assert_eq!(
            map.reserved().iter().copied().collect::<std::vec::Vec<_>>(),
            [region(0x2000, 0x3000), region(0x8000, 0x1000)]
        );
        assert_eq!(
            map.usable_ram()
                .iter()
                .copied()
                .collect::<std::vec::Vec<_>>(),
            [region(0, 0x2000), region(0x5000, 0x3000)]
        );
    }

    #[test]
    fn rejects_ram_mmio_conflict() {
        let mut map = MemoryMap::empty();
        map.insert_ram(region(0x1_0000_0000, 0x4000)).unwrap();
        map.insert_mmio(region(0x1_0000_1000, 0x1000)).unwrap();

        assert!(matches!(
            map.finalize(),
            Err(MemoryMapError::RamMmioConflict { .. })
        ));
    }

    #[test]
    fn every_explicit_reservation_is_excluded_from_usable_ram() {
        let mut map = MemoryMap::empty();
        map.insert_ram(region(0, 0x10_000)).unwrap();
        map.insert_reserved(region(0x2000, 0x1000)).unwrap();
        map.insert_reserved(region(0x8000, 0x2000)).unwrap();

        map.finalize().unwrap();

        assert_eq!(
            map.reserved().iter().copied().collect::<std::vec::Vec<_>>(),
            [region(0x2000, 0x1000), region(0x8000, 0x2000)]
        );
        assert_eq!(
            map.usable_ram()
                .iter()
                .copied()
                .collect::<std::vec::Vec<_>>(),
            [
                region(0, 0x2000),
                region(0x3000, 0x5000),
                region(0xa000, 0x6000)
            ]
        );
    }
}
