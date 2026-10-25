//! DT-described CPU startup memory ownership. Firmware code and entry state are
//! not inspected here; firmware supplies the reservations under the boot protocol.
use crate::{
    cpu_topology::{CpuTopology, MAX_CPUS, StartupMethod},
    memory_map::MemoryMap,
    system_info::{FixedList, PhysAddr, PhysRegion},
};
use dtoolkit::{
    Node, Property,
    fdt::Fdt,
    standard::{NodeStandard, Status},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupContractError {
    InvalidReservation,
    MissingHoldingReservation,
    ReleaseCellNotRam,
    InvalidRam,
    UnsupportedStartupMethod,
    ReleaseCellNotIdentityMapped,
    NonzeroReleaseCell,
    HoldingMemoryAllocatable,
}

/// Owns the release-cell ranges; the live DT remains protected and available
/// for checking every firmware reservation again before allocator initialization.
/// This description grants no permission to activate a secondary CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupMemoryContract {
    release_cells: FixedList<PhysRegion, MAX_CPUS>,
}
impl StartupMemoryContract {
    pub fn release_cells(&self) -> &FixedList<PhysRegion, MAX_CPUS> {
        &self.release_cells
    }

    pub fn verify_release_cells(
        &self,
        mut read: impl FnMut(PhysAddr) -> Result<u64, StartupContractError>,
    ) -> Result<(), StartupContractError> {
        for cell in &self.release_cells {
            if read(cell.start())? != 0 {
                return Err(StartupContractError::NonzeroReleaseCell);
            }
        }
        Ok(())
    }

    /// Preserve ALL DT reservation-block entries, including holding code in a
    /// separate region from the release cells. Never infer the holding-code
    /// footprint from the cell addresses or reserve just their containing page.
    pub fn verify_memory_map(
        &self,
        fdt: Fdt<'_>,
        map: &MemoryMap,
    ) -> Result<(), StartupContractError> {
        for reservation in fdt.memory_reservations() {
            verify_reserved_range(
                reservation_region(reservation.address(), reservation.size())?,
                map,
            )?;
        }
        for cell in &self.release_cells {
            verify_reserved_range(*cell, map)?;
        }
        Ok(())
    }
}

fn verify_reserved_range(region: PhysRegion, map: &MemoryMap) -> Result<(), StartupContractError> {
    if !map
        .reserved()
        .iter()
        .any(|reserved| reserved.contains_region(region))
    {
        return Err(StartupContractError::MissingHoldingReservation);
    }
    if map
        .usable_ram()
        .iter()
        .any(|usable| usable.overlaps(region))
    {
        return Err(StartupContractError::HoldingMemoryAllocatable);
    }
    Ok(())
}

fn reservation_region(address: u64, size: u64) -> Result<PhysRegion, StartupContractError> {
    PhysRegion::new(PhysAddr::new(address), size)
        .map_err(|_| StartupContractError::InvalidReservation)
}

pub fn discover_startup_memory(
    fdt: Fdt<'_>,
    topology: &CpuTopology,
) -> Result<StartupMemoryContract, StartupContractError> {
    // Reject malformed reservation ranges even if no release cell uses them.
    for reservation in fdt.memory_reservations() {
        reservation_region(reservation.address(), reservation.size())?;
    }
    let mut contract = StartupMemoryContract {
        release_cells: FixedList::new(),
    };
    for cpu in topology.cpus().iter().filter(|cpu| cpu.enabled) {
        match cpu.startup {
            StartupMethod::SpinTable { release_address } => {
                let cell = reservation_region(release_address, 8)?;
                if !fdt.memory_reservations().any(|reservation| {
                    reservation_region(reservation.address(), reservation.size())
                        .is_ok_and(|region| region.contains_region(cell))
                }) {
                    return Err(StartupContractError::MissingHoldingReservation);
                }
                if !cell_in_ram(fdt, cell)? {
                    return Err(StartupContractError::ReleaseCellNotRam);
                }
                // Shared release cells are valid descriptions. Read/protect them
                // once; 11.3 must handle every CPU reachable through that cell.
                if !contract.release_cells.iter().any(|entry| *entry == cell) {
                    contract
                        .release_cells
                        .push(cell)
                        .map_err(|_| StartupContractError::UnsupportedStartupMethod)?;
                }
            }
            StartupMethod::Psci => {} // Description only; EL2 callability is a separate gate.
            StartupMethod::Missing | StartupMethod::Unsupported => {
                return Err(StartupContractError::UnsupportedStartupMethod);
            }
        }
    }
    Ok(contract)
}

