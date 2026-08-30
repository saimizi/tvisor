use core::fmt;

use crate::system_info::{
    FixedList, MAX_DYNAMIC_ALLOC_RANGES, MAX_DYNAMIC_RESERVATIONS, MAX_MMIO_REGIONS,
    MAX_RAM_REGIONS, MAX_RESERVED_REGIONS, PhysRegion, RamSource, RegionError, ReservationOrigin,
    ReservationOwner, ReservedRegion, SystemInfoBuilder,
};

pub const MAX_NORMALIZED_RESERVED_REGIONS: usize =
    MAX_RESERVED_REGIONS + MAX_DYNAMIC_RESERVATIONS * MAX_DYNAMIC_ALLOC_RANGES + MAX_RAM_REGIONS;
pub const MAX_USABLE_RAM_REGIONS: usize = MAX_RAM_REGIONS + MAX_NORMALIZED_RESERVED_REGIONS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryMapError {
    Capacity,
    InvalidRegion(RegionError),
    UnboundedDynamicReservation,
    RamMmioConflict { ram: PhysRegion, mmio: PhysRegion },
}

impl fmt::Display for MemoryMapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capacity => formatter.write_str("normalized memory-map capacity is insufficient"),
            Self::InvalidRegion(error) => write!(formatter, "invalid normalized region: {error}"),
            Self::UnboundedDynamicReservation => formatter.write_str(
                "a dynamic reservation has no alloc-ranges; its possible placement is unknown",
            ),
            Self::RamMmioConflict { ram, mmio } => {
                write!(formatter, "RAM {ram} overlaps MMIO {mmio}")
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
/// Normalized physical-address classification derived from temporary platform
/// discovery records.
///
/// Every list is sorted and has overlapping or adjacent regions merged. The
/// two usable-RAM views describe different ownership points in the boot
/// transition: `initial_usable_ram` is safe while U-Boot resources are still
/// live, whereas `usable_ram` is the candidate RAM after no-return takeover.
pub struct MemoryMap {
    /// Physical RAM reported by the platform, including firmware carve-outs.
    ///
    /// This is the containing RAM inventory, not an allocation list; regions
    /// in either reservation list must still be excluded before use.
    ram: FixedList<PhysRegion, MAX_RAM_REGIONS>,
    /// RAM that remains unavailable after tvisor takes ownership.
    ///
    /// This includes permanent DTB/firmware reservations and the possible
    /// placement windows of dynamic reservations whose placement tvisor does
    /// not yet control. These regions are subtracted from both usable views.
    permanent_reserved: FixedList<PhysRegion, MAX_NORMALIZED_RESERVED_REGIONS>,
    /// Handoff-time reservations owned by U-Boot or containing the source DTB.
    ///
    /// They must not be overwritten while tvisor might return to U-Boot or
    /// still borrow handoff data. They become reclaimable only after the
    /// explicit no-return boundary and after all required data is owned.
    transition_reserved: FixedList<PhysRegion, MAX_NORMALIZED_RESERVED_REGIONS>,
    /// CPU physical-address windows classified as MMIO rather than RAM.
    ///
    /// These regions are never allocator input and must be mapped with Device
    /// attributes when tvisor needs to access them.
    mmio: FixedList<PhysRegion, MAX_MMIO_REGIONS>,
    /// RAM safe for bootstrap allocations before the no-return takeover.
    ///
    /// It is `ram` minus both permanent and transition reservations. Phase 7
    /// obtains its bootstrap page-table arena from this conservative view.
    initial_usable_ram: FixedList<PhysRegion, MAX_USABLE_RAM_REGIONS>,
    /// RAM potentially allocatable after tvisor has completed takeover.
    ///
    /// It is `ram` minus permanent reservations only. Transition-reserved
    /// ranges appear here because they can eventually be reclaimed, but a
    /// future allocator must wait for their individual lifetime conditions.
    usable_ram: FixedList<PhysRegion, MAX_USABLE_RAM_REGIONS>,
}

impl MemoryMap {
    pub(crate) fn from_builder(info: &SystemInfoBuilder) -> Result<Self, MemoryMapError> {
        let mut raw_ram: FixedList<PhysRegion, MAX_RAM_REGIONS> = FixedList::new();
        for ram in info.ram() {
            raw_ram
                .push(ram.region)
                .map_err(|_| MemoryMapError::Capacity)?;
        }
        let ram: FixedList<PhysRegion, MAX_RAM_REGIONS> = normalize(&raw_ram)?;

        let mut raw_permanent: FixedList<PhysRegion, MAX_NORMALIZED_RESERVED_REGIONS> =
            FixedList::new();
        let mut raw_transition: FixedList<PhysRegion, MAX_NORMALIZED_RESERVED_REGIONS> =
            FixedList::new();
        for ram in info.ram() {
            if ram.source == RamSource::FirmwareCarveout {
                raw_permanent
                    .push(ram.region)
                    .map_err(|_| MemoryMapError::Capacity)?;
            }
        }
        for reserved in info.reserved() {
            let target = if is_transition_reservation(reserved) {
                &mut raw_transition
            } else {
                &mut raw_permanent
            };
            target
                .push(reserved.region)
                .map_err(|_| MemoryMapError::Capacity)?;
        }
        for dynamic in info.dynamic_reserved() {
            if dynamic.alloc_ranges().is_empty() {
                return Err(MemoryMapError::UnboundedDynamicReservation);
            }
            // Until tvisor owns a placement policy, any address in an
            // allocation range might be selected for this reservation.
            for range in dynamic.alloc_ranges() {
                raw_permanent
                    .push(*range)
                    .map_err(|_| MemoryMapError::Capacity)?;
            }
        }
        let permanent_reserved: FixedList<PhysRegion, MAX_NORMALIZED_RESERVED_REGIONS> =
            normalize(&raw_permanent)?;
        let transition_reserved: FixedList<PhysRegion, MAX_NORMALIZED_RESERVED_REGIONS> =
            normalize(&raw_transition)?;

        let mut raw_mmio: FixedList<PhysRegion, MAX_MMIO_REGIONS> = FixedList::new();
        for mmio in info.mmio() {
            raw_mmio
                .push(mmio.region)
                .map_err(|_| MemoryMapError::Capacity)?;
        }
        let mmio: FixedList<PhysRegion, MAX_MMIO_REGIONS> = normalize(&raw_mmio)?;

        for ram_region in &ram {
            for mmio_region in &mmio {
                if ram_region.overlaps(*mmio_region) {
                    return Err(MemoryMapError::RamMmioConflict {
                        ram: *ram_region,
                        mmio: *mmio_region,
                    });
                }
            }
        }

        let mut usable_ram = FixedList::new();
        for ram_region in &ram {
            subtract_all(*ram_region, &permanent_reserved, &mut usable_ram)?;
        }

        let mut all_active: FixedList<PhysRegion, MAX_NORMALIZED_RESERVED_REGIONS> =
            FixedList::new();
        for region in permanent_reserved.iter().chain(transition_reserved.iter()) {
            all_active
                .push(*region)
                .map_err(|_| MemoryMapError::Capacity)?;
        }
        let all_active: FixedList<PhysRegion, MAX_NORMALIZED_RESERVED_REGIONS> =
            normalize(&all_active)?;
        let mut initial_usable_ram = FixedList::new();
        for ram_region in &ram {
            subtract_all(*ram_region, &all_active, &mut initial_usable_ram)?;
        }

        Ok(Self {
            ram,
            permanent_reserved,
            transition_reserved,
            mmio,
            initial_usable_ram,
            usable_ram,
        })
    }

    pub const fn ram(&self) -> &FixedList<PhysRegion, MAX_RAM_REGIONS> {
        &self.ram
    }
    pub const fn reserved(&self) -> &FixedList<PhysRegion, MAX_NORMALIZED_RESERVED_REGIONS> {
        &self.permanent_reserved
    }
    pub const fn transition_reserved(
        &self,
    ) -> &FixedList<PhysRegion, MAX_NORMALIZED_RESERVED_REGIONS> {
        &self.transition_reserved
    }
    pub const fn mmio(&self) -> &FixedList<PhysRegion, MAX_MMIO_REGIONS> {
        &self.mmio
    }
    pub const fn usable_ram(&self) -> &FixedList<PhysRegion, MAX_USABLE_RAM_REGIONS> {
        &self.usable_ram
    }
    pub const fn initial_usable_ram(&self) -> &FixedList<PhysRegion, MAX_USABLE_RAM_REGIONS> {
        &self.initial_usable_ram
    }
}

fn is_transition_reservation(reserved: &ReservedRegion) -> bool {
    reserved.owner == ReservationOwner::Bootloader
        || matches!(
            reserved.origin,
            ReservationOrigin::Bootloader | ReservationOrigin::Dtb
        )
}

fn normalize<const IN: usize, const OUT: usize>(
    input: &FixedList<PhysRegion, IN>,
) -> Result<FixedList<PhysRegion, OUT>, MemoryMapError> {
    let mut output: FixedList<PhysRegion, OUT> = FixedList::new();
    let mut consumed = [false; IN];
    for _ in 0..input.len() {
        let (index, region) = input
            .iter()
            .enumerate()
            .filter(|(index, _)| !consumed[*index])
            .min_by_key(|(_, region)| region.start())
            .expect("one unconsumed input region");
        let mut region = *region;
        consumed[index] = true;

        if let Some(last) = output.get_mut(output.len().saturating_sub(1))
            && (last.overlaps(region) || last.is_adjacent(region))
        {
            let end = if last.end() > region.end() {
                last.end()
            } else {
                region.end()
            };
            region = PhysRegion::from_bounds(last.start(), end)
                .map_err(MemoryMapError::InvalidRegion)?;
            *last = region;
        } else {
            output.push(region).map_err(|_| MemoryMapError::Capacity)?;
        }
    }
    Ok(output)
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
                .map_err(|_| MemoryMapError::Capacity)?;
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
            .map_err(|_| MemoryMapError::Capacity)?;
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
        for region in &self.transition_reserved {
            writeln!(formatter, "    HANDOFF    {region} reclaim-after-takeover")?;
        }
        for region in &self.initial_usable_ram {
            writeln!(formatter, "    INITIAL    {region}")?;
        }
        for region in &self.usable_ram {
            writeln!(formatter, "    USABLE     {region} after-takeover")?;
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
    use crate::system_info::{
        DynamicReservation, MmioKind, MmioRegion, PhysAddr, RamRegion, ReservationAttributes,
        ReservationOrigin, ReservationOwner, ReservedRegion,
    };

    fn region(start: u64, size: u64) -> PhysRegion {
        PhysRegion::new(PhysAddr::new(start), size).unwrap()
    }

    fn reserve(info: &mut SystemInfoBuilder, start: u64, size: u64) {
        info.add_reserved(ReservedRegion {
            region: region(start, size),
            origin: ReservationOrigin::Unknown,
            owner: ReservationOwner::Unknown,
            attributes: ReservationAttributes::default(),
        })
        .unwrap();
    }

    #[test]
    fn sorts_merges_and_subtracts_all_overlap_shapes() {
        let mut info = SystemInfoBuilder::new();
        info.add_ram(RamRegion {
            region: region(0x1000, 0x9000),
            source: RamSource::DeviceTree,
        })
        .unwrap();
        reserve(&mut info, 0x7000, 0x1000);
        reserve(&mut info, 0x3000, 0x1000);
        reserve(&mut info, 0x3800, 0x1800);
        reserve(&mut info, 0x9000, 0x2000);

        let map = MemoryMap::from_builder(&info).unwrap();
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
        let mut info = SystemInfoBuilder::new();
        info.add_ram(RamRegion {
            region: region(0, 0x8000),
            source: RamSource::DeviceTree,
        })
        .unwrap();
        info.add_ram(RamRegion {
            region: region(0x8000, 0x1000),
            source: RamSource::FirmwareCarveout,
        })
        .unwrap();
        let mut dynamic = DynamicReservation::new(
            0x1000,
            Some(0x1000),
            ReservationOrigin::ReservedMemoryNode,
            ReservationOwner::HostPolicy,
            ReservationAttributes {
                no_map: false,
                reusable: true,
            },
        )
        .unwrap();
        dynamic.add_alloc_range(region(0x2000, 0x3000)).unwrap();
        info.add_dynamic_reserved(dynamic).unwrap();
        let map = MemoryMap::from_builder(&info).unwrap();
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
    fn rejects_unbounded_dynamic_reservation_and_ram_mmio_conflict() {
        let mut info = SystemInfoBuilder::new();
        info.add_ram(RamRegion {
            region: region(0x1_0000_0000, 0x4000),
            source: RamSource::DeviceTree,
        })
        .unwrap();
        let dynamic = DynamicReservation::new(
            0x1000,
            None,
            ReservationOrigin::ReservedMemoryNode,
            ReservationOwner::HostPolicy,
            ReservationAttributes::default(),
        )
        .unwrap();
        info.add_dynamic_reserved(dynamic).unwrap();
        assert_eq!(
            MemoryMap::from_builder(&info),
            Err(MemoryMapError::UnboundedDynamicReservation)
        );

        let mut info = SystemInfoBuilder::new();
        info.add_ram(RamRegion {
            region: region(0x1_0000_0000, 0x4000),
            source: RamSource::DeviceTree,
        })
        .unwrap();
        info.add_mmio(MmioRegion {
            region: region(0x1_0000_1000, 0x1000),
            kind: MmioKind::Device,
        })
        .unwrap();
        assert!(matches!(
            MemoryMap::from_builder(&info),
            Err(MemoryMapError::RamMmioConflict { .. })
        ));
    }

    #[test]
    fn bootloader_memory_is_only_excluded_before_takeover() {
        let mut info = SystemInfoBuilder::new();
        info.add_ram(RamRegion {
            region: region(0, 0x10_000),
            source: RamSource::DeviceTree,
        })
        .unwrap();
        reserve(&mut info, 0x2000, 0x1000);
        info.add_reserved(ReservedRegion {
            region: region(0x8000, 0x2000),
            origin: ReservationOrigin::Bootloader,
            owner: ReservationOwner::Bootloader,
            attributes: ReservationAttributes::default(),
        })
        .unwrap();

        let map = MemoryMap::from_builder(&info).unwrap();
        assert_eq!(
            map.reserved().iter().copied().collect::<std::vec::Vec<_>>(),
            [region(0x2000, 0x1000)]
        );
        assert_eq!(
            map.transition_reserved()
                .iter()
                .copied()
                .collect::<std::vec::Vec<_>>(),
            [region(0x8000, 0x2000)]
        );
        assert_eq!(
            map.initial_usable_ram()
                .iter()
                .copied()
                .collect::<std::vec::Vec<_>>(),
            [
                region(0, 0x2000),
                region(0x3000, 0x5000),
                region(0xa000, 0x6000)
            ]
        );
        assert_eq!(
            map.usable_ram()
                .iter()
                .copied()
                .collect::<std::vec::Vec<_>>(),
            [region(0, 0x2000), region(0x3000, 0xd000)]
        );
    }
}
