use core::fmt;

use crate::println;
use dtoolkit::fdt::Fdt;
use dtoolkit::standard::{NodeStandard, Status};
use dtoolkit::{Node, Property, ToCellInt};

use crate::memory_map::{MemoryMap, MemoryMapError};
use crate::system_info::{PhysAddr, PhysRegion, RegionError};
use crate::*;

const BCM2711_COMPATIBLE: &str = "brcm,bcm2711";
const BCM2711_LOW_MEMORY_LIMIT: u64 = 0x4000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityKind {
    Ram,
    Reserved,
    Mmio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformError {
    MissingMemory,
    InvalidMemory,
    InvalidReservation,
    ConflictingReservationAttributes,
    InvalidDynamicReservation,
    UnboundedDynamicReservation,
    MissingSocRanges,
    InvalidSocRanges,
    InvalidDtbRegion,
    MemoryMap(MemoryMapError),
    Capacity { kind: CapacityKind, capacity: usize },
}

impl fmt::Display for PlatformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMemory => formatter.write_str("the DTB has no enabled memory node"),
            Self::InvalidMemory => formatter.write_str("a DTB memory description is invalid"),
            Self::InvalidReservation => {
                formatter.write_str("a DTB reserved-memory description is invalid")
            }
            Self::ConflictingReservationAttributes => {
                formatter.write_str("a reserved-memory node has both no-map and reusable")
            }
            Self::InvalidDynamicReservation => {
                formatter.write_str("invalid dynamic reservation size or alignment")
            }
            Self::UnboundedDynamicReservation => formatter.write_str(
                "a dynamic reservation has no alloc-ranges; its possible placement is unknown",
            ),
            Self::MissingSocRanges => formatter.write_str("the /soc ranges property is missing"),
            Self::InvalidSocRanges => formatter.write_str("the /soc ranges property is invalid"),
            Self::InvalidDtbRegion => {
                formatter.write_str("the live DTB physical region is invalid")
            }
            Self::MemoryMap(error) => write!(formatter, "invalid memory map: {error}"),
            Self::Capacity { kind, capacity } => {
                write!(formatter, "{kind:?} capacity {capacity} is insufficient")
            }
        }
    }
}

pub fn discover_memory_map(
    fdt: Fdt<'_>,
    dtb_address: PhysAddr,
    tvisor_image: PhysRegion,
    destination: &mut MemoryMap,
) -> Result<(), PlatformError> {
    destination.clear();

    discover_ram(fdt, destination)?;
    discover_bcm2711_firmware_carveout(fdt, destination)?;
    discover_reservations(fdt, dtb_address, tvisor_image, destination)?;
    discover_mmio(fdt, destination)?;
    destination.finalize().map_err(PlatformError::MemoryMap)
}

fn discover_bcm2711_firmware_carveout(
    fdt: Fdt<'_>,
    map: &mut MemoryMap,
) -> Result<(), PlatformError> {
    if !fdt.root().is_compatible(BCM2711_COMPATIBLE) {
        return Ok(());
    }

    let low_ram_end = map
        .ram()
        .iter()
        .filter(|ram| ram.start() == PhysAddr::new(0))
        .map(|ram| ram.end().value())
        .max()
        .ok_or(PlatformError::MissingMemory)?;
    if low_ram_end >= BCM2711_LOW_MEMORY_LIMIT {
        return Ok(());
    }

    let carveout = PhysRegion::from_bounds(
        PhysAddr::new(low_ram_end),
        PhysAddr::new(BCM2711_LOW_MEMORY_LIMIT),
    )
    .map_err(|_| PlatformError::InvalidMemory)?;

    println!("Found BCM2711 firmware carve-out: {}", carveout);
    map.insert_ram(carveout)
        .map_err(|error| memory_error(CapacityKind::Ram, error))?;
    map.insert_reserved(carveout)
        .map_err(|error| memory_error(CapacityKind::Reserved, error))?;

    Ok(())
}

fn discover_ram(fdt: Fdt<'_>, map: &mut MemoryMap) -> Result<(), PlatformError> {
    let mut found = false;
    for node in fdt.root().children() {
        let Some(device_type) = node.property("device_type") else {
            continue;
        };
        if device_type
            .as_str()
            .map_err(|_| PlatformError::InvalidMemory)?
            != "memory"
        {
            continue;
        }
        if node.status().map_err(|_| PlatformError::InvalidMemory)? != Status::Okay {
            continue;
        }

        let registers = node
            .reg()
            .map_err(|_| PlatformError::InvalidMemory)?
            .ok_or(PlatformError::InvalidMemory)?;
        for register in registers {
            let start = register
                .address::<u64>()
                .map_err(|_| PlatformError::InvalidMemory)?;
            let size = register
                .size::<u64>()
                .map_err(|_| PlatformError::InvalidMemory)?;
            let region = PhysRegion::new(PhysAddr::new(start), size)
                .map_err(|_| PlatformError::InvalidMemory)?;
            println!("Found RAM region: {}", region);
            map.insert_ram(region)
                .map_err(|error| memory_error(CapacityKind::Ram, error))?;
            found = true;
        }
    }

    if found {
        Ok(())
    } else {
        Err(PlatformError::MissingMemory)
    }
}

