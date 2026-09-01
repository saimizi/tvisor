//! Allocation-free Device Tree (DTB/FDT v17) serializer for guest EL1 execution.
//!
//! Generates a minimal, valid Devicetree Blob describing:
//! - `/` (root node with `#address-cells = <2>`, `#size-cells = <2>`)
//! - `/chosen` (optional `bootargs`)
//! - `/cpus/cpu@0` (compatible `"arm,cortex-a72"`, `reg = <0>`)
//! - `/memory@<base>` (memory regions covering exact mapped guest RAM)

use core::fmt;

const FDT_MAGIC: u32 = 0xd00dfeed;
const FDT_VERSION: u32 = 17;
const FDT_LAST_COMP_VERSION: u32 = 16;
const FDT_BOOT_CPUID_PHYS: u32 = 0;

const FDT_BEGIN_NODE: u32 = 0x00000001;
const FDT_END_NODE: u32 = 0x00000002;
const FDT_PROP: u32 = 0x00000003;
const FDT_END: u32 = 0x00000009;

const HEADER_SIZE: usize = 40;
const MEM_RSVMAP_SIZE: usize = 16; // One terminating empty reservation (2 x u64 zeros)

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
        let mut list_buf = [0_u8; 128];
        let mut pos = 0;
        for s in strings {
            if pos + s.len() + 1 > list_buf.len() {
                return Err(GuestFdtError::PropertyTooLarge);
            }
            list_buf[pos..pos + s.len()].copy_from_slice(s.as_bytes());
            list_buf[pos + s.len()] = 0;
            pos += s.len() + 1;
        }
        self.prop_str_name(name, &list_buf[..pos], string_table)
    }

    fn finish(mut self, string_table: &StringTable) -> Result<usize, GuestFdtError> {
        self.emit_struct_u32(FDT_END)?;

        let off_dt_struct = (HEADER_SIZE + MEM_RSVMAP_SIZE) as u32;
        let size_dt_struct = (self.struct_pos - off_dt_struct as usize) as u32;

        let off_dt_strings = self.struct_pos;
        let strings_bytes = string_table.as_bytes();
        let size_dt_strings = strings_bytes.len() as u32;

        if off_dt_strings + size_dt_strings as usize > self.buf.len() {
            return Err(GuestFdtError::BufferTooSmall);
        }
        self.buf[off_dt_strings..off_dt_strings + size_dt_strings as usize]
            .copy_from_slice(strings_bytes);

        let totalsize = (off_dt_strings + size_dt_strings as usize) as u32;

        // Populate FDT Header
        self.write_u32_be(0, FDT_MAGIC)?;
        self.write_u32_be(4, totalsize)?;
        self.write_u32_be(8, off_dt_struct)?;
        self.write_u32_be(12, off_dt_strings as u32)?;
        self.write_u32_be(16, HEADER_SIZE as u32)?; // off_mem_rsvmap
        self.write_u32_be(20, FDT_VERSION)?;
        self.write_u32_be(24, FDT_LAST_COMP_VERSION)?;
        self.write_u32_be(28, FDT_BOOT_CPUID_PHYS)?;
        self.write_u32_be(32, size_dt_strings)?;
        self.write_u32_be(36, size_dt_struct)?;

        // Populate empty Memory Reservation Map (terminating entry)
        self.write_u64_be(HEADER_SIZE, 0)?;
        self.write_u64_be(HEADER_SIZE + 8, 0)?;

        Ok(totalsize as usize)
    }
}

struct StringTable {
    buf: [u8; 256],
    len: usize,
}

impl StringTable {
    fn new() -> Self {
        Self {
            buf: [0; 256],
            len: 0,
        }
    }

