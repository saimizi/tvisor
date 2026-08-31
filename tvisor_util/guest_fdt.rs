//! Allocation-free guest Device Tree (DTB) generator for tvisor.
//!
//! Generates a compliant Flattened Device Tree (FDT v17) describing the
//! guest virtual platform in IPA space without requiring dynamic allocation.

use core::fmt;

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_VERSION: u32 = 17;
const FDT_LAST_COMP_VERSION: u32 = 16;

const FDT_BEGIN_NODE: u32 = 0x0000_0001;
const FDT_END_NODE: u32 = 0x0000_0002;
const FDT_PROP: u32 = 0x0000_0003;
const FDT_END: u32 = 0x0000_0009;

const HEADER_SIZE: usize = 40;
const MEM_RSVMAP_SIZE: usize = 16; // One empty (0, 0) entry

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
pub struct GuestFdtConfig<'a> {
    pub ram_base: u64,
    pub ram_size: u64,
    pub bootargs: Option<&'a str>,
}

struct FdtWriter<'a> {
    buf: &'a mut [u8],
    struct_pos: usize,
}

impl<'a> FdtWriter<'a> {
    fn new(buf: &'a mut [u8]) -> Result<Self, GuestFdtError> {
        if buf.len() < HEADER_SIZE + MEM_RSVMAP_SIZE + 64 {
            return Err(GuestFdtError::BufferTooSmall);
        }
        let struct_pos = HEADER_SIZE + MEM_RSVMAP_SIZE;
        Ok(Self { buf, struct_pos })
    }

    fn write_u32_be(&mut self, offset: usize, val: u32) -> Result<(), GuestFdtError> {
        if offset + 4 > self.buf.len() {
            return Err(GuestFdtError::BufferTooSmall);
        }
        self.buf[offset..offset + 4].copy_from_slice(&val.to_be_bytes());
        Ok(())
    }

    fn write_u64_be(&mut self, offset: usize, val: u64) -> Result<(), GuestFdtError> {
        if offset + 8 > self.buf.len() {
            return Err(GuestFdtError::BufferTooSmall);
        }
        self.buf[offset..offset + 8].copy_from_slice(&val.to_be_bytes());
        Ok(())
    }

    fn emit_struct_u32(&mut self, val: u32) -> Result<(), GuestFdtError> {
        let pos = self.struct_pos;
        self.write_u32_be(pos, val)?;
        self.struct_pos += 4;
        Ok(())
    }

    fn emit_struct_bytes(&mut self, bytes: &[u8]) -> Result<(), GuestFdtError> {
        let len = bytes.len();
        let padded = (len + 3) & !3;
        if self.struct_pos + padded > self.buf.len() {
            return Err(GuestFdtError::BufferTooSmall);
        }
        self.buf[self.struct_pos..self.struct_pos + len].copy_from_slice(bytes);
        for b in &mut self.buf[self.struct_pos + len..self.struct_pos + padded] {
            *b = 0;
        }
        self.struct_pos += padded;
        Ok(())
    }

    fn begin_node(&mut self, name: &str) -> Result<(), GuestFdtError> {
        self.emit_struct_u32(FDT_BEGIN_NODE)?;
        let mut name_buf = [0_u8; 64];
        if name.len() >= name_buf.len() {
            return Err(GuestFdtError::NameTooLong);
        }
        name_buf[..name.len()].copy_from_slice(name.as_bytes());
        name_buf[name.len()] = 0;
        self.emit_struct_bytes(&name_buf[..name.len() + 1])
    }

    fn end_node(&mut self) -> Result<(), GuestFdtError> {
        self.emit_struct_u32(FDT_END_NODE)
    }

