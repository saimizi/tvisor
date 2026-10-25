//! Verified lab-rpi4 firmware startup profile. This is discovery evidence,
//! not a secondary activation interface or a measurement of another CPU's PC.
use crate::{
    cpu_topology::{CpuTopology, StartupMethod},
    memory_map::MemoryMap,
    system_info::{PhysAddr, PhysRegion},
};
use dtoolkit::fdt::Fdt;

pub const ARMSTUB_SIZE: usize = 0x150;
/// Canonical source-built stub. See the adjacent BSD license/provenance file.
const ARMSTUB: &[u8; ARMSTUB_SIZE] = include_bytes!("fixtures/rpi4_armstub8_gic.bin");
pub const RELEASE_ADDRESSES: [u64; 4] = [0xd8, 0xe0, 0xe8, 0xf0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupContractError {
    UnsupportedTopology,
    MissingHoldingReservation,
    UnrecognizedStub,
    NonzeroReleaseCell,
    HoldingMemoryAllocatable,
}

/// Construction requires exact immutable firmware bytes and zero release cells.
/// The contract remains valid only while firmware code/cells remain untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpinTableContract {
    holding_memory: PhysRegion,
}
impl SpinTableContract {
    pub fn holding_memory(&self) -> PhysRegion {
        self.holding_memory
    }

    /// Must be checked before initializing allocators. No inferred U-Boot
    /// runtime range or release-cell-only reservation substitutes for this page.
    pub fn verify_memory_map(&self, map: &MemoryMap) -> Result<(), StartupContractError> {
        if !map
            .reserved()
            .iter()
            .any(|region| region.contains_region(self.holding_memory))
        {
            return Err(StartupContractError::MissingHoldingReservation);
        }
        if map
            .usable_ram()
            .iter()
            .any(|region| region.overlaps(self.holding_memory))
        {
            return Err(StartupContractError::HoldingMemoryAllocatable);
        }
        Ok(())
    }
}

pub fn holding_memory() -> PhysRegion {
    PhysRegion::new(PhysAddr::new(0), 0x1000).unwrap()
}

/// The live DT reservation block must cover the whole holding page, not just
/// its release cells. Checked before the board path reads the low-memory stub.
pub fn verify_dt_reservation(fdt: Fdt<'_>) -> Result<(), StartupContractError> {
    let protected = holding_memory();
    for entry in fdt.memory_reservations() {
        if let Ok(region) = PhysRegion::new(PhysAddr::new(entry.address()), entry.size())
            && region.contains_region(protected)
        {
            return Ok(());
        }
    }
    Err(StartupContractError::MissingHoldingReservation)
}