fn discover_reservations(
    fdt: Fdt<'_>,
    dtb_address: PhysAddr,
    tvisor_image: PhysRegion,
    map: &mut MemoryMap,
) -> Result<(), PlatformError> {
    for reservation in fdt.memory_reservations() {
        let region = PhysRegion::new(PhysAddr::new(reservation.address()), reservation.size())
            .map_err(|_| PlatformError::InvalidReservation)?;
        println!("Found Reserved Memory Region: {}", region);
        map.insert_reserved(region)
            .map_err(|error| memory_error(CapacityKind::Reserved, error))?;
    }

    if let Some(reservations) = fdt.reserved_memory() {
        for reservation in reservations {
            if reservation
                .status()
                .map_err(|_| PlatformError::InvalidReservation)?
                != Status::Okay
            {
                continue;
            }

            let no_map = reservation.no_map();
            let reusable = reservation.reusable();
            if no_map && reusable {
                return Err(PlatformError::ConflictingReservationAttributes);
            }
            if reservation.no_map_fixup() {
                return Err(PlatformError::InvalidReservation);
            }

            if let Some(registers) = reservation
                .reg()
                .map_err(|_| PlatformError::InvalidReservation)?
            {
                for register in registers {
                    let start = register
                        .address::<u64>()
                        .map_err(|_| PlatformError::InvalidReservation)?;
                    let size = register
                        .size::<u64>()
                        .map_err(|_| PlatformError::InvalidReservation)?;
                    let region = PhysRegion::new(PhysAddr::new(start), size)
                        .map_err(|_| PlatformError::InvalidReservation)?;
                    println!("Found reserved-memory fixed region: {}", region);
                    map.insert_reserved(region)
                        .map_err(|error| memory_error(CapacityKind::Reserved, error))?;
                }
                continue;
            }

            let size = reservation
                .size()
                .map_err(|_| PlatformError::InvalidReservation)?
                .ok_or(PlatformError::InvalidReservation)?
                .to_int::<u64>()
                .map_err(|_| PlatformError::InvalidReservation)?;
            if size == 0 {
                return Err(PlatformError::InvalidDynamicReservation);
            }

            let alignment = reservation
                .alignment()
                .map_err(|_| PlatformError::InvalidReservation)?
                .map(|value| value.to_int::<u64>())
                .transpose()
                .map_err(|_| PlatformError::InvalidReservation)?;
            if let Some(alignment) = alignment
                && !alignment.is_power_of_two()
            {
                return Err(PlatformError::InvalidDynamicReservation);
            }

            let ranges = reservation
                .alloc_ranges()
                .map_err(|_| PlatformError::InvalidReservation)?
                .ok_or(PlatformError::UnboundedDynamicReservation)?;
            let mut count = 0;
            for range in ranges {
                count += 1;
                let start = range
                    .address::<u64>()
                    .map_err(|_| PlatformError::InvalidReservation)?;
                let size = range
                    .size::<u64>()
                    .map_err(|_| PlatformError::InvalidReservation)?;
                let region = PhysRegion::new(PhysAddr::new(start), size)
                    .map_err(|_| PlatformError::InvalidReservation)?;
                println!("Found reserved-memory dynamic alloc range: {}", region);
                map.insert_reserved(region)
                    .map_err(|error| memory_error(CapacityKind::Reserved, error))?;
            }
            if count == 0 {
                return Err(PlatformError::UnboundedDynamicReservation);
            }
        }
    }

    let dtb_size = u64::try_from(fdt.data().len()).map_err(|_| PlatformError::InvalidDtbRegion)?;
    let dtb_region =
        page_rounded_region(dtb_address, dtb_size).map_err(|_| PlatformError::InvalidDtbRegion)?;
    println!("Reserving live DTB region: {}", dtb_region);
    map.insert_reserved(dtb_region)
        .map_err(|error| memory_error(CapacityKind::Reserved, error))?;

    println!("Reserving tvisor image: {}", tvisor_image);
    map.insert_reserved(tvisor_image)
        .map_err(|error| memory_error(CapacityKind::Reserved, error))
}

