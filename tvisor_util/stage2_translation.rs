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
//! In Phase 9, only 4 KiB leaf page descriptors at Level 3 are constructed.
//! Block descriptors at Level 1 and Level 2 are deferred to future phases.
//!
//! Leaf descriptor bit encoding:
//! ```text
//! Bit(s)   Field      Policy
//! 0        Valid      1 for every populated descriptor
//! 1        Type       1 for L3 4 KiB page (and table at L1/L2)
//! [5:2]    MemAttr    0b1111 = Normal Inner/Outer WB/WA; 0b0001 = Device-nGnRE
//! [7:6]    S2AP       00 = None, 01 = RO, 10 = WO, 11 = RW
//! [9:8]    SH         11 = Inner Shareable for Normal; 00 = Non-shareable for Device
//! 10       AF         1 (Access Flag, set to prevent initial AF fault)
//! [54:53]  XN         00 = Executable; 10 = Execute-Never (XN)
//! ```

use crate::el2_translation::{PAGE_SIZE, TablePage, TranslationError, pa_bits_from_parange};

pub const IPA_BITS: u8 = 39;

const L1_SHIFT: u32 = 30;
const L2_SHIFT: u32 = 21;
const L3_SHIFT: u32 = 12;
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
/// CPTR_EL2 value used while tvisor executes at EL2. TFP remains clear because
/// Rust and compiler-generated routines may use FP/Advanced SIMD instructions.
pub const CPTR_EL2_PHASE9_VALUE: u64 = CPTR_EL2_RES1;

/// Virtual MPIDR_EL1 for vCPU 0 on a virtual uniprocessor system.
/// Bit 31 = RES1 (1)
/// Bit 30 = U (1, uniprocessor)
/// Bits [23:0] = 0 (Affinity 0.0.0)
pub const VMPIDR_EL2_VCPU0: u64 = (1 << 31) | (1 << 30);

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stage2RegisterValues {
    pub vtcr_el2: u64,
    pub vttbr_el2: u64,
    pub hcr_el2: u64,
    pub cptr_el2: u64,
    pub vmpidr_el2: u64,
}

/// Trait for on-demand page table allocation for Stage-2 translation.
///
/// # Safety
///
/// Implementors must ensure that `allocate_table_page()` returns a physical address that:
/// 1. Is 4 KiB aligned (`pa & 0xfff == 0`).
/// 2. Is representable within the physical address range (`pa < (1 << pa_bits)`).
/// 3. Is identity-mapped and writable in EL2 virtual address space (`pa as *mut TablePage` is valid for reads and writes of 4096 bytes).
/// 4. Is zero-initialized.
/// 5. Is exclusively owned and will not alias another live table page for the lifetime of the table set.
pub unsafe trait Stage2Allocator {
    /// Allocates a single, zeroed 4 KiB page for a table level and returns its PA.
    fn allocate_table_page(&mut self) -> Result<u64, TranslationError>;
}

/// Stage-2 translation table set managing on-demand page table trees.
pub struct Stage2TableSet<A: Stage2Allocator> {
    root_pa: u64,
    pa_bits: u8,
    allocator: A,
    allocated_tables: [u64; 32],
    allocated_count: usize,
}

impl<A: Stage2Allocator> Stage2TableSet<A> {
    pub fn new(mut allocator: A, pa_bits: u8) -> Result<Self, TranslationError> {
        if !(32..=48).contains(&pa_bits) {
            return Err(TranslationError::InvalidPaBits);
        }
        let root_pa = allocator.allocate_table_page()?;
        let max_pa = (1_u64 << pa_bits) - 1;
        if root_pa & (PAGE_SIZE - 1) != 0 || root_pa > max_pa {
            return Err(TranslationError::InvalidTableBase);
        }

        let mut set = Self {
            root_pa,
            pa_bits,
            allocator,
            allocated_tables: [0; 32],
            allocated_count: 0,
        };
        set.allocated_tables[0] = root_pa;
        set.allocated_count = 1;
        Ok(set)
    }

    pub fn root_pa(&self) -> u64 {
        self.root_pa
    }

    pub fn allocated_tables(&self) -> &[u64] {
        &self.allocated_tables[..self.allocated_count]
    }

    pub fn used_pages(&self) -> usize {
        self.allocated_count
    }

