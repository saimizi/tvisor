//! EL2 stage-1 translation tables used by tvisor's non-VHE (`E2H=0`) regime.
//!
//! Phase 7 uses a 39-bit virtual address, a 4 KiB granule, and an L1 start:
//!
//! ```text
//! VA[38:30]   VA[29:21]   VA[20:12]   VA[11:0]
//!  L1 index    L2 index    L3 index    page offset
//!   9 bits      9 bits      9 bits       12 bits
//! ```
//!
//! A table page contains 512 64-bit entries. Descriptor bits `[1:0]` select
//! the entry type:
//!
//! ```text
//! Level   00          01              11
//! L1      invalid     1 GiB block     next L2 table
//! L2      invalid     2 MiB block     next L3 table
//! L3      invalid     reserved         4 KiB page
//! ```
//!
//! Thus, L1 and L2 do not always point to another table: either can terminate
//! a walk with a suitably aligned block mapping. The builder chooses the
//! largest aligned leaf that does not cross a mapping boundary.
//!
//! This implementation encodes descriptor addresses in bits `[47:12]`, for a
//! maximum 48-bit PA representation. The processor can implement fewer PA
//! bits; `ID_AA64MMFR0_EL1.PARange` supplies the actual limit and `TCR_EL2.PS`
//! records it. The tested Raspberry Pi 4 reports `PARange=4`, or 44 PA bits.
//! The address portion used by each descriptor is:
//!
//! ```text
//! L1 block: bits [47:30], followed by VA[29:0]  (1 GiB)
//! L2 block: bits [47:21], followed by VA[20:0]  (2 MiB)
//! L3 page:  bits [47:12], followed by VA[11:0]  (4 KiB)
//! L1/L2 table: bits [47:12], the 4 KiB-aligned next-table PA
//! ```
//!
//! Leaf descriptors use this architectural subset:
//!
//! ```text
//! Bit(s)   Field      Policy
//! 0        Valid      1 for every populated descriptor
//! 1        Type       0 for an L1/L2 block; 1 for a table or L3 page
//! [4:2]    AttrIdx    0 = Normal WB/WA; 1 = Device-nGnRE
//! [7:6]    AP         bit 7 selects EL2 read-only; otherwise read/write
//! [9:8]    SH         Inner Shareable for Normal; Non-shareable for Device
//! 10       AF         set so the first access does not take an AF fault
//! 54       XN         set for non-executable mappings
//! ```
//!
//! In this non-VHE EL2 translation regime, bit 54 is the effective `XN` bit.
//! It is clear only for tvisor text and vectors. It must not be confused with
//! the EL0/EL1 regime's `PXN` interpretation. Fields not intentionally used,
//! including table-level permission restrictions, remain zero. A table
//! descriptor therefore contains only its next-table PA, `Valid`, and `Type`.

use core::fmt;

pub const PAGE_SIZE: u64 = 4096;
pub const ENTRIES_PER_TABLE: usize = 512;
pub const VA_BITS: u8 = 39;

const L1_SHIFT: u32 = 30;
const L2_SHIFT: u32 = 21;
const L3_SHIFT: u32 = 12;
const L1_SIZE: u64 = 1 << L1_SHIFT;
const L2_SIZE: u64 = 1 << L2_SHIFT;
const ADDRESS_MASK: u64 = 0x0000_ffff_ffff_f000;

const VALID: u64 = 1 << 0;
const TABLE_OR_PAGE: u64 = 1 << 1;
const ATTR_INDEX_SHIFT: u32 = 2;
const AP_READ_ONLY: u64 = 1 << 7;
const SH_INNER: u64 = 0b11 << 8;
const ACCESS_FLAG: u64 = 1 << 10;
// In an EL2 stage-1 leaf descriptor bit[54] is XN. Bit[53] is PXN in the
// EL0/EL1 regime, but is not the execute permission used by this regime.
const XN: u64 = 1 << 54;