    fn add_string(&mut self, s: &str) -> Result<u32, GuestFdtError> {
        let bytes = s.as_bytes();
        let target_len = bytes.len() + 1;

        // Search for existing string in table
        if self.len >= target_len {
            let mut i = 0;
            while i + target_len <= self.len {
                if &self.buf[i..i + bytes.len()] == bytes && self.buf[i + bytes.len()] == 0 {
                    return Ok(i as u32);
                }
                while i < self.len && self.buf[i] != 0 {
                    i += 1;
                }
                i += 1; // Skip NUL
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

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// Serializes a minimal, valid guest FDT (DTB) into `buffer`.
///
/// Returns the number of bytes written to `buffer`.
pub fn build_guest_dtb(buffer: &mut [u8], config: &GuestFdtConfig) -> Result<usize, GuestFdtError> {
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
    writer.end_node()?; // /cpus/cpu@0
    writer.end_node()?; // /cpus

    // /memory@<base>
    let primary_base = config.memory_regions[0].base;
    let mut mem_node_name = [0_u8; 32];
    let mem_name = format_mem_node_name(primary_base, &mut mem_node_name);
    writer.begin_node(mem_name)?;
    writer.prop_string("device_type", "memory", &mut string_table)?;

    // reg = <base0 size0 base1 size1 ...> (16 bytes per region)
    let mut reg_bytes = [0_u8; 64];
    let total_len = config.memory_regions.len() * 16;
    for (i, region) in config.memory_regions.iter().enumerate() {
        let offset = i * 16;
        reg_bytes[offset..offset + 8].copy_from_slice(&region.base.to_be_bytes());
        reg_bytes[offset + 8..offset + 16].copy_from_slice(&region.size.to_be_bytes());
    }
    writer.prop_str_name("reg", &reg_bytes[..total_len], &mut string_table)?;
    writer.end_node()?; // /memory@<base>

    writer.end_node()?; // / (root)

    writer.finish(&string_table)
}

fn format_mem_node_name(base: u64, buf: &mut [u8; 32]) -> &str {
    const PREFIX: &[u8] = b"memory@";
    buf[..PREFIX.len()].copy_from_slice(PREFIX);
    let mut pos = PREFIX.len();

    let hex_digits = b"0123456789abcdef";
    let mut started = false;
    for shift in (0..16).rev() {
        let nibble = ((base >> (shift * 4)) & 0xf) as usize;
        if nibble != 0 || started || shift == 0 {
            started = true;
            buf[pos] = hex_digits[nibble];
            pos += 1;
        }
    }

    core::str::from_utf8(&buf[..pos]).unwrap_or("memory")
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtoolkit::Node;
    use dtoolkit::Property;
    use dtoolkit::fdt::Fdt;
    use dtoolkit::standard::NodeStandard;

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

        let dtb_slice = &buf[..size];
        let fdt = Fdt::new(dtb_slice).expect("valid FDT blob");
        let root = fdt.root();

        // Check root properties
        assert_eq!(root.address_cells().unwrap(), 2);
        assert_eq!(root.size_cells().unwrap(), 2);
        let model = root.property("model").expect("model prop");
        assert_eq!(model.as_str().unwrap(), "tvisor-virt-v1");

        // /chosen
        let chosen = root.child("chosen").expect("/chosen node");
        assert!(chosen.property("bootargs").is_none());

        // /cpus/cpu@0
        let cpus = root.child("cpus").expect("/cpus node");
        let cpu0 = cpus.child("cpu@0").expect("/cpus/cpu@0");
        let cpu_compat = cpu0.property("compatible").expect("cpu compatible");
        assert_eq!(cpu_compat.as_str().unwrap(), "arm,cortex-a72");
        assert!(cpu0.property("enable-method").is_none());

        // Memory
        let mem = root
            .child("memory@40000000")
            .expect("/memory@40000000 node");
        let device_type = mem.property("device_type").expect("device_type prop");
        assert_eq!(device_type.as_str().unwrap(), "memory");

        let reg = mem.property("reg").expect("reg prop");
        assert_eq!(reg.value().len(), 48);
        // Region 0
        let base0 = u64::from_be_bytes(reg.value()[0..8].try_into().unwrap());
        let size0 = u64::from_be_bytes(reg.value()[8..16].try_into().unwrap());
        assert_eq!(base0, 0x4000_0000);
        assert_eq!(size0, 0x0000_2000);
        // Region 1
        let base1 = u64::from_be_bytes(reg.value()[16..24].try_into().unwrap());
        let size1 = u64::from_be_bytes(reg.value()[24..32].try_into().unwrap());
        assert_eq!(base1, 0x4000_3000);
        assert_eq!(size1, 0x0000_1000);
        // Region 2
        let base2 = u64::from_be_bytes(reg.value()[32..40].try_into().unwrap());
        let size2 = u64::from_be_bytes(reg.value()[40..48].try_into().unwrap());
        assert_eq!(base2, 0x4010_0000);
        assert_eq!(size2, 0x0000_1000);
    }

    #[test]
    fn rejects_unaligned_ram_config() {
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
