use core::fmt;

use dtoolkit::fdt::Fdt;
use dtoolkit::standard::{NodeStandard, Status};
use dtoolkit::{Node, Property, ToCellInt};

use crate::system_info::{
    BusTranslation, CapacityError, ConsoleInfo, CpuEnableMethod, CpuInfo, CpuStatus,
    DynamicReservation, DynamicReservationError, MmioKind, MmioRegion, PhysAddr, PhysRegion,
    RamRegion, RamSource, RegionError, ReservationAttributes, ReservationOrigin, ReservationOwner,
    ReservedRegion, SystemInfoBuilder,
};

const PAGE_SIZE: u64 = 0x1000;
const MPIDR_AFFINITY_MASK: u64 = (0xff << 32) | 0x00ff_ffff;
const SHARED_DMA_POOL_COMPATIBLE: &str = "shared-dma-pool";
const BCM2711_COMPATIBLE: &str = "brcm,bcm2711";
const BCM2711_LOW_MEMORY_LIMIT: u64 = 0x4000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityKind {
    Ram,
    Reserved,
    DynamicReserved,
    DynamicAllocRange,
    Mmio,
    BusTranslation,
    Cpu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformError {
    MissingMemory,
    InvalidMemory,
    InvalidReservation,
    ConflictingReservationAttributes,
    InvalidDynamicReservation(DynamicReservationError),
    MissingSocRanges,
    InvalidSocRanges,
    MissingCpus,
    InvalidCpu,
    CurrentCpuMissing,
    InvalidDtbRegion,
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
            Self::InvalidDynamicReservation(error) => {
                write!(formatter, "invalid dynamic reservation: {error}")
            }
            Self::MissingSocRanges => formatter.write_str("the /soc ranges property is missing"),
            Self::InvalidSocRanges => formatter.write_str("the /soc ranges property is invalid"),
            Self::MissingCpus => formatter.write_str("the DTB has no CPU descriptions"),
            Self::InvalidCpu => formatter.write_str("a DTB CPU description is invalid"),
            Self::CurrentCpuMissing => {
                formatter.write_str("MPIDR_EL1 does not match an enabled DTB CPU")
            }
            Self::InvalidDtbRegion => {
                formatter.write_str("the live DTB physical region is invalid")
            }
            Self::Capacity { kind, capacity } => {
                write!(formatter, "{kind:?} capacity {capacity} is insufficient")
            }
        }
    }
}

/// Collects temporary host-platform records from a validated live FDT.
///
/// The builder contains no references into `fdt` and can be finalized directly
/// into `SystemInfo` after discovery.
pub fn discover_system_info_builder(
    fdt: Fdt<'_>,
    dtb_address: PhysAddr,
    tvisor_image: PhysRegion,
    console: ConsoleInfo,
    mpidr_el1: u64,
) -> Result<SystemInfoBuilder, PlatformError> {
    let mut info = SystemInfoBuilder::new();

    discover_ram(fdt, &mut info)?;
    discover_reservations(fdt, dtb_address, tvisor_image, &mut info)?;
    discover_bcm2711_firmware_carveout(fdt, &mut info)?;
    discover_mmio(fdt, console, &mut info)?;
    discover_cpus(fdt, mpidr_el1, &mut info)?;
    info.set_console(console);

    Ok(info)
}

fn discover_bcm2711_firmware_carveout(
    fdt: Fdt<'_>,
    info: &mut SystemInfoBuilder,
) -> Result<(), PlatformError> {
    if !fdt.root().is_compatible(BCM2711_COMPATIBLE) {
        return Ok(());
    }

    let low_ram_end = info
        .ram()
        .iter()
        .filter(|ram| ram.region.start() == PhysAddr::new(0))
        .map(|ram| ram.region.end().value())
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
    info.add_ram(RamRegion {
        region: carveout,
        source: RamSource::FirmwareCarveout,
    })
    .map_err(|error| capacity(CapacityKind::Ram, error))
}

