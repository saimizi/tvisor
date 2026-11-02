//! Allocation-free physical CPU discovery. Discovery never releases a CPU or
//! executes a firmware call. All returned descriptions own their data.
use dtoolkit::{
    Node, Property,
    fdt::Fdt,
    standard::{NodeStandard, Status},
};

pub const MAX_CPUS: usize = 4;
pub const MPIDR_AFFINITY_MASK: u64 = 0x0000_00ff_00ff_ffff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupMethod {
    Missing,
    Unsupported,
    SpinTable { release_address: u64 },
    Psci,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsciBinding {
    Legacy,
    V0_2,
    V1_0,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsciConduit {
    Smc,
    Hvc,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PsciDescription {
    pub binding: PsciBinding,
    pub conduit: PsciConduit,
    /// Legacy IDs are descriptions only, never permission to call firmware.
    pub legacy_cpu_on: Option<u32>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuDescriptor {
    pub affinity: u64,
    pub enabled: bool,
    /// Disabled CPUs have no logical ID. The boot CPU is always logical 0.
    pub logical_id: Option<usize>,
    pub startup: StartupMethod,
}
const EMPTY_CPU: CpuDescriptor = CpuDescriptor {
    affinity: 0,
    enabled: false,
    logical_id: None,
    startup: StartupMethod::Missing,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTopology {
    cpus: [CpuDescriptor; MAX_CPUS],
    count: usize,
    pub psci: Option<PsciDescription>,
}
impl CpuTopology {
    pub fn cpus(&self) -> &[CpuDescriptor] {
        &self.cpus[..self.count]
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuDiscoveryError {
    MissingCpus,
    InvalidCells,
    InvalidCpu,
    InvalidAffinity,
    InvalidReleaseAddress,
    DuplicateAffinity,
    Capacity,
    BootCpuMissing,
    InvalidPsci,
}

pub fn discover_cpu_topology(
    fdt: Fdt<'_>,
    boot_mpidr: u64,
) -> Result<CpuTopology, CpuDiscoveryError> {
    use CpuDiscoveryError::*;
    let parent = fdt.find_node("/cpus").ok_or(MissingCpus)?;
    if parent.status().map_err(|_| InvalidCpu)? != Status::Okay {
        return Err(MissingCpus);
    }
    let cells = parent
        .property("#address-cells")
        .ok_or(InvalidCells)?
        .as_u32()
        .map_err(|_| InvalidCells)?;
    if !matches!(cells, 1 | 2)
        || parent
            .property("#size-cells")
            .ok_or(InvalidCells)?
            .as_u32()
            .map_err(|_| InvalidCells)?
            != 0
    {
        return Err(InvalidCells);
    }
    let mut result = CpuTopology {
        cpus: [EMPTY_CPU; MAX_CPUS],
        count: 0,
        psci: discover_psci(fdt)?,
    };
    for node in parent.children() {
        let Some(kind) = node.property("device_type") else {
            if node.name_without_address() == "cpu" {
                return Err(InvalidCpu);
            }
            continue;
        };
        if kind.as_str().map_err(|_| InvalidCpu)? != "cpu" {
            continue;
        }
        let enabled = node.status().map_err(|_| InvalidCpu)? == Status::Okay;
        let reg = node.property("reg").ok_or(InvalidCpu)?;
        let affinity = match cells {
            1 => u64::from(reg.as_u32().map_err(|_| InvalidCpu)?),
            _ => reg.as_u64().map_err(|_| InvalidCpu)?,
        };
        if affinity & !MPIDR_AFFINITY_MASK != 0 {
            return Err(InvalidAffinity);
        }
        if result.cpus().iter().any(|cpu| cpu.affinity == affinity) {
            return Err(DuplicateAffinity);
        }
        if result.count == MAX_CPUS {
            return Err(Capacity);
        }
        let startup = match node.property("enable-method") {
            None => StartupMethod::Missing,
            Some(property) => match property.as_str().map_err(|_| InvalidCpu)? {
                "psci" => {
                    if result.psci.is_none() {
                        return Err(InvalidPsci);
                    }
                    StartupMethod::Psci
                }
                "spin-table" => {
                    let address = node
                        .property("cpu-release-addr")
                        .ok_or(InvalidReleaseAddress)?
                        .as_u64()
                        .map_err(|_| InvalidReleaseAddress)?;
                    if address & 7 != 0 || address.checked_add(8).is_none() {
                        return Err(InvalidReleaseAddress);
                    }
                    StartupMethod::SpinTable {
                        release_address: address,
                    }
                }
                _ => StartupMethod::Unsupported,
            },
        };
        result.cpus[result.count] = CpuDescriptor {
            affinity,
            enabled,
            logical_id: None,
            startup,
        };
        result.count += 1;
    }
    let boot_affinity = boot_mpidr & MPIDR_AFFINITY_MASK;
    let boot = result
        .cpus()
        .iter()
        .position(|cpu| cpu.enabled && cpu.affinity == boot_affinity)
        .ok_or(BootCpuMissing)?;
    result.cpus.swap(0, boot);
    // Deterministic affinity ordering after boot CPU, independent of DT order.
    result.cpus[1..result.count].sort_unstable_by_key(|cpu| cpu.affinity);
    let mut next = 0;
    for cpu in &mut result.cpus[..result.count] {
        if cpu.enabled {
            cpu.logical_id = Some(next);
            next += 1;
        }
    }
    Ok(result)
}

fn discover_psci(fdt: Fdt<'_>) -> Result<Option<PsciDescription>, CpuDiscoveryError> {
    use CpuDiscoveryError::InvalidPsci;
    // Match the binding's compatible strings, not a particular node name.
    let mut found = None;
    for node in fdt.root().children() {
        let binding = if node.is_compatible("arm,psci-1.0") {
            PsciBinding::V1_0
        } else if node.is_compatible("arm,psci-0.2") {
            PsciBinding::V0_2
        } else if node.is_compatible("arm,psci") {
            PsciBinding::Legacy
        } else {
            if node.name_without_address() == "psci" {
                return Err(InvalidPsci);
            }
            continue;
        };
        if node.status().map_err(|_| InvalidPsci)? != Status::Okay {
            continue;
        }
        if found.is_some() {
            return Err(InvalidPsci);
        }
        let conduit = match node
            .property("method")
            .ok_or(InvalidPsci)?
            .as_str()
            .map_err(|_| InvalidPsci)?
        {
            "smc" => PsciConduit::Smc,
            "hvc" => PsciConduit::Hvc,
            _ => return Err(InvalidPsci),
        };
        let legacy_cpu_on = node
            .property("cpu_on")
            .map(|p| p.as_u32())
            .transpose()
            .map_err(|_| InvalidPsci)?;
        found = Some(PsciDescription {
            binding,
            conduit,
            legacy_cpu_on,
        });
    }
    Ok(found)
}

/// Capability interpretation after a separately verified, callable firmware
/// interface returns PSCI_VERSION. This function performs no SMC/HVC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuOnCheck {
    MandatoryV0_2,
    QueryFeatures { function_id: u32 },
}
pub fn cpu_on_check(version: u32) -> Option<CpuOnCheck> {
    let major = version >> 16;
    let minor = version & 0xffff;
    if major >= 1 && major < 0x8000 {
        Some(CpuOnCheck::QueryFeatures {
            function_id: 0xc400_0003,
        })
    } else if major == 0 && minor == 2 {
        Some(CpuOnCheck::MandatoryV0_2)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{format, vec::Vec};
    use dtoolkit::model::{DeviceTree, DeviceTreeNode, DeviceTreeProperty};
    fn prop(node: &mut DeviceTreeNode, name: &str, bytes: &[u8]) {
        node.add_property(DeviceTreeProperty::new_unchecked(name, bytes.to_vec()));
    }
    fn fixture(
        cells: u32,
        ids: &[u64],
        method: &[u8],
        disabled: Option<usize>,
        psci: Option<(&[u8], &[u8])>,
    ) -> Vec<u8> {
        let mut tree = DeviceTree::new();
        let mut cpus = DeviceTreeNode::new_unchecked("cpus");
        prop(&mut cpus, "#address-cells", &cells.to_be_bytes());
        prop(&mut cpus, "#size-cells", &0u32.to_be_bytes());
        for (index, id) in ids.iter().enumerate() {
            let mut cpu = DeviceTreeNode::new_unchecked(format!("cpu@{index:x}"));
            prop(&mut cpu, "device_type", b"cpu\0");
            let reg_bytes = id.to_be_bytes();
            prop(
                &mut cpu,
                "reg",
                if cells == 1 {
                    &reg_bytes[4..]
                } else {
                    &reg_bytes
                },
            );
            prop(&mut cpu, "enable-method", method);
            prop(
                &mut cpu,
                "cpu-release-addr",
                &(0xd8 + index as u64 * 8).to_be_bytes(),
            );
            if disabled == Some(index) {
                prop(&mut cpu, "status", b"disabled\0");
            }
            cpus.add_child(cpu);
        }
        tree.root.add_child(cpus);
        if let Some((compatible, method)) = psci {
            let mut node = DeviceTreeNode::new_unchecked("psci");
            prop(&mut node, "compatible", compatible);
            prop(&mut node, "method", method);
            tree.root.add_child(node);
        }
        tree.to_dtb()
    }
    fn parse(bytes: &[u8], boot: u64) -> Result<CpuTopology, CpuDiscoveryError> {
        discover_cpu_topology(Fdt::new(bytes).unwrap(), boot)
    }
    #[test]
    fn boot_affinity_not_low_bits_selects_logical_zero() {
        let bytes = fixture(2, &[0x100, 0x100000001, 0x200], b"spin-table\0", None, None);
        let topology = parse(&bytes, 0x100000001 | (1 << 31)).unwrap();
        assert_eq!(topology.cpus()[0].affinity, 0x100000001);
        assert_eq!(topology.cpus()[0].logical_id, Some(0));
        assert_eq!(topology.cpus()[1].affinity, 0x100);
        assert_eq!(topology.cpus()[2].logical_id, Some(2));
    }
    #[test]
    fn records_disabled_cpus_without_assigning_logical_id() {
        let bytes = fixture(1, &[0, 1, 2, 3], b"spin-table\0", Some(1), None);
        let t = parse(&bytes, 0).unwrap();
        assert_eq!(t.cpus()[1].logical_id, None);
        assert_eq!(t.cpus()[2].logical_id, Some(1));
        assert_eq!(
            t.cpus()[3].startup,
            StartupMethod::SpinTable {
                release_address: 0xf0
            }
        );
        assert_eq!(parse(&bytes, 1), Err(CpuDiscoveryError::BootCpuMissing));
    }
    #[test]
    fn rejects_duplicates_capacity_missing_boot_and_reserved_affinity_bits() {
        for (ids, error) in [
            (&[0, 0][..], CpuDiscoveryError::DuplicateAffinity),
            (&[0, 1, 2, 3, 4][..], CpuDiscoveryError::Capacity),
            (&[1, 2][..], CpuDiscoveryError::BootCpuMissing),
            (&[0, 1 << 24][..], CpuDiscoveryError::InvalidAffinity),
        ] {
            assert_eq!(
                parse(&fixture(2, ids, b"spin-table\0", None, None), 0),
                Err(error)
            );
        }
    }
    #[test]
    fn psci_description_does_not_imply_callability() {
        for (method, conduit) in [
            (&b"smc\0"[..], PsciConduit::Smc),
            (&b"hvc\0"[..], PsciConduit::Hvc),
        ] {
            let bytes = fixture(
                1,
                &[0, 1],
                b"psci\0",
                None,
                Some((b"arm,psci-1.0\0arm,psci-0.2\0", method)),
            );
            let t = parse(&bytes, 0).unwrap();
            assert_eq!(t.psci.unwrap().conduit, conduit);
            assert_eq!(t.psci.unwrap().binding, PsciBinding::V1_0);
            assert_eq!(t.cpus()[1].startup, StartupMethod::Psci);
        }
    }
    #[test]
    fn rejects_missing_psci_and_bad_conduit() {
        assert_eq!(
            parse(&fixture(1, &[0], b"psci\0", None, None), 0),
            Err(CpuDiscoveryError::InvalidPsci)
        );
        assert_eq!(
            parse(
                &fixture(
                    1,
                    &[0],
                    b"psci\0",
                    None,
                    Some((b"arm,psci-0.2\0", b"bad\0"))
                ),
                0
            ),
            Err(CpuDiscoveryError::InvalidPsci)
        );
    }
    #[test]
    fn unsupported_cpu_method_is_recorded_without_fallback() {
        let t = parse(
            &fixture(
                1,
                &[0],
                b"vendor,unknown\0",
                None,
                Some((b"arm,psci-0.2\0", b"smc\0")),
            ),
            0,
        )
        .unwrap();
        assert_eq!(t.cpus()[0].startup, StartupMethod::Unsupported);
    }
    #[test]
    fn version_specific_cpu_on_checks() {
        assert_eq!(cpu_on_check(2), Some(CpuOnCheck::MandatoryV0_2));
        assert_eq!(
            cpu_on_check(0x10000),
            Some(CpuOnCheck::QueryFeatures {
                function_id: 0xc4000003
            })
        );
        assert_eq!(cpu_on_check(u32::MAX), None);
        assert_eq!(cpu_on_check(1), None);
    }
    fn replace(bytes: &mut [u8], path: &str, name: &str, value: &[u8]) {
        let mut fdt = dtoolkit::fdt_mut::FdtMut::new(bytes).unwrap();
        let mut node = fdt.find_node_mut(path).unwrap();
        node.property_mut(name).unwrap().set_value(value).unwrap();
    }
    #[test]
    fn rejects_bad_cell_width_and_nonzero_size_cells() {
        let mut bytes = fixture(1, &[0], b"spin-table\0", None, None);
        replace(&mut bytes, "/cpus", "#address-cells", &3u32.to_be_bytes());
        assert_eq!(parse(&bytes, 0), Err(CpuDiscoveryError::InvalidCells));
        replace(&mut bytes, "/cpus", "#address-cells", &1u32.to_be_bytes());
        replace(&mut bytes, "/cpus", "#size-cells", &1u32.to_be_bytes());
        assert_eq!(parse(&bytes, 0), Err(CpuDiscoveryError::InvalidCells));
    }
    #[test]
    fn rejects_malformed_register_and_release_address() {
        let mut bytes = fixture(1, &[0], b"spin-table\0", None, None);
        replace(
            &mut bytes,
            "/cpus/cpu@0",
            "cpu-release-addr",
            &0xd9u64.to_be_bytes(),
        );
        assert_eq!(
            parse(&bytes, 0),
            Err(CpuDiscoveryError::InvalidReleaseAddress)
        );
        replace(&mut bytes, "/cpus/cpu@0", "cpu-release-addr", &[0; 4]);
        assert_eq!(
            parse(&bytes, 0),
            Err(CpuDiscoveryError::InvalidReleaseAddress)
        );
        replace(&mut bytes, "/cpus/cpu@0", "reg", &[0; 3]);
        assert_eq!(parse(&bytes, 0), Err(CpuDiscoveryError::InvalidCpu));
    }
    #[test]
    fn rejects_unterminated_enable_method_and_missing_cpu_tree() {
        let bytes = fixture(1, &[0], b"spin-table", None, None);
        assert_eq!(parse(&bytes, 0), Err(CpuDiscoveryError::InvalidCpu));
        let bytes = DeviceTree::new().to_dtb();
        assert_eq!(parse(&bytes, 0), Err(CpuDiscoveryError::MissingCpus));
    }
    #[test]
    fn legacy_psci_and_unsupported_version_are_not_modern_interfaces() {
        let bytes = fixture(1, &[0], b"psci\0", None, Some((b"arm,psci\0", b"smc\0")));
        let t = parse(&bytes, 0).unwrap();
        assert_eq!(t.psci.unwrap().binding, PsciBinding::Legacy);
        assert_eq!(t.psci.unwrap().legacy_cpu_on, None);
        assert_eq!(cpu_on_check(0), None);
        assert_eq!(cpu_on_check(3), None);
    }
}
