//! Stage-2 translation tables used by tvisor for guest EL1/EL0 execution.
//!
//! Phase 9 uses a 39-bit Intermediate Physical Address (IPA) space, a 4 KiB
//! translation granule, and a Level 1 starting lookup:
//!
//! ```text
//! IPA[38:30]   IPA[29:21]   IPA[20:12]   IPA[11:0]
//!  L1 index     L2 index     L3 index    Page offset
//!   9 bits       9 bits       9 bits       12 bits
//! ```
//!
//! With `T0SZ=25` (39 bits) and `SL0=0b01` (Level 1 start), the root table is a
//! single standard 4 KiB page containing 512 64-bit descriptors.
//!
//! Descriptor bits `[1:0]` select the entry type:
//!
//! ```text
//! Level   00          01              11
//! L1      invalid     1 GiB block     next L2 table
//! L2      invalid     2 MiB block     next L3 table
//! L3      invalid     reserved         4 KiB page
//! ```
//!
//! Unlike Stage-1 descriptors (which index `MAIR_EL2`), Stage-2 leaf descriptors
//! directly encode memory attributes and stage-2 access permissions:
//!
//! ```text
//! Bit(s)   Field      Policy
//! 0        Valid      1 for every populated descriptor
//! 1        Type       0 for an L1/L2 block; 1 for a table or L3 page
//! [5:2]    MemAttr    0b1111 = Normal Inner/Outer WB/WA; 0b0001 = Device-nGnRE
//! [7:6]    S2AP       00 = None, 01 = RO, 10 = WO, 11 = RW
//! [9:8]    SH         11 = Inner Shareable for Normal; 00 = Non-shareable for Device
//! 10       AF         1 (Access Flag, set to prevent initial AF fault)
//! [54:53]  XN         00 = Executable; 10 = Execute-Never (XN)
//! ```

use crate::el2_translation::{
    PAGE_SIZE, TablePage, TableStorage, TranslationError, pa_bits_from_parange,
};

pub const IPA_BITS: u8 = 39;

const L1_SHIFT: u32 = 30;
const L2_SHIFT: u32 = 21;
const L3_SHIFT: u32 = 12;
const L1_SIZE: u64 = 1 << L1_SHIFT;
const L2_SIZE: u64 = 1 << L2_SHIFT;
const ADDRESS_MASK: u64 = 0x0000_ffff_ffff_f000;

const VALID: u64 = 1 << 0;
const TABLE_OR_PAGE: u64 = 1 << 1;
const MEM_ATTR_NORMAL_WB_WA: u64 = 0b1111 << 2;
const MEM_ATTR_DEVICE_NGNRE: u64 = 0b0001 << 2;
const S2AP_NONE: u64 = 0b00 << 6;
const S2AP_READ_ONLY: u64 = 0b01 << 6;
const S2AP_WRITE_ONLY: u64 = 0b10 << 6;
const S2AP_READ_WRITE: u64 = 0b11 << 6;
const SH_NONE: u64 = 0b00 << 8;
const SH_INNER: u64 = 0b11 << 8;
const ACCESS_FLAG: u64 = 1 << 10;
const XN_EXEC: u64 = 0b00 << 53;
const XN_NON_EXEC: u64 = 0b10 << 53;

pub const VTCR_EL2_T0SZ_39_BIT: u64 = 25;
pub const VTCR_EL2_SL0_LEVEL_1: u64 = 0b01 << 6;
pub const VTCR_EL2_IRGN0_NORMAL_WB_WA: u64 = 0b01 << 8;
pub const VTCR_EL2_ORGN0_NORMAL_WB_WA: u64 = 0b01 << 10;
pub const VTCR_EL2_SH0_INNER: u64 = 0b11 << 12;
pub const VTCR_EL2_TG0_4KB: u64 = 0b00 << 14;
pub const VTCR_EL2_RES1: u64 = 1 << 31;

pub const HCR_EL2_VM: u64 = 1 << 0;
pub const HCR_EL2_SWIO: u64 = 1 << 1;
pub const HCR_EL2_TSC: u64 = 1 << 19;
pub const HCR_EL2_RW: u64 = 1 << 31;
pub const HCR_EL2_PHASE9_VALUE: u64 = HCR_EL2_RW | HCR_EL2_TSC | HCR_EL2_SWIO | HCR_EL2_VM;