fn discover_mmio(fdt: Fdt<'_>, map: &mut MemoryMap) -> Result<(), PlatformError> {
    let Some(soc) = fdt.find_node("/soc") else {
        return Ok(());
    };
    if soc.status().map_err(|_| PlatformError::InvalidSocRanges)? != Status::Okay {
        return Ok(());
    }
    let ranges = soc
        .ranges()
        .map_err(|_| PlatformError::InvalidSocRanges)?
        .ok_or(PlatformError::MissingSocRanges)?;
    for range in ranges {
        let child_start = range
            .child_bus_address::<u64>()
            .map_err(|_| PlatformError::InvalidSocRanges)?;
        let start = range
            .parent_bus_address::<u64>()
            .map_err(|_| PlatformError::InvalidSocRanges)?;
        let size = range
            .length::<u64>()
            .map_err(|_| PlatformError::InvalidSocRanges)?;
        let region = PhysRegion::new(PhysAddr::new(start), size)
            .map_err(|_| PlatformError::InvalidSocRanges)?;

        println!(
            "SOC translation: child {:#x} -> parent {:#x} size {:#x}",
            child_start, start, size
        );
        map.insert_mmio(region)
            .map_err(|error| memory_error(CapacityKind::Mmio, error))?;
    }
    Ok(())
}

fn memory_error(kind: CapacityKind, error: MemoryMapError) -> PlatformError {
    match error {
        MemoryMapError::Capacity { capacity } => PlatformError::Capacity { kind, capacity },
        MemoryMapError::InvalidRegion(_) | MemoryMapError::RamMmioConflict { .. } => {
            PlatformError::InvalidMemory
        }
    }
}

fn page_rounded_region(start: PhysAddr, size: u64) -> Result<PhysRegion, RegionError> {
    let end = start
        .checked_add(size)
        .ok_or(RegionError::AddressOverflow)?;
    let rounded_start = page_address(start.value() as usize) as u64;
    let rounded_end = page_address(
        end.value()
            .checked_add(PAGE_SIZE as u64 - 1)
            .ok_or(RegionError::AddressOverflow)? as usize,
    ) as u64;
    PhysRegion::from_bounds(PhysAddr::new(rounded_start), PhysAddr::new(rounded_end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_map::MemoryMap;

    const TEST_DTB: &[u8] =
        include_bytes!("../third_party/dtoolkit/tests/dtb/test_pretty_print.dtb");
    const MEMRESERVE_DTB: &[u8] =
        include_bytes!("../third_party/dtoolkit/tests/dtb/test_memreserve.dtb");

    #[test]
    fn discovers_ram_and_rejects_unbounded_dynamic_reservation() {
        let fdt = Fdt::new(TEST_DTB).unwrap();
        let image = PhysRegion::new(PhysAddr::new(0x0400_0000), 0x20_0000).unwrap();
        let mut map = MemoryMap::empty();

        discover_ram(fdt, &mut map).unwrap();

        assert_eq!(map.ram().len(), 1);
        assert_eq!(
            map.ram().get(0).unwrap(),
            &PhysRegion::new(PhysAddr::new(0x8000_0000), 0x2000_0000).unwrap()
        );
        assert!(map.mmio().is_empty());
        let error =
            discover_reservations(fdt, PhysAddr::new(0x0300_0123), image, &mut map).unwrap_err();

        assert_eq!(error, PlatformError::UnboundedDynamicReservation);
    }

    #[test]
    fn discovers_fdt_memory_reservation_block() {
        let fdt = Fdt::new(MEMRESERVE_DTB).unwrap();
        let image = PhysRegion::new(PhysAddr::new(0x0400_0000), 0x20_0000).unwrap();
        let mut map = MemoryMap::empty();

        discover_reservations(fdt, PhysAddr::new(0x0300_0000), image, &mut map).unwrap();

        assert_eq!(map.reserved().len(), 4);
        assert_eq!(
            map.reserved().get(0).unwrap(),
            &PhysRegion::new(PhysAddr::new(0x1000), 0x100).unwrap()
        );
        assert_eq!(
            map.reserved().get(1).unwrap(),
            &PhysRegion::new(PhysAddr::new(0x2000), 0x200).unwrap()
        );
    }

    #[test]
    fn page_rounding_covers_unaligned_blob() {
        assert_eq!(
            page_rounded_region(PhysAddr::new(0x2eff_1f00), 0xe0b7),
            PhysRegion::new(PhysAddr::new(0x2eff_1000), 0xf000)
        );
    }
}