pub const MAIR_NORMAL_WB_WA: u8 = 0xff;
pub const MAIR_DEVICE_NGNRE: u8 = 0x04;
const MAIR_INDEX_NORMAL_WB_WA: u32 = 0;
const MAIR_INDEX_DEVICE_NGNRE: u32 = 1;
pub const MAIR_EL2_VALUE: u64 = ((MAIR_NORMAL_WB_WA as u64) << (MAIR_INDEX_NORMAL_WB_WA * 8))
    | ((MAIR_DEVICE_NGNRE as u64) << (MAIR_INDEX_DEVICE_NGNRE * 8));
pub const TCR_EL2_T0SZ_39_BIT: u64 = 25;
pub const TCR_EL2_RES1: u64 = (1 << 31) | (1 << 23);
pub const SCTLR_EL2_RES1: u64 =
    (0b11 << 28) | (0b11 << 22) | (1 << 18) | (1 << 16) | (1 << 11) | (0b11 << 4);
pub const SCTLR_EL2_VALUE: u64 =
    SCTLR_EL2_RES1 | (1 << 0) | (1 << 2) | (1 << 3) | (1 << 12) | (1 << 19);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct El2RegisterValues {
    pub mair_el2: u64,
    pub tcr_el2: u64,
    pub ttbr0_el2: u64,
    pub sctlr_el2: u64,
}

pub fn pa_bits_from_parange(parange: u8) -> Result<u8, TranslationError> {
    match parange {
        0 => Ok(32),
        1 => Ok(36),
        2 => Ok(40),
        3 => Ok(42),
        4 => Ok(44),
        5 => Ok(48),
        _ => Err(TranslationError::InvalidPaBits),
    }
}

