//! Guest Device Tree (DTB/FDT v17) construction for EL1 execution.
//!
//! Uses dtoolkit's mutable model to describe the guest platform and serialize
//! it into the caller-provided guest-memory buffer. Building the intermediate
//! tree requires the global Rust heap to have been initialized.
//!
//! Generates a minimal, valid Devicetree Blob describing:
//! - `/` (root node with `#address-cells = <2>`, `#size-cells = <2>`)
//! - `/chosen` (optional `bootargs`)
//! - `/cpus/cpu@0` (compatible `"arm,cortex-a72"`, `reg = <0>`)
//! - `/memory@<base>` (memory regions covering exact mapped guest RAM)

use alloc::{format, vec::Vec};
use core::fmt;

use dtoolkit::model::{DeviceTree, DeviceTreeNode, DeviceTreeProperty};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestFdtError {
    BufferTooSmall,
    InvalidConfiguration,
    NameTooLong,
    PropertyTooLarge,
}

impl fmt::Display for GuestFdtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferTooSmall => f.write_str("guest DTB buffer capacity exceeded"),
            Self::InvalidConfiguration => f.write_str("invalid guest DTB configuration"),
            Self::NameTooLong => f.write_str("node or property name exceeds maximum limit"),
            Self::PropertyTooLarge => f.write_str("property value exceeds maximum limit"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestMemoryRegion {
    pub base: u64,
    pub size: u64,
}

pub const MAX_GUEST_MEMORY_REGIONS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestFdtConfig<'a> {
    pub memory_regions: &'a [GuestMemoryRegion],
    pub bootargs: Option<&'a str>,
}

fn property(name: &str, value: impl Into<Vec<u8>>) -> DeviceTreeProperty {
    // All property names in this module are compile-time constants known to
    // satisfy the Devicetree naming rules.
    DeviceTreeProperty::new_unchecked(name, value)
}

fn string_value(value: &str) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(value.len() + 1);
    encoded.extend_from_slice(value.as_bytes());
    encoded.push(0);
    encoded
}

fn string_list_value(values: &[&str]) -> Vec<u8> {
    let size = values.iter().map(|value| value.len() + 1).sum();
    let mut encoded = Vec::with_capacity(size);
    for value in values {
        encoded.extend_from_slice(value.as_bytes());
        encoded.push(0);
    }
    encoded
}

fn validate_config(config: &GuestFdtConfig<'_>) -> Result<(), GuestFdtError> {
    if config.memory_regions.is_empty() || config.memory_regions.len() > MAX_GUEST_MEMORY_REGIONS {
        return Err(GuestFdtError::InvalidConfiguration);
    }

    for region in config.memory_regions {
        if region.size == 0
            || region.size & 0xfff != 0
            || region.base & 0xfff != 0
            || region.base.checked_add(region.size).is_none()
        {
            return Err(GuestFdtError::InvalidConfiguration);
        }
    }

    // Preserve the limit enforced by the former fixed-size serializer.
    if config
        .bootargs
        .is_some_and(|bootargs| bootargs.len() >= 128)
    {
        return Err(GuestFdtError::PropertyTooLarge);
    }

    Ok(())
}

fn build_tree(config: &GuestFdtConfig<'_>) -> DeviceTree {
    let mut tree = DeviceTree::new();
    tree.root
        .add_property(property("#address-cells", 2_u32.to_be_bytes().to_vec()));
    tree.root
        .add_property(property("#size-cells", 2_u32.to_be_bytes().to_vec()));
    tree.root
        .add_property(property("model", string_value("tvisor-virt-v1")));
    tree.root.add_property(property(
        "compatible",
        string_list_value(&["tvisor,virt", "linux,dummy-virt"]),
    ));

    let mut chosen = DeviceTreeNode::new_unchecked("chosen");
    if let Some(bootargs) = config.bootargs {
        chosen.add_property(property("bootargs", string_value(bootargs)));
    }
    tree.root.add_child(chosen);

    let mut cpu = DeviceTreeNode::new_unchecked("cpu@0");
    cpu.add_property(property("device_type", string_value("cpu")));
    cpu.add_property(property("compatible", string_value("arm,cortex-a72")));
    cpu.add_property(property("reg", 0_u32.to_be_bytes().to_vec()));

    let mut cpus = DeviceTreeNode::new_unchecked("cpus");
    cpus.add_property(property("#address-cells", 1_u32.to_be_bytes().to_vec()));
    cpus.add_property(property("#size-cells", 0_u32.to_be_bytes().to_vec()));
    cpus.add_child(cpu);
    tree.root.add_child(cpus);

    let primary_base = config.memory_regions[0].base;
    let mut memory = DeviceTreeNode::new_unchecked(format!("memory@{primary_base:x}"));
    memory.add_property(property("device_type", string_value("memory")));

    let mut reg = Vec::with_capacity(config.memory_regions.len() * 16);
    for region in config.memory_regions {
        reg.extend_from_slice(&region.base.to_be_bytes());
        reg.extend_from_slice(&region.size.to_be_bytes());
    }
    memory.add_property(property("reg", reg));
    tree.root.add_child(memory);

    tree
}