fn discover_ram(fdt: Fdt<'_>, info: &mut SystemInfoBuilder) -> Result<(), PlatformError> {
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
            info.add_ram(RamRegion {
                region,
                source: RamSource::DeviceTree,
            })
            .map_err(|error| capacity(CapacityKind::Ram, error))?;
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
    info: &mut SystemInfoBuilder,
) -> Result<(), PlatformError> {
    for reservation in fdt.memory_reservations() {
        let region = PhysRegion::new(PhysAddr::new(reservation.address()), reservation.size())
            .map_err(|_| PlatformError::InvalidReservation)?;
        add_reserved(
            info,
            region,
            ReservationOrigin::FdtReservationBlock,
            ReservationOwner::Unknown,
            ReservationAttributes::default(),
        )?;
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

            let attributes = ReservationAttributes {
                no_map: reservation.no_map(),
                reusable: reservation.reusable(),
            };
            if attributes.no_map && attributes.reusable {
                return Err(PlatformError::ConflictingReservationAttributes);
            }
            if reservation.no_map_fixup() {
                return Err(PlatformError::InvalidReservation);
            }
            let owner = if reservation.is_compatible(SHARED_DMA_POOL_COMPATIBLE) {
                ReservationOwner::HostPolicy
            } else {
                ReservationOwner::Unknown
            };

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
                    add_reserved(
                        info,
                        region,
                        ReservationOrigin::ReservedMemoryNode,
                        owner,
                        attributes,
                    )?;
                }
                continue;
            }

            let size = reservation
                .size()
                .map_err(|_| PlatformError::InvalidReservation)?
                .ok_or(PlatformError::InvalidReservation)?
                .to_int::<u64>()
                .map_err(|_| PlatformError::InvalidReservation)?;
            let alignment = reservation
                .alignment()
                .map_err(|_| PlatformError::InvalidReservation)?
                .map(|value| value.to_int::<u64>())
                .transpose()
                .map_err(|_| PlatformError::InvalidReservation)?;
            let mut dynamic = DynamicReservation::new(
                size,
                alignment,
                ReservationOrigin::ReservedMemoryNode,
                owner,
                attributes,
            )
            .map_err(PlatformError::InvalidDynamicReservation)?;

            if let Some(ranges) = reservation
                .alloc_ranges()
                .map_err(|_| PlatformError::InvalidReservation)?
            {
                for range in ranges {
                    let start = range
                        .address::<u64>()
                        .map_err(|_| PlatformError::InvalidReservation)?;
                    let size = range
                        .size::<u64>()
                        .map_err(|_| PlatformError::InvalidReservation)?;
                    let region = PhysRegion::new(PhysAddr::new(start), size)
                        .map_err(|_| PlatformError::InvalidReservation)?;
                    dynamic
                        .add_alloc_range(region)
                        .map_err(|error| capacity(CapacityKind::DynamicAllocRange, error))?;
                }
            }
            info.add_dynamic_reserved(dynamic)
                .map_err(|error| capacity(CapacityKind::DynamicReserved, error))?;
        }
    }

    let dtb_size = u64::try_from(fdt.data().len()).map_err(|_| PlatformError::InvalidDtbRegion)?;
    let dtb_region =
        page_rounded_region(dtb_address, dtb_size).map_err(|_| PlatformError::InvalidDtbRegion)?;
    add_reserved(
        info,
        dtb_region,
        ReservationOrigin::Dtb,
        ReservationOwner::Tvisor,
        ReservationAttributes::default(),
    )?;
    add_reserved(
        info,
        tvisor_image,
        ReservationOrigin::TvisorImage,
        ReservationOwner::Tvisor,
        ReservationAttributes::default(),
    )
}

fn discover_mmio(
    fdt: Fdt<'_>,
    console: ConsoleInfo,
    info: &mut SystemInfoBuilder,
) -> Result<(), PlatformError> {
    info.add_mmio(MmioRegion {
        region: console.registers,
        kind: MmioKind::Console,
    })
    .map_err(|error| capacity(CapacityKind::Mmio, error))?;

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
        let translation =
            BusTranslation::new(PhysAddr::new(child_start), PhysAddr::new(start), size)
                .map_err(|_| PlatformError::InvalidSocRanges)?;
        info.add_bus_translation(translation)
            .map_err(|error| capacity(CapacityKind::BusTranslation, error))?;
        info.add_mmio(MmioRegion {
            region,
            kind: MmioKind::BusWindow,
        })
        .map_err(|error| capacity(CapacityKind::Mmio, error))?;
    }
    Ok(())
}

fn discover_cpus(
    fdt: Fdt<'_>,
    mpidr_el1: u64,
    info: &mut SystemInfoBuilder,
) -> Result<(), PlatformError> {
    let cpus = fdt.cpus().map_err(|_| PlatformError::MissingCpus)?;
    let current_affinity = mpidr_el1 & MPIDR_AFFINITY_MASK;
    let mut found_cpu = false;
    let mut found_current = false;

    for cpu in cpus.cpus() {
        let status = if cpu.status().map_err(|_| PlatformError::InvalidCpu)? == Status::Okay {
            CpuStatus::Enabled
        } else {
            CpuStatus::Disabled
        };
        let enable_method = match cpu.enable_method().and_then(|mut methods| methods.next()) {
            Some("psci") => CpuEnableMethod::Psci,
            Some("spin-table") => {
                let release_address = cpu
                    .cpu_release_addr()
                    .map_err(|_| PlatformError::InvalidCpu)?
                    .ok_or(PlatformError::InvalidCpu)?;
                CpuEnableMethod::SpinTable {
                    release_address: PhysAddr::new(release_address),
                }
            }
            _ => CpuEnableMethod::Unknown,
        };

        let ids = cpu.ids().map_err(|_| PlatformError::InvalidCpu)?;
        for id in ids {
            let affinity = id.to_int::<u64>().map_err(|_| PlatformError::InvalidCpu)?;
            let is_current = affinity == current_affinity;
            if is_current && status != CpuStatus::Enabled {
                return Err(PlatformError::CurrentCpuMissing);
            }
            found_current |= is_current;
            found_cpu = true;
            info.add_cpu(CpuInfo {
                affinity,
                status,
                enable_method,
                is_current,
            })
            .map_err(|error| capacity(CapacityKind::Cpu, error))?;
        }
    }

    if !found_cpu {
        Err(PlatformError::MissingCpus)
    } else if !found_current {
        Err(PlatformError::CurrentCpuMissing)
    } else {
        Ok(())
    }
}