pub fn verify_spin_table_contract(
    topology: &CpuTopology,
    snapshot: &[u8; ARMSTUB_SIZE],
) -> Result<SpinTableContract, StartupContractError> {
    // This profile is deliberately narrower than generic topology discovery.
    // The matched firmware starts affinity 0 as primary and selects cells by
    // low MPIDR bits. Tvisor itself still matches the complete DT affinities.
    if topology.psci.is_some()
        || topology.cpus().first().is_none_or(|cpu| cpu.affinity != 0)
        || topology.cpus().len() != 4
    {
        return Err(StartupContractError::UnsupportedTopology);
    }
    for (affinity, expected_cell) in RELEASE_ADDRESSES.iter().enumerate() {
        let Some(cpu) = topology
            .cpus()
            .iter()
            .find(|cpu| cpu.affinity == affinity as u64)
        else {
            return Err(StartupContractError::UnsupportedTopology);
        };
        if cpu.startup
            != (StartupMethod::SpinTable {
                release_address: *expected_cell,
            })
        {
            return Err(StartupContractError::UnsupportedTopology);
        }
    }
    // Firmware changes these cells and patches the DTB/kernel pointers. Every
    // other byte, including literals, entry setup and setup_gic, must match.
    if snapshot[..0xd8] != ARMSTUB[..0xd8] || snapshot[0x100..] != ARMSTUB[0x100..] {
        return Err(StartupContractError::UnrecognizedStub);
    }
    if snapshot[0xd8..0xf8].iter().any(|byte| *byte != 0) {
        return Err(StartupContractError::NonzeroReleaseCell);
    }
    Ok(SpinTableContract {
        holding_memory: holding_memory(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu_topology::discover_cpu_topology;
    use dtoolkit::model::{DeviceTree, DeviceTreeNode, DeviceTreeProperty};
    fn topology(cell_offset: u64) -> CpuTopology {
        let mut tree = DeviceTree::new();
        let mut cpus = DeviceTreeNode::new_unchecked("cpus");
        cpus.add_property(DeviceTreeProperty::new_unchecked(
            "#address-cells",
            1u32.to_be_bytes().to_vec(),
        ));
        cpus.add_property(DeviceTreeProperty::new_unchecked(
            "#size-cells",
            0u32.to_be_bytes().to_vec(),
        ));
        for id in 0..4u32 {
            let mut cpu = DeviceTreeNode::new_unchecked(alloc::format!("cpu@{id}"));
            cpu.add_property(DeviceTreeProperty::new_unchecked(
                "device_type",
                b"cpu\0".to_vec(),
            ));
            cpu.add_property(DeviceTreeProperty::new_unchecked(
                "reg",
                id.to_be_bytes().to_vec(),
            ));
            cpu.add_property(DeviceTreeProperty::new_unchecked(
                "enable-method",
                b"spin-table\0".to_vec(),
            ));
            cpu.add_property(DeviceTreeProperty::new_unchecked(
                "cpu-release-addr",
                (0xd8 + id as u64 * 8 + cell_offset).to_be_bytes().to_vec(),
            ));
            cpus.add_child(cpu);
        }
        tree.root.add_child(cpus);
        let bytes = tree.to_dtb();
        discover_cpu_topology(Fdt::new(&bytes).unwrap(), 0x80000000).unwrap()
    }
    fn snapshot() -> [u8; ARMSTUB_SIZE] {
        let mut bytes = *ARMSTUB;
        bytes[0xd8..0xf8].fill(0);
        bytes[0xf8..0xfc].copy_from_slice(&0x2eff1f00u32.to_le_bytes());
        bytes[0xfc..0x100].copy_from_slice(&0x80000u32.to_le_bytes());
        bytes
    }
    #[test]
    fn accepts_exact_stub_with_firmware_patches() {
        let contract = verify_spin_table_contract(&topology(0), &snapshot()).unwrap();
        assert_eq!(contract.holding_memory(), holding_memory());
    }
    #[test]
    fn rejects_every_immutable_byte_change() {
        let topology = topology(0);
        for index in (0..0xd8).chain(0x100..ARMSTUB_SIZE) {
            let mut bytes = snapshot();
            bytes[index] ^= 1;
            assert_eq!(
                verify_spin_table_contract(&topology, &bytes),
                Err(StartupContractError::UnrecognizedStub)
            );
        }
    }
    #[test]
    fn rejects_nonzero_cells_and_changed_release_addresses() {
        for address in RELEASE_ADDRESSES {
            let mut bytes = snapshot();
            bytes[address as usize] = 1;
            assert_eq!(
                verify_spin_table_contract(&topology(0), &bytes),
                Err(StartupContractError::NonzeroReleaseCell)
            );
        }
        assert_eq!(
            verify_spin_table_contract(&topology(8), &snapshot()),
            Err(StartupContractError::UnsupportedTopology)
        );
    }
    #[test]
    fn holding_page_must_remain_reserved_before_allocation() {
        let contract = verify_spin_table_contract(&topology(0), &snapshot()).unwrap();
        let mut map = MemoryMap::empty();
        map.insert_ram(PhysRegion::new(PhysAddr::new(0), 0x4000).unwrap())
            .unwrap();
        map.finalize().unwrap();
        assert_eq!(
            contract.verify_memory_map(&map),
            Err(StartupContractError::MissingHoldingReservation)
        );
        map.insert_reserved(holding_memory()).unwrap();
        // Catch stale usable_ram even when a reservation was inserted later.
        assert_eq!(
            contract.verify_memory_map(&map),
            Err(StartupContractError::HoldingMemoryAllocatable)
        );
        map.finalize().unwrap();
        contract.verify_memory_map(&map).unwrap();
    }
    #[test]
    fn rejects_missing_dt_memory_reservation() {
        let bytes = DeviceTree::new().to_dtb();
        assert_eq!(
            verify_dt_reservation(Fdt::new(&bytes).unwrap()),
            Err(StartupContractError::MissingHoldingReservation)
        );
        let bytes = include_bytes!("../third_party/dtoolkit/tests/dtb/test_memreserve.dtb");
        // The fixture reserves only 0x100 bytes at 0x1000, not the holding page.
        assert_eq!(
            verify_dt_reservation(Fdt::new(bytes).unwrap()),
            Err(StartupContractError::MissingHoldingReservation)
        );
    }
    #[test]
    fn accepts_full_dt_reservation_and_rejects_partial_or_overflowing_ranges() {
        use dtoolkit::memreserve::MemoryReservation;
        for (address, size, valid) in [
            (0, 0x1000, true),
            (0, 0x2000, true),
            (0, 0xfff, false),
            (8, 0x1000, false),
            (u64::MAX, 2, false),
        ] {
            let mut tree = DeviceTree::new();
            tree.memory_reservations
                .push(MemoryReservation::new(address, size));
            let bytes = tree.to_dtb();
            assert_eq!(
                verify_dt_reservation(Fdt::new(&bytes).unwrap()).is_ok(),
                valid
            );
        }
    }
    #[test]
    fn allocator_never_returns_or_reclaims_the_holding_page() {
        use crate::page_allocator::{AllocatorError, PageAllocator, PageBitmap, PageState};
        let contract = verify_spin_table_contract(&topology(0), &snapshot()).unwrap();
        let mut map = MemoryMap::empty();
        map.insert_ram(PhysRegion::new(PhysAddr::new(0), 0x4000).unwrap())
            .unwrap();
        map.insert_reserved(holding_memory()).unwrap();
        map.finalize().unwrap();
        contract.verify_memory_map(&map).unwrap();
        let (mut managed, mut in_use) = (PageBitmap::<1>::zeroed(), PageBitmap::<1>::zeroed());
        let mut allocator = PageAllocator::new(
            map.ram(),
            map.usable_ram(),
            map.usable_ram(),
            &mut managed,
            &mut in_use,
        )
        .unwrap();
        assert_eq!(allocator.state(PhysAddr::new(0)), Ok(PageState::Reserved));
        for _ in 0..3 {
            assert!(allocator.allocate().unwrap().value() >= 0x1000);
        }
        assert_eq!(allocator.allocate(), Err(AllocatorError::Exhausted));
        assert_eq!(allocator.release(holding_memory()), Ok(0));
        assert_eq!(allocator.state(PhysAddr::new(0)), Ok(PageState::Reserved));
    }
}