fn cell_in_ram(fdt: Fdt<'_>, cell: PhysRegion) -> Result<bool, StartupContractError> {
    for node in fdt.root().children() {
        let Some(kind) = node.property("device_type") else {
            continue;
        };
        if kind
            .as_str()
            .map_err(|_| StartupContractError::InvalidRam)?
            != "memory"
            || node
                .status()
                .map_err(|_| StartupContractError::InvalidRam)?
                != Status::Okay
        {
            continue;
        }
        let registers = node
            .reg()
            .map_err(|_| StartupContractError::InvalidRam)?
            .ok_or(StartupContractError::InvalidRam)?;
        for register in registers {
            let address = register
                .address::<u64>()
                .map_err(|_| StartupContractError::InvalidRam)?;
            let size = register
                .size::<u64>()
                .map_err(|_| StartupContractError::InvalidRam)?;
            let ram = PhysRegion::new(PhysAddr::new(address), size)
                .map_err(|_| StartupContractError::InvalidRam)?;
            if ram.contains_region(cell) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu_topology::discover_cpu_topology;
    use dtoolkit::{
        memreserve::MemoryReservation,
        model::{DeviceTree, DeviceTreeNode, DeviceTreeProperty},
    };
    fn property(node: &mut DeviceTreeNode, name: &str, bytes: &[u8]) {
        node.add_property(DeviceTreeProperty::new_unchecked(name, bytes.to_vec()));
    }
    fn tree(cells: &[(u64, u64)], reservations: &[(u64, u64)]) -> DeviceTree {
        let mut tree = DeviceTree::new();
        property(&mut tree.root, "#address-cells", &2u32.to_be_bytes());
        property(&mut tree.root, "#size-cells", &2u32.to_be_bytes());
        let mut memory = DeviceTreeNode::new_unchecked("memory@0");
        property(&mut memory, "device_type", b"memory\0");
        let mut registers = 0u64.to_be_bytes().to_vec();
        registers.extend_from_slice(&0x10000u64.to_be_bytes());
        property(&mut memory, "reg", &registers);
        tree.root.add_child(memory);
        let mut cpus = DeviceTreeNode::new_unchecked("cpus");
        property(&mut cpus, "#address-cells", &2u32.to_be_bytes());
        property(&mut cpus, "#size-cells", &0u32.to_be_bytes());
        for &(affinity, address) in cells {
            let mut cpu = DeviceTreeNode::new_unchecked(alloc::format!("cpu@{affinity:x}"));
            property(&mut cpu, "device_type", b"cpu\0");
            property(&mut cpu, "reg", &affinity.to_be_bytes());
            property(&mut cpu, "enable-method", b"spin-table\0");
            property(&mut cpu, "cpu-release-addr", &address.to_be_bytes());
            cpus.add_child(cpu);
        }
        tree.root.add_child(cpus);
        for &(start, size) in reservations {
            tree.memory_reservations
                .push(MemoryReservation::new(start, size));
        }
        tree
    }
    fn discover(bytes: &[u8], boot: u64) -> Result<StartupMemoryContract, StartupContractError> {
        let fdt = Fdt::new(bytes).unwrap();
        let topology = discover_cpu_topology(fdt, boot).unwrap();
        discover_startup_memory(fdt, &topology)
    }
    fn memory_map() -> MemoryMap {
        let mut map = MemoryMap::empty();
        map.insert_ram(PhysRegion::new(PhysAddr::new(0), 0x10000).unwrap())
            .unwrap();
        map
    }
    #[test]
    fn accepts_dt_selected_addresses_and_affinities_without_firmware_bytes() {
        let bytes = tree(
            &[(0x100, 0x2080), (0x200, 0x3088)],
            &[(0x2000, 0x2000), (0x8000, 0x1000)],
        )
        .to_dtb();
        let contract = discover(&bytes, 0x200).unwrap();
        let mut seen = alloc::vec::Vec::new();
        contract
            .verify_release_cells(|address| {
                seen.push(address.value());
                Ok(0)
            })
            .unwrap();
        assert_eq!(seen, [0x3088, 0x2080]);
    }
    #[test]
    fn accepts_shared_release_cells_and_reads_them_once() {
        let bytes = tree(&[(0, 0x2080), (1, 0x2080)], &[(0x2000, 0x1000)]).to_dtb();
        let contract = discover(&bytes, 0).unwrap();
        assert_eq!(contract.release_cells().len(), 1);
        let mut reads = 0;
        contract
            .verify_release_cells(|_| {
                reads += 1;
                Ok(0)
            })
            .unwrap();
        assert_eq!(reads, 1);
    }
    #[test]
    fn rejects_missing_partial_and_overflowing_reservations() {
        for reservations in [&[][..], &[(0x2080, 7)][..], &[(0x2081, 8)][..]] {
            let bytes = tree(&[(0, 0x2080)], reservations).to_dtb();
            assert_eq!(
                discover(&bytes, 0),
                Err(StartupContractError::MissingHoldingReservation)
            );
        }
        let bytes = tree(&[(0, 0x2080)], &[(u64::MAX, 8)]).to_dtb();
        assert_eq!(
            discover(&bytes, 0),
            Err(StartupContractError::InvalidReservation)
        );
    }
    #[test]
    fn release_cells_must_be_in_ram_before_any_read() {
        let bytes = tree(&[(0, 0x10000)], &[(0x10000, 8)]).to_dtb();
        assert_eq!(
            discover(&bytes, 0),
            Err(StartupContractError::ReleaseCellNotRam)
        );
        let bytes = tree(&[(0, 0xfff8)], &[(0xf000, 0x1000)]).to_dtb();
        discover(&bytes, 0).unwrap();
    }
    #[test]
    fn rejects_nonzero_values_and_propagates_unreadable_cell_errors() {
        let bytes = tree(&[(0, 0x2080)], &[(0x2000, 0x1000)]).to_dtb();
        let contract = discover(&bytes, 0).unwrap();
        assert_eq!(
            contract.verify_release_cells(|_| Ok(0x4001000)),
            Err(StartupContractError::NonzeroReleaseCell)
        );
        assert_eq!(
            contract
                .verify_release_cells(|_| Err(StartupContractError::ReleaseCellNotIdentityMapped)),
            Err(StartupContractError::ReleaseCellNotIdentityMapped)
        );
    }
    #[test]
    fn protects_separate_holding_code_reservations_not_just_cell_pages() {
        let bytes = tree(&[(0, 0x2080)], &[(0x2000, 0x1000), (0x8000, 0x1000)]).to_dtb();
        let fdt = Fdt::new(&bytes).unwrap();
        let contract = discover(&bytes, 0).unwrap();
        let mut map = memory_map();
        map.insert_reserved(PhysRegion::new(PhysAddr::new(0x2000), 0x1000).unwrap())
            .unwrap();
        map.finalize().unwrap();
        assert_eq!(
            contract.verify_memory_map(fdt, &map),
            Err(StartupContractError::MissingHoldingReservation)
        );
        map.insert_reserved(PhysRegion::new(PhysAddr::new(0x8000), 0x1000).unwrap())
            .unwrap();
        assert_eq!(
            contract.verify_memory_map(fdt, &map),
            Err(StartupContractError::HoldingMemoryAllocatable)
        );
        map.finalize().unwrap();
        contract.verify_memory_map(fdt, &map).unwrap();
    }
    #[test]
    fn allocator_cannot_allocate_or_release_reserved_startup_memory() {
        use crate::page_allocator::{AllocatorError, PageAllocator, PageBitmap, PageState};
        let bytes = tree(&[(0, 0xd8)], &[(0, 0x1000), (0x8000, 0x1000)]).to_dtb();
        let fdt = Fdt::new(&bytes).unwrap();
        let contract = discover(&bytes, 0).unwrap();
        let mut map = memory_map();
        for reservation in fdt.memory_reservations() {
            map.insert_reserved(
                reservation_region(reservation.address(), reservation.size()).unwrap(),
            )
            .unwrap();
        }
        map.finalize().unwrap();
        contract.verify_memory_map(fdt, &map).unwrap();
        let (mut managed, mut in_use) = (PageBitmap::<2>::zeroed(), PageBitmap::<2>::zeroed());
        let mut allocator = PageAllocator::new(
            map.ram(),
            map.usable_ram(),
            map.usable_ram(),
            &mut managed,
            &mut in_use,
        )
        .unwrap();
        for _ in 0..14 {
            let page = allocator.allocate().unwrap().value();
            assert_ne!(page, 0);
            assert_ne!(page, 0x8000);
        }
        assert_eq!(allocator.allocate(), Err(AllocatorError::Exhausted));
        for address in [0, 0x8000] {
            assert_eq!(
                allocator.release(reservation_region(address, 0x1000).unwrap()),
                Ok(0)
            );
            assert_eq!(
                allocator.state(PhysAddr::new(address)),
                Ok(PageState::Reserved)
            );
        }
    }
    #[test]
    fn psci_description_does_not_invent_release_cells_or_call_firmware() {
        let mut tree = tree(&[(0, 0x2080)], &[]);
        let cpu = tree.find_node_mut("/cpus/cpu@0").unwrap();
        cpu.remove_property("enable-method");
        cpu.remove_property("cpu-release-addr");
        property(cpu, "enable-method", b"psci\0");
        let mut psci = DeviceTreeNode::new_unchecked("psci");
        property(&mut psci, "compatible", b"arm,psci-0.2\0");
        property(&mut psci, "method", b"smc\0");
        tree.root.add_child(psci);
        let bytes = tree.to_dtb();
        let contract = discover(&bytes, 0).unwrap();
        assert!(contract.release_cells().is_empty());
        contract
            .verify_release_cells(|_| panic!("PSCI has no release-cell reads"))
            .unwrap();
    }
}