pub fn register_values(root_pa: u64, parange: u8) -> Result<El2RegisterValues, TranslationError> {
    let pa_bits = pa_bits_from_parange(parange)?;
    if root_pa & (PAGE_SIZE - 1) != 0 || root_pa >= (1_u64 << pa_bits) {
        return Err(TranslationError::InvalidTableBase);
    }
    let tcr_el2 = TCR_EL2_RES1
        | TCR_EL2_T0SZ_39_BIT
        | (0b01 << 8)
        | (0b01 << 10)
        | (0b11 << 12)
        | ((parange as u64) << 16);
    Ok(El2RegisterValues {
        mair_el2: MAIR_EL2_VALUE,
        tcr_el2,
        ttbr0_el2: root_pa,
        sctlr_el2: SCTLR_EL2_VALUE,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryType {
    Normal,
    Device,
}

impl MemoryType {
    const fn attr_index(self) -> u64 {
        match self {
            Self::Normal => MAIR_INDEX_NORMAL_WB_WA as u64,
            Self::Device => MAIR_INDEX_DEVICE_NGNRE as u64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mapping {
    pub va: u64,
    pub pa: u64,
    pub size: u64,
    pub memory_type: MemoryType,
    pub writable: bool,
    pub executable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationError {
    InvalidTableBase,
    InvalidPaBits,
    EmptyMapping,
    UnalignedMapping,
    AddressOverflow,
    VirtualAddressOutOfRange,
    PhysicalAddressOutOfRange,
    TableExhausted,
    ConflictingEntry,
    CorruptTable,
}

impl fmt::Display for TranslationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Translation {
    pub pa: u64,
    pub memory_type: MemoryType,
    pub writable: bool,
    pub executable: bool,
    pub level: u8,
}

#[repr(C, align(4096))]
#[derive(Clone)]
pub struct TablePage {
    entries: [u64; ENTRIES_PER_TABLE],
}

impl TablePage {
    pub const fn zeroed() -> Self {
        Self {
            entries: [0; ENTRIES_PER_TABLE],
        }
    }

    pub const fn entries(&self) -> &[u64; ENTRIES_PER_TABLE] {
        &self.entries
    }

    pub fn entries_mut(&mut self) -> &mut [u64; ENTRIES_PER_TABLE] {
        &mut self.entries
    }
}

#[repr(C, align(4096))]
pub struct TableStorage<const N: usize> {
    pages: [TablePage; N],
}

impl<const N: usize> TableStorage<N> {
    pub const fn zeroed() -> Self {
        Self {
            pages: [const { TablePage::zeroed() }; N],
        }
    }

    pub const fn pages(&self) -> &[TablePage; N] {
        &self.pages
    }

    pub fn pages_mut(&mut self) -> &mut [TablePage; N] {
        &mut self.pages
    }
}

pub struct TableSet<'a, const N: usize> {
    storage: &'a mut TableStorage<N>,
    used: usize,
    base_pa: u64,
    pa_bits: u8,
}

impl<'a, const N: usize> TableSet<'a, N> {
    pub fn new(
        storage: &'a mut TableStorage<N>,
        base_pa: u64,
        pa_bits: u8,
    ) -> Result<Self, TranslationError> {
        // Check `base_pa` is PAGE_SIZE aligned.
        if base_pa & (PAGE_SIZE - 1) != 0 {
            return Err(TranslationError::InvalidTableBase);
        }

        if !(32..=48).contains(&pa_bits) {
            return Err(TranslationError::InvalidPaBits);
        }

        // Page numbers included in the page table
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

        // Allocat L1 root table page. It is the first page of the page table.
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
        self.storage.pages.get(index).filter(|_| index < self.used)
    }

    pub fn map(&mut self, mapping: Mapping) -> Result<(), TranslationError> {
        self.validate_mapping(mapping)?;
        let mut va = mapping.va;
        let mut pa = mapping.pa;
        let mut remaining = mapping.size;
        while remaining != 0 {
            let (level, chunk) = if aligned_for(va, pa, L1_SIZE) && remaining >= L1_SIZE {
                (1, L1_SIZE)
            } else if aligned_for(va, pa, L2_SIZE) && remaining >= L2_SIZE {
                (2, L2_SIZE)
            } else {
                (3, PAGE_SIZE)
            };
            self.map_leaf(level, va, pa, mapping)?;
            va += chunk;
            pa += chunk;
            remaining -= chunk;
        }
        Ok(())
    }

    pub fn walk(&self, va: u64) -> Result<Option<Translation>, TranslationError> {
        if va >= (1_u64 << VA_BITS) {
            return Err(TranslationError::VirtualAddressOutOfRange);
        }
        let mut table = 0;
        for level in 1..=3 {
            let index = index_for(level, va);
            let descriptor = self.storage.pages[table].entries[index];
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
            let pa = base | (va & (block_size - 1));
            let attr_index = (descriptor >> ATTR_INDEX_SHIFT) & 0b111;
            let memory_type = match attr_index {
                0 => MemoryType::Normal,
                1 => MemoryType::Device,
                _ => return Err(TranslationError::CorruptTable),
            };
            return Ok(Some(Translation {
                pa,
                memory_type,
                writable: descriptor & AP_READ_ONLY == 0,
                executable: descriptor & XN == 0,
                level,
            }));
        }
        Err(TranslationError::CorruptTable)
    }

    fn validate_mapping(&self, mapping: Mapping) -> Result<(), TranslationError> {
        if mapping.size == 0 {
            return Err(TranslationError::EmptyMapping);
        }
        if (mapping.va | mapping.pa | mapping.size) & (PAGE_SIZE - 1) != 0 {
            return Err(TranslationError::UnalignedMapping);
        }
        let va_end = mapping
            .va
            .checked_add(mapping.size)
            .ok_or(TranslationError::AddressOverflow)?;
        let pa_end = mapping
            .pa
            .checked_add(mapping.size)
            .ok_or(TranslationError::AddressOverflow)?;
        if va_end > (1_u64 << VA_BITS) {
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
        va: u64,
        pa: u64,
        mapping: Mapping,
    ) -> Result<(), TranslationError> {
        // L1 root is 0;
        let mut table = 0;
        for current_level in 1..level {
            let index = index_for(current_level, va);
            let descriptor = self.storage.pages[table].entries[index];
            table = if descriptor == 0 {
                let child = self.allocate_page()?;
                self.storage.pages[table].entries[index] =
                    self.table_pa(child) | VALID | TABLE_OR_PAGE;
                child
            } else if descriptor & (VALID | TABLE_OR_PAGE) == (VALID | TABLE_OR_PAGE) {
                self.table_index(descriptor & ADDRESS_MASK)?
            } else {
                return Err(TranslationError::ConflictingEntry);
            };
        }
        let index = index_for(level, va);
        let descriptor = leaf_descriptor(level, pa, mapping);
        let entry = &mut self.storage.pages[table].entries[index];
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
        self.storage.pages[index].entries.fill(0);
        self.used += 1;
        Ok(index)
    }

    fn table_pa(&self, index: usize) -> u64 {
        self.base_pa + index as u64 * PAGE_SIZE
    }

    fn table_index(&self, pa: u64) -> Result<usize, TranslationError> {
        let offset = pa
            .checked_sub(self.base_pa)
            .ok_or(TranslationError::CorruptTable)?;
        if offset & (PAGE_SIZE - 1) != 0 {
            return Err(TranslationError::CorruptTable);
        }
        let index =
            usize::try_from(offset / PAGE_SIZE).map_err(|_| TranslationError::CorruptTable)?;
        if index >= self.used {
            return Err(TranslationError::CorruptTable);
        }
        Ok(index)
    }
}

fn aligned_for(va: u64, pa: u64, size: u64) -> bool {
    (va | pa) & (size - 1) == 0
}

fn index_for(level: u8, va: u64) -> usize {
    let shift = match level {
        1 => L1_SHIFT,
        2 => L2_SHIFT,
        3 => L3_SHIFT,
        _ => unreachable!(),
    };
    ((va >> shift) & 0x1ff) as usize
}

fn level_size(level: u8) -> u64 {
    match level {
        1 => L1_SIZE,
        2 => L2_SIZE,
        3 => PAGE_SIZE,
        _ => unreachable!(),
    }
}

fn output_mask(level: u8) -> u64 {
    ADDRESS_MASK & !(level_size(level) - 1)
}

fn leaf_descriptor(level: u8, pa: u64, mapping: Mapping) -> u64 {
    // If AF=0, the first access may generate an Access Flag Fault, so setting ACCESS_FLAG here to
    // prevent it.
    let mut descriptor = (pa & output_mask(level))
        | VALID
        | ACCESS_FLAG
        | (mapping.memory_type.attr_index() << ATTR_INDEX_SHIFT);

    if level == 3 {
        descriptor |= TABLE_OR_PAGE;
    }

    if mapping.memory_type == MemoryType::Normal {
        descriptor |= SH_INNER;
    }

    if !mapping.writable {
        descriptor |= AP_READ_ONLY;
    }

    if !mapping.executable {
        descriptor |= XN;
    }

    descriptor
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(va: u64, pa: u64, size: u64) -> Mapping {
        Mapping {
            va,
            pa,
            size,
            memory_type: MemoryType::Normal,
            writable: true,
            executable: false,
        }
    }

    #[test]
    fn maps_and_walks_each_leaf_level() {
        let mut storage = TableStorage::<8>::zeroed();
        let mut tables = TableSet::new(&mut storage, 0x3000_0000, 44).unwrap();
        tables.map(mapping(0, 0, L1_SIZE)).unwrap();
        tables
            .map(mapping(0x4000_0000, 0x4000_0000, L2_SIZE))
            .unwrap();
        tables
            .map(mapping(0x5000_1000, 0x6000_1000, PAGE_SIZE))
            .unwrap();

        assert_eq!(tables.walk(0x1234).unwrap().unwrap().level, 1);
        assert_eq!(
            tables.walk(0x401f_ffff).unwrap().unwrap(),
            Translation {
                pa: 0x401f_ffff,
                memory_type: MemoryType::Normal,
                writable: true,
                executable: false,
                level: 2,
            }
        );
        assert_eq!(tables.walk(0x5000_1234).unwrap().unwrap().pa, 0x6000_1234);
    }

    #[test]
    fn preserves_permissions_and_memory_type() {
        let mut storage = TableStorage::<4>::zeroed();
        let mut tables = TableSet::new(&mut storage, 0x3000_0000, 44).unwrap();
        tables
            .map(Mapping {
                va: 0xfe21_5000,
                pa: 0xfe21_5000,
                size: PAGE_SIZE,
                memory_type: MemoryType::Device,
                writable: true,
                executable: false,
            })
            .unwrap();
        let uart = tables.walk(0xfe21_5040).unwrap().unwrap();
        assert_eq!(uart.memory_type, MemoryType::Device);
        assert!(uart.writable);
        assert!(!uart.executable);
    }

    #[test]
    fn encodes_el2_xn_in_bit_54() {
        let executable = Mapping {
            executable: true,
            ..mapping(0x400_1000, 0x400_1000, PAGE_SIZE)
        };
        let non_executable = mapping(0x400_2000, 0x400_2000, PAGE_SIZE);
        let executable_descriptor = leaf_descriptor(3, executable.pa, executable);
        let non_executable_descriptor = leaf_descriptor(3, non_executable.pa, non_executable);

        assert_eq!(executable_descriptor & (1 << 54), 0);
        assert_ne!(non_executable_descriptor & (1 << 54), 0);
        assert_eq!(executable_descriptor & (1 << 53), 0);
        assert_eq!(non_executable_descriptor & (1 << 53), 0);
    }

    #[test]
    fn leaves_guard_unmapped_and_rejects_conflicts() {
        let mut storage = TableStorage::<4>::zeroed();
        let mut tables = TableSet::new(&mut storage, 0x3000_0000, 44).unwrap();
        tables
            .map(mapping(0x405a_000, 0x405a_000, PAGE_SIZE))
            .unwrap();
        assert_eq!(tables.walk(0x4059_000).unwrap(), None);
        assert_eq!(
            tables.map(mapping(0x405a_000, 0x505a_000, PAGE_SIZE)),
            Err(TranslationError::ConflictingEntry)
        );
    }

    #[test]
    fn rejects_invalid_ranges_and_table_exhaustion() {
        let mut one_storage = TableStorage::<1>::zeroed();
        let mut one = TableSet::new(&mut one_storage, 0x3000_0000, 44).unwrap();
        assert_eq!(
            one.map(mapping(0x1000, 0x1000, PAGE_SIZE)),
            Err(TranslationError::TableExhausted)
        );
        let mut storage = TableStorage::<4>::zeroed();
        let mut tables = TableSet::new(&mut storage, 0x3000_0000, 36).unwrap();
        assert_eq!(
            tables.map(mapping(1 << VA_BITS, 0, PAGE_SIZE)),
            Err(TranslationError::VirtualAddressOutOfRange)
        );
        assert_eq!(
            tables.map(mapping(0, 1 << 36, PAGE_SIZE)),
            Err(TranslationError::PhysicalAddressOutOfRange)
        );
        assert_eq!(
            tables.map(mapping(1, 0, PAGE_SIZE)),
            Err(TranslationError::UnalignedMapping)
        );
    }

    #[test]
    fn encodes_expected_mair_attributes() {
        assert_eq!(MAIR_EL2_VALUE, 0x04ff);
    }

    #[test]
    fn constructs_phase7_register_values() {
        let registers = register_values(0x3000_0000, 4).unwrap();
        assert_eq!(registers.mair_el2, 0x04ff);
        assert_eq!(registers.ttbr0_el2, 0x3000_0000);
        assert_eq!(registers.tcr_el2 & 0x3f, 25);
        assert_eq!((registers.tcr_el2 >> 8) & 0b11, 0b01);
        assert_eq!((registers.tcr_el2 >> 10) & 0b11, 0b01);
        assert_eq!((registers.tcr_el2 >> 12) & 0b11, 0b11);
        assert_eq!((registers.tcr_el2 >> 14) & 0b11, 0b00);
        assert_eq!((registers.tcr_el2 >> 16) & 0b111, 4);
        assert_eq!(registers.tcr_el2 & TCR_EL2_RES1, TCR_EL2_RES1);
        assert_ne!(registers.sctlr_el2 & (1 << 0), 0);
        assert_ne!(registers.sctlr_el2 & (1 << 19), 0);
    }
}
