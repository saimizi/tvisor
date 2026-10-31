//! Explicit selection of the documented BCM2711 armstub trust profile.
use crate::{
    cpu_topology::{CpuTopology, MPIDR_AFFINITY_MASK},
    secondary_entry::EntryContract,
};
use dtoolkit::{
    Node, Property,
    fdt::Fdt,
    standard::{NodeStandard, Status},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendError {
    Platform,
    Cpu,
    Cluster,
    FirmwareInterface,
}
pub struct LabRpi4Backend {
    _private: (),
}
impl LabRpi4Backend {
    pub fn select(
        fdt: Fdt<'_>,
        topology: &CpuTopology,
        midr: u64,
        mpidr: u64,
    ) -> Result<Self, BackendError> {
        if !fdt.root().is_compatible("raspberrypi,4-model-b")
            || !fdt.root().is_compatible("brcm,bcm2711")
        {
            return Err(BackendError::Platform);
        }
        if (midr >> 24) & 255 != 0x41 || (midr >> 4) & 4095 != 0xd08 {
            return Err(BackendError::Cpu);
        }
        if topology.psci.is_some() {
            return Err(BackendError::FirmwareInterface);
        }
        let boot = mpidr & MPIDR_AFFINITY_MASK;
        if !topology
            .cpus()
            .iter()
            .any(|cpu| cpu.enabled && cpu.logical_id == Some(0) && cpu.affinity == boot)
            || topology
                .cpus()
                .iter()
                .filter(|cpu| cpu.enabled)
                .any(|cpu| cpu.affinity & !255 != boot & !255 || cpu.affinity & 255 >= 4)
        {
            return Err(BackendError::Cluster);
        }
        let cpus = fdt.find_node("/cpus").ok_or(BackendError::Cpu)?;
        let mut count = 0;
        for node in cpus.children() {
            if node
                .property("device_type")
                .is_some_and(|p| p.as_str().ok() == Some("cpu"))
                && node.status().map_err(|_| BackendError::Cpu)? == Status::Okay
            {
                if !node.is_compatible("arm,cortex-a72") {
                    return Err(BackendError::Cpu);
                }
                count += 1;
            }
        }
        if count != topology.cpus().iter().filter(|cpu| cpu.enabled).count() {
            return Err(BackendError::Cpu);
        }
        Ok(Self { _private: () })
    }
    /// Justified by phase11_3_backend_contract.md for this selected trust profile,
    /// not a measurement of another CPU's live state or firmware authentication.
    pub fn contract(&self) -> EntryContract {
        EntryContract {
            el2_mmu_off: true,
            coherent: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu_topology::discover_cpu_topology;
    use dtoolkit::model::{DeviceTree, DeviceTreeNode, DeviceTreeProperty};
    fn fixture(
        compatible: &[u8],
        affinity: u32,
        cpu_compatible: &[u8],
        psci: bool,
    ) -> alloc::vec::Vec<u8> {
        fn prop(n: &mut DeviceTreeNode, key: &str, v: &[u8]) {
            n.add_property(DeviceTreeProperty::new_unchecked(key, v.to_vec()));
        }
        let mut t = DeviceTree::new();
        prop(&mut t.root, "compatible", compatible);
        let mut cpus = DeviceTreeNode::new_unchecked("cpus");
        prop(&mut cpus, "#address-cells", &1u32.to_be_bytes());
        prop(&mut cpus, "#size-cells", &0u32.to_be_bytes());
        for (i, a) in [0, affinity].into_iter().enumerate() {
            let mut c = DeviceTreeNode::new_unchecked(alloc::format!("cpu@{a:x}"));
            prop(&mut c, "device_type", b"cpu\0");
            prop(&mut c, "compatible", cpu_compatible);
            prop(&mut c, "reg", &a.to_be_bytes());
            prop(&mut c, "enable-method", b"spin-table\0");
            prop(
                &mut c,
                "cpu-release-addr",
                &(0xd8 + i as u64 * 8).to_be_bytes(),
            );
            cpus.add_child(c);
        }
        t.root.add_child(cpus);
        if psci {
            let mut p = DeviceTreeNode::new_unchecked("psci");
            prop(&mut p, "compatible", b"arm,psci-0.2\0");
            prop(&mut p, "method", b"smc\0");
            t.root.add_child(p);
        }
        t.to_dtb()
    }
    #[test]
    fn selects_only_documented_board_cpu_and_single_cluster_profile() {
        let board = b"raspberrypi,4-model-b\0brcm,bcm2711\0";
        for (compat, affinity, cpu, psci, midr, expected) in [
            (
                &board[..],
                1,
                &b"arm,cortex-a72\0"[..],
                false,
                0x410fd083,
                None,
            ),
            (
                &b"brcm,bcm2711\0"[..],
                1,
                &b"arm,cortex-a72\0"[..],
                false,
                0x410fd083,
                Some(BackendError::Platform),
            ),
            (
                &board[..],
                0x101,
                &b"arm,cortex-a72\0"[..],
                false,
                0x410fd083,
                Some(BackendError::Cluster),
            ),
            (
                &board[..],
                4,
                &b"arm,cortex-a72\0"[..],
                false,
                0x410fd083,
                Some(BackendError::Cluster),
            ),
            (
                &board[..],
                1,
                &b"arm,cortex-a53\0"[..],
                false,
                0x410fd083,
                Some(BackendError::Cpu),
            ),
            (
                &board[..],
                1,
                &b"arm,cortex-a72\0"[..],
                false,
                0x410fd033,
                Some(BackendError::Cpu),
            ),
            (
                &board[..],
                1,
                &b"arm,cortex-a72\0"[..],
                true,
                0x410fd083,
                Some(BackendError::FirmwareInterface),
            ),
        ] {
            let bytes = fixture(compat, affinity, cpu, psci);
            let fdt = Fdt::new(&bytes).unwrap();
            let topology = discover_cpu_topology(fdt, 0x80000000).unwrap();
            let result = LabRpi4Backend::select(fdt, &topology, midr, 0x80000000);
            assert_eq!(result.as_ref().err().copied(), expected);
            if let Ok(backend) = result {
                assert!(backend.contract().coherent);
                assert!(backend.contract().el2_mmu_off);
            }
        }
    }
}