    fn prop_str_name(
        &mut self,
        name: &str,
        val: &[u8],
        string_table: &mut StringTable,
    ) -> Result<(), GuestFdtError> {
        let nameoff = string_table.add_string(name)?;
        self.emit_struct_u32(FDT_PROP)?;
        self.emit_struct_u32(val.len() as u32)?;
        self.emit_struct_u32(nameoff)?;
        self.emit_struct_bytes(val)
    }

    fn prop_u32(
        &mut self,
        name: &str,
        val: u32,
        string_table: &mut StringTable,
    ) -> Result<(), GuestFdtError> {
        self.prop_str_name(name, &val.to_be_bytes(), string_table)
    }

    fn prop_string(
        &mut self,
        name: &str,
        val: &str,
        string_table: &mut StringTable,
    ) -> Result<(), GuestFdtError> {
        let mut str_buf = [0_u8; 128];
        if val.len() >= str_buf.len() {
            return Err(GuestFdtError::PropertyTooLarge);
        }
        str_buf[..val.len()].copy_from_slice(val.as_bytes());
        str_buf[val.len()] = 0;
        self.prop_str_name(name, &str_buf[..val.len() + 1], string_table)
    }

    fn prop_string_list(
        &mut self,
        name: &str,
        strings: &[&str],
        string_table: &mut StringTable,
    ) -> Result<(), GuestFdtError> {
        let mut buf = [0_u8; 256];
        let mut pos = 0;
        for s in strings {
            if pos + s.len() + 1 > buf.len() {
                return Err(GuestFdtError::PropertyTooLarge);
            }
            buf[pos..pos + s.len()].copy_from_slice(s.as_bytes());
            buf[pos + s.len()] = 0;
            pos += s.len() + 1;
        }
        self.prop_str_name(name, &buf[..pos], string_table)
    }

    fn finish(
        mut self,
        string_table: &StringTable,
    ) -> Result<usize, GuestFdtError> {
        self.emit_struct_u32(FDT_END)?;

        let off_mem_rsvmap = HEADER_SIZE;
        // Emit empty reservation table entry: address 0, size 0
        self.write_u64_be(off_mem_rsvmap, 0)?;
        self.write_u64_be(off_mem_rsvmap + 8, 0)?;

        let off_dt_struct = HEADER_SIZE + MEM_RSVMAP_SIZE;
        let size_dt_struct = self.struct_pos - off_dt_struct;

        // Place strings block immediately after struct block (aligned to 4)
        let off_dt_strings = (self.struct_pos + 3) & !3;
        let size_dt_strings = string_table.len();

        if off_dt_strings + size_dt_strings > self.buf.len() {
            return Err(GuestFdtError::BufferTooSmall);
        }

        self.buf[off_dt_strings..off_dt_strings + size_dt_strings]
            .copy_from_slice(string_table.as_bytes());

        let totalsize = off_dt_strings + size_dt_strings;

        // Write header
        self.write_u32_be(0, FDT_MAGIC)?;
        self.write_u32_be(4, totalsize as u32)?;
        self.write_u32_be(8, off_dt_struct as u32)?;
        self.write_u32_be(12, off_dt_strings as u32)?;
        self.write_u32_be(16, off_mem_rsvmap as u32)?;
        self.write_u32_be(20, FDT_VERSION)?;
        self.write_u32_be(24, FDT_LAST_COMP_VERSION)?;
        self.write_u32_be(28, 0)?; // boot_cpuid_phys
        self.write_u32_be(32, size_dt_strings as u32)?;
        self.write_u32_be(36, size_dt_struct as u32)?;

        Ok(totalsize)
    }
}

struct StringTable {
    buf: [u8; 512],
    len: usize,
}

impl StringTable {
    const fn new() -> Self {
        Self {
            buf: [0; 512],
            len: 0,
        }
    }