    fn allocate_subtable(&mut self) -> Result<u64, TranslationError> {
        if self.allocated_count >= self.allocated_tables.len() {
            return Err(TranslationError::TableExhausted);
        }
        let page_pa = self.allocator.allocate_table_page()?;
        let max_pa = (1_u64 << self.pa_bits) - 1;
        if page_pa & (PAGE_SIZE - 1) != 0 || page_pa > max_pa {
            return Err(TranslationError::InvalidTableBase);
        }
        self.allocated_tables[self.allocated_count] = page_pa;
        self.allocated_count += 1;
        Ok(page_pa)
    }

    pub fn map(&mut self, mapping: Stage2Mapping) -> Result<(), TranslationError> {
        let max_ipa = (1_u64 << IPA_BITS) - 1;
        let max_pa = (1_u64 << self.pa_bits) - 1;

        if mapping.size == 0 {
            return Err(TranslationError::EmptyMapping);
        }
        if mapping.ipa & (PAGE_SIZE - 1) != 0
            || mapping.pa & (PAGE_SIZE - 1) != 0
            || mapping.size & (PAGE_SIZE - 1) != 0
        {
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

        if ipa_end - 1 > max_ipa {
            return Err(TranslationError::VirtualAddressOutOfRange);
        }
        if pa_end - 1 > max_pa {
            return Err(TranslationError::PhysicalAddressOutOfRange);
        }

        let mut current_ipa = mapping.ipa;
        let mut current_pa = mapping.pa;

        while current_ipa < ipa_end {
            self.map_l3_page(
                current_ipa,
                current_pa,
                mapping.mem_type,
                mapping.access,
                mapping.exec,
            )?;
            current_ipa += PAGE_SIZE;
            current_pa += PAGE_SIZE;
        }

        Ok(())
    }

    fn map_l3_page(
        &mut self,
        ipa: u64,
        pa: u64,
        mem_type: Stage2MemoryType,
        access: Stage2Access,
        exec: Stage2Exec,
    ) -> Result<(), TranslationError> {
        let l1_idx = ((ipa >> L1_SHIFT) & 0x1ff) as usize;
        let l2_idx = ((ipa >> L2_SHIFT) & 0x1ff) as usize;
        let l3_idx = ((ipa >> L3_SHIFT) & 0x1ff) as usize;

        // Level 1 lookup
        let root_table = unsafe { &mut *(self.root_pa as *mut TablePage) };
        let l1_entry = root_table.entries()[l1_idx];
        let l2_pa = if l1_entry & VALID != 0 {
            if l1_entry & TABLE_OR_PAGE == 0 {
                return Err(TranslationError::ConflictingEntry);
            }
            l1_entry & ADDRESS_MASK
        } else {
            let next_pa = self.allocate_subtable()?;
            root_table.entries_mut()[l1_idx] = VALID | TABLE_OR_PAGE | (next_pa & ADDRESS_MASK);
            next_pa
        };

        // Level 2 lookup
        let l2_table = unsafe { &mut *(l2_pa as *mut TablePage) };
        let l2_entry = l2_table.entries()[l2_idx];
        let l3_pa = if l2_entry & VALID != 0 {
            if l2_entry & TABLE_OR_PAGE == 0 {
                return Err(TranslationError::ConflictingEntry);
            }
            l2_entry & ADDRESS_MASK
        } else {
            let next_pa = self.allocate_subtable()?;
            l2_table.entries_mut()[l2_idx] = VALID | TABLE_OR_PAGE | (next_pa & ADDRESS_MASK);
            next_pa
        };

        // Level 3 leaf page insertion
        let l3_table = unsafe { &mut *(l3_pa as *mut TablePage) };
        let current_desc = l3_table.entries()[l3_idx];
        let new_desc = encode_l3_page_descriptor(pa, mem_type, access, exec);

        if current_desc & VALID != 0 {
            if current_desc != new_desc {
                return Err(TranslationError::ConflictingEntry);
            }
            return Ok(());
        }

        l3_table.entries_mut()[l3_idx] = new_desc;
        Ok(())
    }

    pub fn translate(&self, ipa: u64) -> Option<Stage2Translation> {
        if ipa >> IPA_BITS != 0 {
            return None;
        }

        let l1_idx = ((ipa >> L1_SHIFT) & 0x1ff) as usize;
        let l2_idx = ((ipa >> L2_SHIFT) & 0x1ff) as usize;
        let l3_idx = ((ipa >> L3_SHIFT) & 0x1ff) as usize;
        let page_offset = ipa & (PAGE_SIZE - 1);

        let root_table = unsafe { &*(self.root_pa as *const TablePage) };
        let l1_entry = root_table.entries()[l1_idx];
        if l1_entry & (VALID | TABLE_OR_PAGE) != (VALID | TABLE_OR_PAGE) {
            return None;
        }
        let l2_pa = l1_entry & ADDRESS_MASK;

        let l2_table = unsafe { &*(l2_pa as *const TablePage) };
        let l2_entry = l2_table.entries()[l2_idx];
        if l2_entry & (VALID | TABLE_OR_PAGE) != (VALID | TABLE_OR_PAGE) {
            return None;
        }
        let l3_pa = l2_entry & ADDRESS_MASK;

        let l3_table = unsafe { &*(l3_pa as *const TablePage) };
        let l3_entry = l3_table.entries()[l3_idx];
        if l3_entry & (VALID | TABLE_OR_PAGE) != (VALID | TABLE_OR_PAGE) {
            return None;
        }

        let (mem_type, access, exec) = decode_l3_page_attributes(l3_entry);
        let base_pa = l3_entry & ADDRESS_MASK;

        Some(Stage2Translation {
            pa: base_pa | page_offset,
            mem_type,
            access,
            exec,
        })
    }
}

pub fn encode_l3_page_descriptor(
    pa: u64,
    mem_type: Stage2MemoryType,
    access: Stage2Access,
    exec: Stage2Exec,
) -> u64 {
    let mem_attr = match mem_type {
        Stage2MemoryType::NormalWbWa => MEM_ATTR_NORMAL_WB_WA,
        Stage2MemoryType::DeviceNgNre => MEM_ATTR_DEVICE_NGNRE,
    };
    let s2ap = match access {
        Stage2Access::None => S2AP_NONE,
        Stage2Access::ReadOnly => S2AP_READ_ONLY,
        Stage2Access::WriteOnly => S2AP_WRITE_ONLY,
        Stage2Access::ReadWrite => S2AP_READ_WRITE,
    };
    let sh = match mem_type {
        Stage2MemoryType::NormalWbWa => SH_INNER,
        Stage2MemoryType::DeviceNgNre => SH_NONE,
    };
    let xn = match exec {
        Stage2Exec::Executable => XN_EXEC,
        Stage2Exec::ExecuteNever => XN_NON_EXEC,
    };

    VALID | TABLE_OR_PAGE | mem_attr | s2ap | sh | ACCESS_FLAG | xn | (pa & ADDRESS_MASK)
}

fn decode_l3_page_attributes(desc: u64) -> (Stage2MemoryType, Stage2Access, Stage2Exec) {
    let mem_type = match (desc >> 2) & 0b1111 {
        0b0001 => Stage2MemoryType::DeviceNgNre,
        _ => Stage2MemoryType::NormalWbWa,
    };
    let access = match (desc >> 6) & 0b11 {
        0b00 => Stage2Access::None,
        0b01 => Stage2Access::ReadOnly,
        0b10 => Stage2Access::WriteOnly,
        _ => Stage2Access::ReadWrite,
    };
    let exec = match (desc >> 53) & 0b11 {
        0b00 => Stage2Exec::Executable,
        _ => Stage2Exec::ExecuteNever,
    };
    (mem_type, access, exec)
}

pub fn stage2_register_values(
    vmid: u8,
    root_table_pa: u64,
    parange: u8,
) -> Result<Stage2RegisterValues, TranslationError> {
    let pa_bits = pa_bits_from_parange(parange)?;
    let max_root_pa = (1_u64 << pa_bits) - 1;
    if root_table_pa & (PAGE_SIZE - 1) != 0 || root_table_pa > max_root_pa {
        return Err(TranslationError::InvalidTableBase);
    }

    let ps_field = ((parange & 0x7) as u64) << 16;
    let vtcr_el2 = VTCR_EL2_RES1
        | VTCR_EL2_TG0_4KB
        | VTCR_EL2_SH0_INNER
        | VTCR_EL2_ORGN0_NORMAL_WB_WA
        | VTCR_EL2_IRGN0_NORMAL_WB_WA
        | VTCR_EL2_SL0_LEVEL_1
        | ps_field
        | VTCR_EL2_T0SZ_39_BIT;

    let vttbr_el2 = ((vmid as u64) << 48) | (root_table_pa & ADDRESS_MASK);

    Ok(Stage2RegisterValues {
        vtcr_el2,
        vttbr_el2,
        hcr_el2: HCR_EL2_PHASE9_VALUE,
        cptr_el2: CPTR_EL2_PHASE9_VALUE,
        vmpidr_el2: VMPIDR_EL2_VCPU0,
    })
}

#[cfg(test)]
pub struct MockStage2Allocator<const N: usize> {
    pub pages: [TablePage; N],
    pub next_idx: usize,
}

#[cfg(test)]
impl<const N: usize> MockStage2Allocator<N> {
    pub const fn new() -> Self {
        Self {
            pages: [const { TablePage::zeroed() }; N],
            next_idx: 0,
        }
    }
}

#[cfg(test)]
unsafe impl<const N: usize> Stage2Allocator for &mut MockStage2Allocator<N> {
    fn allocate_table_page(&mut self) -> Result<u64, TranslationError> {
        if self.next_idx >= N {
            return Err(TranslationError::TableExhausted);
        }
        let page_ptr = core::ptr::addr_of_mut!(self.pages[self.next_idx]);
        self.next_idx += 1;
        unsafe {
            core::ptr::write_bytes(page_ptr as *mut u8, 0, PAGE_SIZE as usize);
        }
        Ok(page_ptr as u64)
    }
}

#[cfg(test)]
struct MisalignedAllocator;

#[cfg(test)]
unsafe impl Stage2Allocator for MisalignedAllocator {
    fn allocate_table_page(&mut self) -> Result<u64, TranslationError> {
        Ok(0x5000_0001) // Not 4 KiB aligned
    }
}

#[cfg(test)]
struct OutOfRangeAllocator;

#[cfg(test)]
unsafe impl Stage2Allocator for OutOfRangeAllocator {
    fn allocate_table_page(&mut self) -> Result<u64, TranslationError> {
        Ok(0x1_0000_0000_0000) // Exceeds 40-bit PA width
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_fdt::{GuestFdtConfig, GuestMemoryRegion, build_guest_dtb};
    use dtoolkit::Node;
    use dtoolkit::Property;
    use dtoolkit::fdt::Fdt;

    #[test]
    fn stage2_register_values_encodes_expected_fields() {
        let root_pa = 0x0500_0000;
        let regs = stage2_register_values(1, root_pa, 0b0010).expect("valid regs");

        assert_eq!(regs.vtcr_el2 & 0x3f, 25); // T0SZ = 25 (39 bits)
        assert_eq!((regs.vtcr_el2 >> 6) & 0x3, 0b01); // SL0 = Level 1
        assert_eq!((regs.vtcr_el2 >> 14) & 0x3, 0b00); // TG0 = 4 KiB
        assert_eq!((regs.vtcr_el2 >> 16) & 0x7, 0b0010); // PS = 40 bits
        assert_eq!((regs.vtcr_el2 >> 31) & 0x1, 1); // RES1

        assert_eq!((regs.vttbr_el2 >> 48) & 0xff, 1); // VMID = 1
        assert_eq!(regs.vttbr_el2 & ADDRESS_MASK, root_pa);

        assert_eq!(regs.hcr_el2 & HCR_EL2_VM, HCR_EL2_VM);
        assert_eq!(regs.hcr_el2 & HCR_EL2_RW, HCR_EL2_RW);
        assert_eq!(regs.cptr_el2 & CPTR_EL2_TFP, 0);
        assert_eq!(regs.vmpidr_el2, 0xC000_0000);
    }

    #[test]
    fn stage2_maps_and_walks_l3_leaf_pages_with_permissions() {
        let mut mock_alloc = MockStage2Allocator::<8>::new();
        let mut table_set =
            Stage2TableSet::new(&mut mock_alloc, 48).expect("create Stage2TableSet");

        // Map code: ReadOnly, Executable
        table_set
            .map(Stage2Mapping {
                ipa: 0x4000_0000,
                pa: 0x1000_0000,
                size: 0x1000,
                mem_type: Stage2MemoryType::NormalWbWa,
                access: Stage2Access::ReadOnly,
                exec: Stage2Exec::Executable,
            })
            .expect("map code page");

        // Map data: ReadWrite, ExecuteNever
        table_set
            .map(Stage2Mapping {
                ipa: 0x4000_1000,
                pa: 0x1000_1000,
                size: 0x1000,
                mem_type: Stage2MemoryType::NormalWbWa,
                access: Stage2Access::ReadWrite,
                exec: Stage2Exec::ExecuteNever,
            })
            .expect("map data page");

        // Check translation of code page
        let code_trans = table_set.translate(0x4000_0000).expect("translate code");
        assert_eq!(code_trans.pa, 0x1000_0000);
        assert_eq!(code_trans.access, Stage2Access::ReadOnly);
        assert_eq!(code_trans.exec, Stage2Exec::Executable);

        // Check translation of data page
        let data_trans = table_set.translate(0x4000_1000).expect("translate data");
        assert_eq!(data_trans.pa, 0x1000_1000);
        assert_eq!(data_trans.access, Stage2Access::ReadWrite);
        assert_eq!(data_trans.exec, Stage2Exec::ExecuteNever);

        // Check unmapped page (stack guard)
        assert!(table_set.translate(0x4000_2000).is_none());

        // Check completely unmapped range
        assert!(table_set.translate(0x3000_0000).is_none());
    }

    #[test]
    fn guest_dtb_and_stage2_mappings_agree_exactly() {
        let mut mock_alloc = MockStage2Allocator::<8>::new();
        let mut table_set =
            Stage2TableSet::new(&mut mock_alloc, 48).expect("create Stage2TableSet");

        let mem_regions = [
            GuestMemoryRegion {
                base: 0x4000_0000,
                size: 0x0000_2000, // covers payload (0x4000_0000) and scratch (0x4000_1000)
            },
            GuestMemoryRegion {
                base: 0x4000_3000,
                size: 0x0000_1000, // guest stack
            },
            GuestMemoryRegion {
                base: 0x4010_0000,
                size: 0x0000_1000, // guest DTB
            },
        ];

        // Map payload: ReadOnly, Executable
        table_set
            .map(Stage2Mapping {
                ipa: 0x4000_0000,
                pa: 0x1000_0000,
                size: 0x1000,
                mem_type: Stage2MemoryType::NormalWbWa,
                access: Stage2Access::ReadOnly,
                exec: Stage2Exec::Executable,
            })
            .unwrap();

        // Map scratch: ReadWrite, ExecuteNever
        table_set
            .map(Stage2Mapping {
                ipa: 0x4000_1000,
                pa: 0x1000_1000,
                size: 0x1000,
                mem_type: Stage2MemoryType::NormalWbWa,
                access: Stage2Access::ReadWrite,
                exec: Stage2Exec::ExecuteNever,
            })
            .unwrap();

        // Map stack: ReadWrite, ExecuteNever
        table_set
            .map(Stage2Mapping {
                ipa: 0x4000_3000,
                pa: 0x1000_3000,
                size: 0x1000,
                mem_type: Stage2MemoryType::NormalWbWa,
                access: Stage2Access::ReadWrite,
                exec: Stage2Exec::ExecuteNever,
            })
            .unwrap();

        // Map DTB: ReadOnly, ExecuteNever
        table_set
            .map(Stage2Mapping {
                ipa: 0x4010_0000,
                pa: 0x1010_0000,
                size: 0x1000,
                mem_type: Stage2MemoryType::NormalWbWa,
                access: Stage2Access::ReadOnly,
                exec: Stage2Exec::ExecuteNever,
            })
            .unwrap();

        // Build guest DTB describing these exact memory regions
        let mut dtb_buf = [0_u8; 1024];
        let dtb_size = build_guest_dtb(
            &mut dtb_buf,
            &GuestFdtConfig {
                memory_regions: &mem_regions,
                bootargs: None,
            },
        )
        .expect("build DTB");

        let fdt = Fdt::new(&dtb_buf[..dtb_size]).expect("parse DTB");
        let mem = fdt.root().child("memory@40000000").expect("/memory node");
        let reg = mem.property("reg").expect("reg property");
        let reg_bytes = reg.value();
        assert_eq!(reg_bytes.len(), mem_regions.len() * 16);

        // Verify every page advertised by the DTB has a valid Stage-2 translation
        for i in 0..mem_regions.len() {
            let base = u64::from_be_bytes(reg_bytes[i * 16..i * 16 + 8].try_into().unwrap());
            let size = u64::from_be_bytes(reg_bytes[i * 16 + 8..i * 16 + 16].try_into().unwrap());
            let mut page = base;
            while page < base + size {
                let trans = table_set
                    .translate(page)
                    .unwrap_or_else(|| panic!("Page {:#x} advertised in DTB is unmapped", page));
                assert_eq!(trans.mem_type, Stage2MemoryType::NormalWbWa);
                page += PAGE_SIZE;
            }
        }

        // Verify that stack guard page (0x4000_2000) is NOT advertised in DTB and is unmapped in Stage-2
        assert!(table_set.translate(0x4000_2000).is_none());
        // Verify that unmapped low device space is unmapped
        assert!(table_set.translate(0x3000_0000).is_none());
    }

    #[test]
    fn stage2_rejects_misaligned_and_out_of_range_table_allocator() {
        assert_eq!(
            Stage2TableSet::new(MisalignedAllocator, 40).map(|_| ()),
            Err(TranslationError::InvalidTableBase)
        );
        assert_eq!(
            Stage2TableSet::new(OutOfRangeAllocator, 40).map(|_| ()),
            Err(TranslationError::InvalidTableBase)
        );
    }

    #[test]
    fn stage2_encodes_device_and_readonly_attributes() {
        let desc = encode_l3_page_descriptor(
            0x2000_0000,
            Stage2MemoryType::DeviceNgNre,
            Stage2Access::ReadOnly,
            Stage2Exec::ExecuteNever,
        );
        assert_eq!(desc & VALID, VALID);
        assert_eq!(desc & TABLE_OR_PAGE, TABLE_OR_PAGE);
        assert_eq!(desc & (0b1111 << 2), MEM_ATTR_DEVICE_NGNRE);
        assert_eq!(desc & (0b11 << 6), S2AP_READ_ONLY);
        assert_eq!(desc & (0b11 << 8), SH_NONE);
        assert_eq!(desc & ACCESS_FLAG, ACCESS_FLAG);
        assert_eq!(desc & (0b11 << 53), XN_NON_EXEC);
        assert_eq!(desc & ADDRESS_MASK, 0x2000_0000);
    }

    #[test]
    fn stage2_rejects_unaligned_and_overflowing_mappings() {
        let mut mock_alloc = MockStage2Allocator::<8>::new();
        let mut table_set =
            Stage2TableSet::new(&mut mock_alloc, 48).expect("create Stage2TableSet");

        let unaligned = Stage2Mapping {
            ipa: 0x4000_0001,
            pa: 0x1000_0000,
            size: 0x1000,
            mem_type: Stage2MemoryType::NormalWbWa,
            access: Stage2Access::ReadWrite,
            exec: Stage2Exec::Executable,
        };
        assert_eq!(
            table_set.map(unaligned),
            Err(TranslationError::UnalignedMapping)
        );

        let overflow = Stage2Mapping {
            ipa: (1 << IPA_BITS) - 0x800,
            pa: 0x1000_0000,
            size: 0x1000,
            mem_type: Stage2MemoryType::NormalWbWa,
            access: Stage2Access::ReadWrite,
            exec: Stage2Exec::Executable,
        };
        assert_eq!(
            table_set.map(overflow),
            Err(TranslationError::UnalignedMapping)
        );
    }

    #[test]
    fn stage2_handles_table_exhaustion_cleanly() {
        let mut mock_alloc = MockStage2Allocator::<1>::new(); // Only root table can be allocated
        let mut table_set =
            Stage2TableSet::new(&mut mock_alloc, 48).expect("create Stage2TableSet");

        // Attempting to map requires L2 and L3 subtables, which must fail cleanly
        let res = table_set.map(Stage2Mapping {
            ipa: 0x4000_0000,
            pa: 0x1000_0000,
            size: 0x1000,
            mem_type: Stage2MemoryType::NormalWbWa,
            access: Stage2Access::ReadWrite,
            exec: Stage2Exec::Executable,
        });
        assert_eq!(res, Err(TranslationError::TableExhausted));
    }

    #[test]
    fn allocation_failure_injection_restores_clean_state_on_rollback() {
        struct FailingTrackingAllocator {
            pages: [TablePage; 16],
            in_use: [bool; 16],
            fail_at: usize,
            alloc_count: usize,
            recorded: [usize; 16],
            recorded_count: usize,
        }

        impl FailingTrackingAllocator {
            fn new(fail_at: usize) -> Self {
                Self {
                    pages: [const { TablePage::zeroed() }; 16],
                    in_use: [false; 16],
                    fail_at,
                    alloc_count: 0,
                    recorded: [0; 16],
                    recorded_count: 0,
                }
            }

            fn in_use_count(&self) -> usize {
                self.in_use.iter().filter(|&&used| used).count()
            }

            fn rollback(&mut self) {
                for &idx in &self.recorded[..self.recorded_count] {
                    self.in_use[idx] = false;
                }
                self.recorded_count = 0;
            }
        }

        unsafe impl Stage2Allocator for &mut FailingTrackingAllocator {
            fn allocate_table_page(&mut self) -> Result<u64, TranslationError> {
                if self.alloc_count == self.fail_at {
                    return Err(TranslationError::TableExhausted);
                }
                let idx = self.alloc_count;
                self.alloc_count += 1;
                self.in_use[idx] = true;
                self.recorded[self.recorded_count] = idx;
                self.recorded_count += 1;
                let page_ptr = core::ptr::addr_of_mut!(self.pages[idx]);
                Ok(page_ptr as u64)
            }
        }

        // Mapping 1 page requires 3 table allocations: Root (L1), L2 subtable, L3 subtable.
        // Test failure at each of these allocation steps (0, 1, 2)
        for fail_point in 0..3 {
            let mut tracking = FailingTrackingAllocator::new(fail_point);
            assert_eq!(tracking.in_use_count(), 0);

            let res = (|| -> Result<(), TranslationError> {
                let mut table_set = Stage2TableSet::new(&mut tracking, 48)?;
                table_set.map(Stage2Mapping {
                    ipa: 0x4000_0000,
                    pa: 0x1000_0000,
                    size: 0x1000,
                    mem_type: Stage2MemoryType::NormalWbWa,
                    access: Stage2Access::ReadOnly,
                    exec: Stage2Exec::Executable,
                })?;
                Ok(())
            })();

            assert!(res.is_err());
            // Rollback should return all acquired pages
            tracking.rollback();
            assert_eq!(
                tracking.in_use_count(),
                0,
                "Injected failure at step {} must restore 0 in-use pages",
                fail_point
            );
        }
    }

    #[test]
    fn rollback_preserves_unfreed_pages_on_injected_free_failure_and_allows_retry() {
        #[derive(Debug, PartialEq, Eq)]
        enum MockFreeError {
            InjectedFailure,
        }

        struct MockResourceManager {
            allocated_pages: [u64; 8],
            allocated_count: usize,
            fail_free_pa: Option<u64>,
        }

        impl MockResourceManager {
            fn new() -> Self {
                Self {
                    allocated_pages: [0; 8],
                    allocated_count: 0,
                    fail_free_pa: None,
                }
            }

            fn allocate(&mut self, pa: u64) {
                self.allocated_pages[self.allocated_count] = pa;
                self.allocated_count += 1;
            }

            fn rollback(&mut self) -> Result<(), MockFreeError> {
                while self.allocated_count > 0 {
                    let last_idx = self.allocated_count - 1;
                    let pa = self.allocated_pages[last_idx];
                    if Some(pa) == self.fail_free_pa {
                        return Err(MockFreeError::InjectedFailure);
                    }
                    self.allocated_pages[last_idx] = 0;
                    self.allocated_count -= 1;
                }
                Ok(())
            }
        }

        let mut mgr = MockResourceManager::new();
        mgr.allocate(0x1000);
        mgr.allocate(0x2000);
        mgr.allocate(0x3000);
        mgr.allocate(0x4000);
        assert_eq!(mgr.allocated_count, 4);

        // Inject failure when attempting to free page 0x2000 (3rd in reverse order)
        mgr.fail_free_pa = Some(0x2000);

        // First rollback attempt should free 0x4000 and 0x3000, then fail on 0x2000
        let res = mgr.rollback();
        assert_eq!(res, Err(MockFreeError::InjectedFailure));
        // Remaining un-freed pages must still be tracked!
        assert_eq!(mgr.allocated_count, 2);
        assert_eq!(&mgr.allocated_pages[..2], &[0x1000, 0x2000]);

        // Clear failure injection and retry rollback
        mgr.fail_free_pa = None;
        let retry_res = mgr.rollback();
        assert!(retry_res.is_ok());
        assert_eq!(mgr.allocated_count, 0);
    }
}