/// Serializes a minimal guest FDT into `buffer`.
///
/// Returns the number of bytes written. The global Rust allocator must be
/// initialized before calling this function on the target.
pub fn build_guest_dtb(
    buffer: &mut [u8],
    config: &GuestFdtConfig<'_>,
) -> Result<usize, GuestFdtError> {
    validate_config(config)?;

    let dtb = build_tree(config).to_dtb();
    if dtb.len() > buffer.len() {
        return Err(GuestFdtError::BufferTooSmall);
    }

    buffer[..dtb.len()].copy_from_slice(&dtb);
    Ok(dtb.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtoolkit::fdt::Fdt;
    use dtoolkit::standard::NodeStandard;
    use dtoolkit::{Node, Property};

    #[test]
    fn builds_valid_guest_dtb_parseable_by_fdt() {
        let mut buf = [0_u8; 1024];
        let mem_regions = [
            GuestMemoryRegion {
                base: 0x4000_0000,
                size: 0x0000_2000,
            },
            GuestMemoryRegion {
                base: 0x4000_3000,
                size: 0x0000_1000,
            },
            GuestMemoryRegion {
                base: 0x4010_0000,
                size: 0x0000_1000,
            },
        ];
        let config = GuestFdtConfig {
            memory_regions: &mem_regions,
            bootargs: None,
        };

        let size = build_guest_dtb(&mut buf, &config).expect("build guest dtb");
        assert!(size > 0 && size <= buf.len());

        let fdt = Fdt::new(&buf[..size]).expect("valid FDT blob");
        let root = fdt.root();

        assert_eq!(root.address_cells().unwrap(), 2);
        assert_eq!(root.size_cells().unwrap(), 2);
        assert_eq!(
            root.property("model").unwrap().as_str().unwrap(),
            "tvisor-virt-v1"
        );
        let compatible: Vec<_> = root.property("compatible").unwrap().as_str_list().collect();
        assert_eq!(compatible, ["tvisor,virt", "linux,dummy-virt"]);

        let chosen = root.child("chosen").expect("/chosen node");
        assert!(chosen.property("bootargs").is_none());

        let cpus = root.child("cpus").expect("/cpus node");
        let cpu0 = cpus.child("cpu@0").expect("/cpus/cpu@0");
        assert_eq!(
            cpu0.property("compatible").unwrap().as_str().unwrap(),
            "arm,cortex-a72"
        );
        assert!(cpu0.property("enable-method").is_none());

        let mem = root
            .child("memory@40000000")
            .expect("/memory@40000000 node");
        assert_eq!(
            mem.property("device_type").unwrap().as_str().unwrap(),
            "memory"
        );

        let reg = mem.property("reg").expect("reg prop");
        assert_eq!(reg.value().len(), 48);
        assert_eq!(
            u64::from_be_bytes(reg.value()[0..8].try_into().unwrap()),
            0x4000_0000
        );
        assert_eq!(
            u64::from_be_bytes(reg.value()[8..16].try_into().unwrap()),
            0x0000_2000
        );
        assert_eq!(
            u64::from_be_bytes(reg.value()[16..24].try_into().unwrap()),
            0x4000_3000
        );
        assert_eq!(
            u64::from_be_bytes(reg.value()[24..32].try_into().unwrap()),
            0x0000_1000
        );
        assert_eq!(
            u64::from_be_bytes(reg.value()[32..40].try_into().unwrap()),
            0x4010_0000
        );
        assert_eq!(
            u64::from_be_bytes(reg.value()[40..48].try_into().unwrap()),
            0x0000_1000
        );
    }

    #[test]
    fn includes_bootargs() {
        let mut buf = [0_u8; 1024];
        let mem_regions = [GuestMemoryRegion {
            base: 0x4000_0000,
            size: 0x0020_0000,
        }];
        let config = GuestFdtConfig {
            memory_regions: &mem_regions,
            bootargs: Some("console=hvc0"),
        };

        let size = build_guest_dtb(&mut buf, &config).expect("build guest dtb");
        let fdt = Fdt::new(&buf[..size]).expect("valid FDT blob");
        assert_eq!(
            fdt.root()
                .child("chosen")
                .unwrap()
                .property("bootargs")
                .unwrap()
                .as_str()
                .unwrap(),
            "console=hvc0"
        );
    }

    #[test]
    fn rejects_invalid_ram_config() {
        let mut buf = [0_u8; 1024];
        let invalid_regions = [GuestMemoryRegion {
            base: 0x4000_0001,
            size: 0x0020_0000,
        }];
        let config = GuestFdtConfig {
            memory_regions: &invalid_regions,
            bootargs: None,
        };
        assert_eq!(
            build_guest_dtb(&mut buf, &config),
            Err(GuestFdtError::InvalidConfiguration)
        );

        let empty_regions: [GuestMemoryRegion; 0] = [];
        let config_empty = GuestFdtConfig {
            memory_regions: &empty_regions,
            bootargs: None,
        };
        assert_eq!(
            build_guest_dtb(&mut buf, &config_empty),
            Err(GuestFdtError::InvalidConfiguration)
        );
    }

    #[test]
    fn rejects_tiny_buffer() {
        let mut buf = [0_u8; 32];
        let mem_regions = [GuestMemoryRegion {
            base: 0x4000_0000,
            size: 0x0020_0000,
        }];
        let config = GuestFdtConfig {
            memory_regions: &mem_regions,
            bootargs: None,
        };
        assert_eq!(
            build_guest_dtb(&mut buf, &config),
            Err(GuestFdtError::BufferTooSmall)
        );
    }
}