pub const CPTR_EL2_TFP: u64 = 1 << 10;
pub const CPTR_EL2_RES1: u64 = (0b11 << 12) | 0x3ff;
pub const CPTR_EL2_PHASE9_VALUE: u64 = CPTR_EL2_RES1 | CPTR_EL2_TFP;

pub const VMPIDR_EL2_VCPU0: u64 = 0x8000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage2MemoryType {
    NormalWbWa,
    DeviceNgNre,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage2Access {
    None,
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage2Exec {
    Executable,
    ExecuteNever,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stage2Mapping {
    pub ipa: u64,
    pub pa: u64,
    pub size: u64,
    pub mem_type: Stage2MemoryType,
    pub access: Stage2Access,
    pub exec: Stage2Exec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stage2Translation {
    pub pa: u64,
    pub mem_type: Stage2MemoryType,
    pub access: Stage2Access,
    pub exec: Stage2Exec,
    pub level: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stage2RegisterValues {
    pub vtcr_el2: u64,
    pub vttbr_el2: u64,
    pub hcr_el2: u64,
    pub cptr_el2: u64,
    pub vmpidr_el2: u64,
}

pub fn stage2_register_values(
    vmid: u16,
    root_pa: u64,
    parange: u8,
) -> Result<Stage2RegisterValues, TranslationError> {
    let pa_bits = pa_bits_from_parange(parange)?;
    if root_pa & (PAGE_SIZE - 1) != 0 || root_pa >= (1_u64 << pa_bits) {
        return Err(TranslationError::InvalidTableBase);
    }
    let vtcr_el2 = VTCR_EL2_RES1
        | VTCR_EL2_T0SZ_39_BIT
        | VTCR_EL2_SL0_LEVEL_1
        | VTCR_EL2_IRGN0_NORMAL_WB_WA
        | VTCR_EL2_ORGN0_NORMAL_WB_WA
        | VTCR_EL2_SH0_INNER
        | VTCR_EL2_TG0_4KB
        | (((parange as u64) & 0b111) << 16);

    let vttbr_el2 = ((vmid as u64) << 48) | (root_pa & ADDRESS_MASK);

    Ok(Stage2RegisterValues {
        vtcr_el2,
        vttbr_el2,
        hcr_el2: HCR_EL2_PHASE9_VALUE,
        cptr_el2: CPTR_EL2_PHASE9_VALUE,
        vmpidr_el2: VMPIDR_EL2_VCPU0,
    })
}

pub struct Stage2TableSet<'a, const N: usize> {
    storage: &'a mut TableStorage<N>,
    used: usize,
    base_pa: u64,
    pa_bits: u8,
}

impl<'a, const N: usize> Stage2TableSet<'a, N> {
    pub fn new(
        storage: &'a mut TableStorage<N>,
        base_pa: u64,
        pa_bits: u8,
    ) -> Result<Self, TranslationError> {
        if base_pa & (PAGE_SIZE - 1) != 0 {
            return Err(TranslationError::InvalidTableBase);
        }
        if !(32..=48).contains(&pa_bits) {
            return Err(TranslationError::InvalidPaBits);
        }
        if N == 0 {
            return Err(TranslationError::TableExhausted);
        }

        let byte_len = (N as u64)
            .checked_mul(PAGE_SIZE)
            .ok_or(TranslationError::AddressOverflow)?;
        let end = base_pa
            .checked_add(byte_len)
            .ok_or(TranslationError::AddressOverflow)?;
        if end > (1_u64 << pa_bits) {
            return Err(TranslationError::PhysicalAddressOutOfRange);
        }

        let mut set = Self {
            storage,
            used: 0,
            base_pa,
            pa_bits,
        };
        // Allocate L1 root table page (first page of storage)
        set.allocate_page()?;
        Ok(set)
    }

    pub const fn root_pa(&self) -> u64 {
        self.base_pa
    }

    pub const fn used_pages(&self) -> usize {
        self.used
    }

    pub fn page(&self, index: usize) -> Option<&TablePage> {
        self.storage.pages().get(index).filter(|_| index < self.used)
    }

    pub fn map(&mut self, mapping: Stage2Mapping) -> Result<(), TranslationError> {
        self.validate_mapping(mapping)?;
        let mut ipa = mapping.ipa;
        let mut pa = mapping.pa;
        let mut remaining = mapping.size;
        while remaining != 0 {
            let (level, chunk) = if aligned_for(ipa, pa, L1_SIZE) && remaining >= L1_SIZE {
                (1, L1_SIZE)
            } else if aligned_for(ipa, pa, L2_SIZE) && remaining >= L2_SIZE {
                (2, L2_SIZE)
            } else {
                (3, PAGE_SIZE)
            };
            self.map_leaf(level, ipa, pa, mapping)?;
            ipa += chunk;
            pa += chunk;
            remaining -= chunk;
        }
        Ok(())
    }

    pub fn walk(&self, ipa: u64) -> Result<Option<Stage2Translation>, TranslationError> {
        if ipa >= (1_u64 << IPA_BITS) {
            return Err(TranslationError::VirtualAddressOutOfRange);
        }
        let mut table = 0;
        for level in 1..=3 {
            let index = index_for(level, ipa);
            let descriptor = self.storage.pages()[table].entries()[index];
            if descriptor & VALID == 0 {
                return Ok(None);
            }
            let is_table_or_page = descriptor & TABLE_OR_PAGE != 0;
            if level < 3 && is_table_or_page {
                table = self.table_index(descriptor & ADDRESS_MASK)?;
                continue;
            }
            if level == 3 && !is_table_or_page {
                return Err(TranslationError::CorruptTable);
            }

            let block_size = level_size(level);
            let base = descriptor & output_mask(level);
            let pa = base | (ipa & (block_size - 1));

            let mem_type = match descriptor & (0b1111 << 2) {
                MEM_ATTR_NORMAL_WB_WA => Stage2MemoryType::NormalWbWa,
                MEM_ATTR_DEVICE_NGNRE => Stage2MemoryType::DeviceNgNre,
                _ => return Err(TranslationError::CorruptTable),
            };

            let access = match descriptor & (0b11 << 6) {
                S2AP_NONE => Stage2Access::None,
                S2AP_READ_ONLY => Stage2Access::ReadOnly,
                S2AP_WRITE_ONLY => Stage2Access::WriteOnly,
                S2AP_READ_WRITE => Stage2Access::ReadWrite,
                _ => return Err(TranslationError::CorruptTable),
            };

            let exec = match descriptor & (0b11 << 53) {
                XN_EXEC => Stage2Exec::Executable,
                XN_NON_EXEC => Stage2Exec::ExecuteNever,
                _ => return Err(TranslationError::CorruptTable),
            };

            return Ok(Some(Stage2Translation {
                pa,
                mem_type,
                access,
                exec,
                level,
            }));
        }
        Err(TranslationError::CorruptTable)
    }

    fn validate_mapping(&self, mapping: Stage2Mapping) -> Result<(), TranslationError> {
        if mapping.size == 0 {
            return Err(TranslationError::EmptyMapping);
        }
        if (mapping.ipa | mapping.pa | mapping.size) & (PAGE_SIZE - 1) != 0 {
            return Err(TranslationError::UnalignedMapping);
        }
        let ipa_end = mapping
            .ipa
            .checked_add(mapping.size)
            .ok_or(TranslationError::AddressOverflow)?;
        let pa_end = mapping
            .pa
            .checked_add(mapping.size)
            .ok_or(TranslationError::AddressOverflow)?;
        if ipa_end > (1_u64 << IPA_BITS) {
            return Err(TranslationError::VirtualAddressOutOfRange);
        }
        if pa_end > (1_u64 << self.pa_bits) {
            return Err(TranslationError::PhysicalAddressOutOfRange);
        }
        Ok(())
    }

    fn map_leaf(
        &mut self,
        level: u8,
        ipa: u64,
        pa: u64,
        mapping: Stage2Mapping,
    ) -> Result<(), TranslationError> {
        let mut table = 0;
        for current_level in 1..level {
            let index = index_for(current_level, ipa);
            let descriptor = self.storage.pages()[table].entries()[index];
            table = if descriptor == 0 {
                let child = self.allocate_page()?;
                self.storage.pages_mut()[table].entries_mut()[index] =
                    self.table_pa(child) | VALID | TABLE_OR_PAGE;
                child
            } else if descriptor & (VALID | TABLE_OR_PAGE) == (VALID | TABLE_OR_PAGE) {
                self.table_index(descriptor & ADDRESS_MASK)?
            } else {
                return Err(TranslationError::ConflictingEntry);
            };
        }
        let index = index_for(level, ipa);
        let descriptor = leaf_descriptor(level, pa, mapping);
        let entry = &mut self.storage.pages_mut()[table].entries_mut()[index];
        if *entry != 0 && *entry != descriptor {
            return Err(TranslationError::ConflictingEntry);
        }
        *entry = descriptor;
        Ok(())
    }

    fn allocate_page(&mut self) -> Result<usize, TranslationError> {
        if self.used == N {
            return Err(TranslationError::TableExhausted);
        }
        let index = self.used;
        self.used += 1;
        Ok(index)
    }

    fn table_pa(&self, index: usize) -> u64 {
        self.base_pa + (index as u64) * PAGE_SIZE
    }

    fn table_index(&self, pa: u64) -> Result<usize, TranslationError> {
        if pa < self.base_pa {
            return Err(TranslationError::CorruptTable);
        }
        let offset = pa - self.base_pa;
        if offset & (PAGE_SIZE - 1) != 0 {
            return Err(TranslationError::CorruptTable);
        }
        let index = (offset / PAGE_SIZE) as usize;
        if index >= self.used {
            return Err(TranslationError::CorruptTable);
        }
        Ok(index)
    }
}

const fn aligned_for(ipa: u64, pa: u64, size: u64) -> bool {
    ipa & (size - 1) == 0 && pa & (size - 1) == 0
}

const fn level_size(level: u8) -> u64 {
    match level {
        1 => L1_SIZE,
        2 => L2_SIZE,
        3 => PAGE_SIZE,
        _ => 0,
    }
}

const fn output_mask(level: u8) -> u64 {
    match level {
        1 => 0x0000_ffff_c000_0000,
        2 => 0x0000_ffff_ffe0_0000,
        3 => 0x0000_ffff_ffff_f000,
        _ => 0,
    }
}

const fn index_for(level: u8, ipa: u64) -> usize {
    let shift = match level {
        1 => L1_SHIFT,
        2 => L2_SHIFT,
        3 => L3_SHIFT,
        _ => 0,
    };
    ((ipa >> shift) & 0x1ff) as usize
}

fn leaf_descriptor(level: u8, pa: u64, mapping: Stage2Mapping) -> u64 {
    let mem_attr = match mapping.mem_type {
        Stage2MemoryType::NormalWbWa => MEM_ATTR_NORMAL_WB_WA,
        Stage2MemoryType::DeviceNgNre => MEM_ATTR_DEVICE_NGNRE,
    };
    let s2ap = match mapping.access {
        Stage2Access::None => S2AP_NONE,
        Stage2Access::ReadOnly => S2AP_READ_ONLY,
        Stage2Access::WriteOnly => S2AP_WRITE_ONLY,
        Stage2Access::ReadWrite => S2AP_READ_WRITE,
    };
    let sh = match mapping.mem_type {
        Stage2MemoryType::NormalWbWa => SH_INNER,
        Stage2MemoryType::DeviceNgNre => SH_NONE,
    };
    let xn = match mapping.exec {
        Stage2Exec::Executable => XN_EXEC,
        Stage2Exec::ExecuteNever => XN_NON_EXEC,
    };
    let type_bit = if level == 3 { TABLE_OR_PAGE } else { 0 };
    (pa & output_mask(level))
        | VALID
        | type_bit
        | mem_attr
        | s2ap
        | sh
        | ACCESS_FLAG
        | xn
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage2_register_values_encodes_expected_fields() {
        let root_pa = 0x3000_4000;
        let parange = 4; // 44-bit PA
        let regs = stage2_register_values(1, root_pa, parange).unwrap();

        // Check VTCR_EL2 fields
        assert_eq!(regs.vtcr_el2 & 0x3f, 25); // T0SZ = 25
        assert_eq!((regs.vtcr_el2 >> 6) & 0b11, 1); // SL0 = 1
        assert_eq!((regs.vtcr_el2 >> 8) & 0b11, 1); // IRGN0 = 1
        assert_eq!((regs.vtcr_el2 >> 10) & 0b11, 1); // ORGN0 = 1
        assert_eq!((regs.vtcr_el2 >> 12) & 0b11, 3); // SH0 = 3 (inner)
        assert_eq!((regs.vtcr_el2 >> 14) & 0b11, 0); // TG0 = 0 (4KB)
        assert_eq!((regs.vtcr_el2 >> 16) & 0b111, 4); // PS = 4
        assert_ne!(regs.vtcr_el2 & (1 << 31), 0); // RES1 set

        // Check VTTBR_EL2
        assert_eq!(regs.vttbr_el2 >> 48, 1); // VMID = 1
        assert_eq!(regs.vttbr_el2 & 0x0000_ffff_ffff_f000, root_pa);

        // Check HCR_EL2 and CPTR_EL2
        assert_eq!(regs.hcr_el2, HCR_EL2_PHASE9_VALUE);
        assert_eq!(regs.cptr_el2, CPTR_EL2_PHASE9_VALUE);
        assert_eq!(regs.vmpidr_el2, VMPIDR_EL2_VCPU0);
    }

    #[test]
    fn stage2_maps_and_walks_leaf_pages_with_permissions() {
        let mut storage = TableStorage::<8>::zeroed();
        let mut tables = Stage2TableSet::new(&mut storage, 0x1000_0000, 40).unwrap();

        let guest_ram = Stage2Mapping {
            ipa: 0x4000_0000,
            pa: 0x0500_0000,
            size: 0x20_0000, // 2 MiB
            mem_type: Stage2MemoryType::NormalWbWa,
            access: Stage2Access::ReadWrite,
            exec: Stage2Exec::Executable,
        };
        tables.map(guest_ram).unwrap();

        // Walk entry IPA
        let t = tables.walk(0x4000_0000).unwrap().unwrap();
        assert_eq!(t.pa, 0x0500_0000);
        assert_eq!(t.mem_type, Stage2MemoryType::NormalWbWa);
        assert_eq!(t.access, Stage2Access::ReadWrite);
        assert_eq!(t.exec, Stage2Exec::Executable);

        // Walk middle page
        let t_mid = tables.walk(0x4001_1000).unwrap().unwrap();
        assert_eq!(t_mid.pa, 0x0501_1000);

        // Unmapped IPA must return None
        assert_eq!(tables.walk(0x3000_0000).unwrap(), None);
        assert_eq!(tables.walk(0x4020_0000).unwrap(), None);
    }

    #[test]
    fn stage2_encodes_device_and_readonly_attributes() {
        let mut storage = TableStorage::<8>::zeroed();
        let mut tables = Stage2TableSet::new(&mut storage, 0x1000_0000, 40).unwrap();

        let dtb_mapping = Stage2Mapping {
            ipa: 0x4010_0000,
            pa: 0x0600_0000,
            size: 0x1000,
            mem_type: Stage2MemoryType::NormalWbWa,
            access: Stage2Access::ReadOnly,
            exec: Stage2Exec::ExecuteNever,
        };
        tables.map(dtb_mapping).unwrap();

        let t = tables.walk(0x4010_0000).unwrap().unwrap();
        assert_eq!(t.pa, 0x0600_0000);
        assert_eq!(t.access, Stage2Access::ReadOnly);
        assert_eq!(t.exec, Stage2Exec::ExecuteNever);
    }

    #[test]
    fn stage2_rejects_unaligned_and_overflowing_mappings() {
        let mut storage = TableStorage::<4>::zeroed();
        let mut tables = Stage2TableSet::new(&mut storage, 0x1000_0000, 40).unwrap();

        // Empty size
        assert_eq!(
            tables.map(Stage2Mapping {
                ipa: 0x4000_0000,
                pa: 0x0500_0000,
                size: 0,
                mem_type: Stage2MemoryType::NormalWbWa,
                access: Stage2Access::ReadWrite,
                exec: Stage2Exec::Executable,
            }),
            Err(TranslationError::EmptyMapping)
        );

        // Unaligned IPA
        assert_eq!(
            tables.map(Stage2Mapping {
                ipa: 0x4000_0001,
                pa: 0x0500_0000,
                size: 0x1000,
                mem_type: Stage2MemoryType::NormalWbWa,
                access: Stage2Access::ReadWrite,
                exec: Stage2Exec::Executable,
            }),
            Err(TranslationError::UnalignedMapping)
        );

        // Out of 39-bit IPA space (>= 512 GiB)
        assert_eq!(
            tables.map(Stage2Mapping {
                ipa: 1 << 39,
                pa: 0x0500_0000,
                size: 0x1000,
                mem_type: Stage2MemoryType::NormalWbWa,
                access: Stage2Access::ReadWrite,
                exec: Stage2Exec::Executable,
            }),
            Err(TranslationError::VirtualAddressOutOfRange)
        );
    }
}