    fn add_string(&mut self, s: &str) -> Result<u32, GuestFdtError> {
        let bytes = s.as_bytes();
        let target_len = bytes.len() + 1;

        // Check if string already exists in string table to dedup
        let mut offset = 0;
        while offset < self.len {
            let existing = &self.buf[offset..];
            if let Some(nul_pos) = existing.iter().position(|&b| b == 0) {
                if &existing[..nul_pos] == bytes {
                    return Ok(offset as u32);
                }
                offset += nul_pos + 1;
            } else {
                break;
            }
        }

        if self.len + target_len > self.buf.len() {
            return Err(GuestFdtError::BufferTooSmall);
        }

        let nameoff = self.len as u32;
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.buf[self.len + bytes.len()] = 0;
        self.len += target_len;
        Ok(nameoff)
    }

    fn len(&self) -> usize {
        self.len
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// Serializes a minimal, valid guest FDT (DTB) into `buffer`.
///
/// Returns the number of bytes written to `buffer`.
pub fn build_guest_dtb(
    buffer: &mut [u8],
    config: &GuestFdtConfig,
) -> Result<usize, GuestFdtError> {
    if config.ram_size == 0 || config.ram_size & 0xfff != 0 || config.ram_base & 0xfff != 0 {
        return Err(GuestFdtError::InvalidConfiguration);
    }

    let mut string_table = StringTable::new();
    let mut writer = FdtWriter::new(buffer)?;

    // / (root node)
    writer.begin_node("")?;
    writer.prop_u32("#address-cells", 2, &mut string_table)?;
    writer.prop_u32("#size-cells", 2, &mut string_table)?;
    writer.prop_string("model", "tvisor-virt-v1", &mut string_table)?;
    writer.prop_string_list(
        "compatible",
        &["tvisor,virt", "linux,dummy-virt"],
        &mut string_table,
    )?;

    // /chosen
    writer.begin_node("chosen")?;
    if let Some(bootargs) = config.bootargs {
        writer.prop_string("bootargs", bootargs, &mut string_table)?;
    }
    writer.end_node()?; // /chosen

    // /cpus
    writer.begin_node("cpus")?;
    writer.prop_u32("#address-cells", 1, &mut string_table)?;
    writer.prop_u32("#size-cells", 0, &mut string_table)?;

    // /cpus/cpu@0
    writer.begin_node("cpu@0")?;
    writer.prop_string("device_type", "cpu", &mut string_table)?;
    writer.prop_string("compatible", "arm,cortex-a72", &mut string_table)?;
    writer.prop_u32("reg", 0, &mut string_table)?;
    writer.prop_string("enable-method", "psci", &mut string_table)?;
    writer.end_node()?; // /cpus/cpu@0
    writer.end_node()?; // /cpus

    // /memory@<base>
    let mut mem_node_name = [0_u8; 32];
    let mem_name = format_mem_node_name(config.ram_base, &mut mem_node_name);
    writer.begin_node(mem_name)?;
    writer.prop_string("device_type", "memory", &mut string_table)?;

    // reg = <ram_base_hi ram_base_lo ram_size_hi ram_size_lo>
    let mut reg_bytes = [0_u8; 16];
    reg_bytes[0..8].copy_from_slice(&config.ram_base.to_be_bytes());
    reg_bytes[8..16].copy_from_slice(&config.ram_size.to_be_bytes());
    writer.prop_str_name("reg", &reg_bytes, &mut string_table)?;
    writer.end_node()?; // /memory@<base>

    writer.end_node()?; // / (root)

    writer.finish(&string_table)
}

fn format_mem_node_name(base: u64, buf: &mut [u8; 32]) -> &str {
    const PREFIX: &[u8] = b"memory@";
    buf[..PREFIX.len()].copy_from_slice(PREFIX);
    let mut pos = PREFIX.len();

    let hex_digits = b"0123456789abcdef";
    let mut shift = 60_i32;
    let mut started = false;
    while shift >= 0 {
        let digit = ((base >> shift) & 0xf) as usize;
        if digit != 0 || started || shift == 0 {
            buf[pos] = hex_digits[digit];
            pos += 1;
            started = true;
        }
        shift -= 4;
    }
    unsafe { core::str::from_utf8_unchecked(&buf[..pos]) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtoolkit::fdt::Fdt;
    use dtoolkit::standard::NodeStandard;
    use dtoolkit::{Node, Property};

    #[test]
    fn builds_valid_guest_dtb_parseable_by_fdt() {
        let mut buf = [0_u8; 2048];
        let config = GuestFdtConfig {
            ram_base: 0x4000_0000,
            ram_size: 0x0020_0000, // 2 MiB
            bootargs: Some("earlycon"),
        };

        let dtb_size = build_guest_dtb(&mut buf, &config).unwrap();
        assert!(dtb_size > HEADER_SIZE);

        let fdt = Fdt::new(&buf[..dtb_size]).expect("valid FDT");
        let root = fdt.root();

        // Model & Compatible
        assert_eq!(root.model().unwrap(), Some("tvisor-virt-v1"));
        let mut compatibles = root.compatible().unwrap();
        assert_eq!(compatibles.next(), Some("tvisor,virt"));
        assert_eq!(compatibles.next(), Some("linux,dummy-virt"));

        // Cells
        assert_eq!(root.address_cells().unwrap(), 2);
        assert_eq!(root.size_cells().unwrap(), 2);

        // Chosen
        let chosen = root.child("chosen").expect("/chosen node");
        let bootargs = chosen.property("bootargs").expect("bootargs prop");
        assert_eq!(bootargs.as_str().unwrap(), "earlycon");

        // CPUs
        let cpus = root.child("cpus").expect("/cpus node");
        assert_eq!(cpus.address_cells().unwrap(), 1);
        assert_eq!(cpus.size_cells().unwrap(), 0);

        let cpu0 = cpus.child("cpu@0").expect("/cpus/cpu@0");
        let cpu_compat = cpu0.property("compatible").expect("cpu compatible");
        assert_eq!(cpu_compat.as_str().unwrap(), "arm,cortex-a72");
        let enable_method = cpu0
            .property("enable-method")
            .expect("enable-method");
        assert_eq!(enable_method.as_str().unwrap(), "psci");

        // Memory
        let mem = root
            .child("memory@40000000")
            .expect("/memory@40000000 node");
        let device_type = mem
            .property("device_type")
            .expect("device_type prop");
        assert_eq!(device_type.as_str().unwrap(), "memory");

        let reg = mem.property("reg").expect("reg prop");
        assert_eq!(reg.value().len(), 16);
        let base = u64::from_be_bytes(reg.value()[0..8].try_into().unwrap());
        let size = u64::from_be_bytes(reg.value()[8..16].try_into().unwrap());
        assert_eq!(base, 0x4000_0000);
        assert_eq!(size, 0x0020_0000);
    }

    #[test]
    fn rejects_unaligned_ram_config() {
        let mut buf = [0_u8; 1024];
        let bad_base = GuestFdtConfig {
            ram_base: 0x4000_0001,
            ram_size: 0x2000,
            bootargs: None,
        };
        assert_eq!(
            build_guest_dtb(&mut buf, &bad_base),
            Err(GuestFdtError::InvalidConfiguration)
        );

        let zero_size = GuestFdtConfig {
            ram_base: 0x4000_0000,
            ram_size: 0,
            bootargs: None,
        };
        assert_eq!(
            build_guest_dtb(&mut buf, &zero_size),
            Err(GuestFdtError::InvalidConfiguration)
        );
    }

    #[test]
    fn rejects_tiny_buffer() {
        let mut buf = [0_u8; 32];
        let config = GuestFdtConfig {
            ram_base: 0x4000_0000,
            ram_size: 0x2000,
            bootargs: None,
        };
        assert_eq!(
            build_guest_dtb(&mut buf, &config),
            Err(GuestFdtError::BufferTooSmall)
        );
    }
}