fn add_reserved(
    info: &mut SystemInfoBuilder,
    region: PhysRegion,
    origin: ReservationOrigin,
    owner: ReservationOwner,
    attributes: ReservationAttributes,
) -> Result<(), PlatformError> {
    info.add_reserved(ReservedRegion {
        region,
        origin,
        owner,
        attributes,
    })
    .map_err(|error| capacity(CapacityKind::Reserved, error))
}

fn capacity(kind: CapacityKind, error: CapacityError) -> PlatformError {
    PlatformError::Capacity {
        kind,
        capacity: error.capacity(),
    }
}

fn page_rounded_region(start: PhysAddr, size: u64) -> Result<PhysRegion, RegionError> {
    let end = start
        .checked_add(size)
        .ok_or(RegionError::AddressOverflow)?;
    let rounded_start = start.value() & !(PAGE_SIZE - 1);
    let rounded_end = end
        .value()
        .checked_add(PAGE_SIZE - 1)
        .ok_or(RegionError::AddressOverflow)?
        & !(PAGE_SIZE - 1);
    PhysRegion::from_bounds(PhysAddr::new(rounded_start), PhysAddr::new(rounded_end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system_info::{ConsoleKind, ReservationOrigin};

    const TEST_DTB: &[u8] =
        include_bytes!("../third_party/dtoolkit/tests/dtb/test_pretty_print.dtb");
    const MEMRESERVE_DTB: &[u8] =
        include_bytes!("../third_party/dtoolkit/tests/dtb/test_memreserve.dtb");

    fn test_console() -> ConsoleInfo {
        ConsoleInfo {
            kind: ConsoleKind::MiniUart,
            registers: PhysRegion::new(PhysAddr::new(0xfe21_5040), 0x40).unwrap(),
        }
    }

    #[test]
    fn discovers_permanent_runtime_reservations_and_platform_data() {
        let fdt = Fdt::new(TEST_DTB).unwrap();
        let image = PhysRegion::new(PhysAddr::new(0x0400_0000), 0x20_0000).unwrap();

        let info =
            discover_system_info_builder(fdt, PhysAddr::new(0x0300_0123), image, test_console(), 0)
                .unwrap();

        assert_eq!(info.ram().len(), 1);
        assert_eq!(
            info.ram().get(0).unwrap().region,
            PhysRegion::new(PhysAddr::new(0x8000_0000), 0x2000_0000).unwrap()
        );
        assert_eq!(info.dynamic_reserved().len(), 1);
        assert_eq!(info.dynamic_reserved().get(0).unwrap().size(), 0x0400_0000);
        assert_eq!(info.cpus().len(), 1);
        assert!(info.cpus().get(0).unwrap().is_current);
        assert_eq!(info.console(), Some(test_console()));
        assert_eq!(info.mmio().get(0).unwrap().kind, MmioKind::Console);

        assert!(info.reserved().iter().any(|entry| {
            entry.origin == ReservationOrigin::ReservedMemoryNode
                && entry.region == PhysRegion::new(PhysAddr::new(0x7800_0000), 0x0080_0000).unwrap()
        }));
        assert!(info.reserved().iter().any(|entry| {
            entry.origin == ReservationOrigin::Dtb
                && entry.owner == ReservationOwner::Tvisor
                && entry.region.start() == PhysAddr::new(0x0300_0000)
        }));
        assert!(info.reserved().iter().any(|entry| {
            entry.origin == ReservationOrigin::TvisorImage && entry.region == image
        }));
    }

    #[test]
    fn rejects_current_cpu_missing_from_dtb() {
        let fdt = Fdt::new(TEST_DTB).unwrap();
        let image = PhysRegion::new(PhysAddr::new(0x0400_0000), 0x20_0000).unwrap();

        let error =
            discover_system_info_builder(fdt, PhysAddr::new(0x0300_0000), image, test_console(), 1)
                .unwrap_err();

        assert_eq!(error, PlatformError::CurrentCpuMissing);
    }

    #[test]
    fn discovers_fdt_memory_reservation_block() {
        let fdt = Fdt::new(MEMRESERVE_DTB).unwrap();
        let image = PhysRegion::new(PhysAddr::new(0x0400_0000), 0x20_0000).unwrap();
        let mut info = SystemInfoBuilder::new();

        discover_reservations(fdt, PhysAddr::new(0x0300_0000), image, &mut info).unwrap();

        assert_eq!(info.reserved().len(), 4);
        assert_eq!(
            info.reserved().get(0).unwrap().region,
            PhysRegion::new(PhysAddr::new(0x1000), 0x100).unwrap()
        );
        assert_eq!(
            info.reserved().get(1).unwrap().region,
            PhysRegion::new(PhysAddr::new(0x2000), 0x200).unwrap()
        );
        assert_eq!(
            info.reserved().get(0).unwrap().origin,
            ReservationOrigin::FdtReservationBlock
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
