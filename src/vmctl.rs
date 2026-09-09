use crate::alloc::string::ToString;
use crate::mm;
use alloc::vec::Vec;
use tvisor_util::aarch64_reg::*;
use tvisor_util::el2_translation::*;
use tvisor_util::page_allocator::AllocatorError;
use tvisor_util::stage2_translation::*;
use tvisor_util::*;

use core::fmt::Display;
use tvisor_util::system_info::{PhysAddr, PhysRegion};

pub const MAX_GUEST_MEM_BYTES: usize = 1024 * 1024;
pub const MAX_GUEST_MEM_PAGES: usize = (MAX_GUEST_MEM_BYTES) / PAGE_SIZE;

pub type AddressType = PhysAddr;
pub type RegionType = PhysRegion;

/// Guest intermediate physical address (IPA).
///
/// This is deliberately distinct from [`PhysAddr`], which names a host
/// physical address, so callers cannot accidentally mix address spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct IpaAddr(u64);

impl IpaAddr {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    const fn checked_add(self, offset: u64) -> Option<Self> {
        match self.0.checked_add(offset) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

impl Display for IpaAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "0x{:016x}", self.0)
    }
}

/// A contiguous guest IPA range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpaRegion {
    start: IpaAddr,
    size: u64,
}

impl IpaRegion {
    fn new(start: IpaAddr, size: u64) -> Option<Self> {
        if size == 0 || start.checked_add(size).is_none() {
            return None;
        }

        Some(Self { start, size })
    }

    pub const fn start(self) -> IpaAddr {
        self.start
    }

    #[allow(unused)]
    pub const fn size(self) -> u64 {
        self.size
    }

    #[allow(unused)]
    pub const fn end(self) -> IpaAddr {
        // Construction proves this addition cannot overflow.
        IpaAddr(self.start.0 + self.size)
    }
}

#[derive(PartialEq, PartialOrd, Clone, Copy, Debug)]
pub enum VmMemUsage {
    Image,
    Scratch,
    Stack,
    Dtb,
    Guard,
    PageTable,
}

impl VmMemUsage {
    pub fn need_physical_memory(&self) -> bool {
        match self {
            VmMemUsage::Image => true,
            VmMemUsage::Dtb => true,
            VmMemUsage::Stack => true,
            VmMemUsage::Guard => false,
            VmMemUsage::Scratch => true,
            VmMemUsage::PageTable => true,
        }
    }
}

impl Display for VmMemUsage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            VmMemUsage::Image => "Image",
            VmMemUsage::Dtb => "DTB",
            VmMemUsage::Stack => "Stack",
            VmMemUsage::Guard => "Guard",
            VmMemUsage::Scratch => "Scratch",
            VmMemUsage::PageTable => "PageTable",
        };

        write!(f, "{}", msg)
    }
}

#[derive(PartialEq, PartialOrd)]
struct IpaOnlyRegion {
    usage: VmMemUsage,
    base: IpaAddr,
    size: usize,
}

#[allow(unused)]
impl IpaOnlyRegion {
    fn end(&self) -> Option<IpaAddr> {
        self.base.checked_add(self.size as u64)
    }
}

impl Display for IpaOnlyRegion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "ipa:{} size:{} usage:{}",
            self.base, self.size, self.usage
        )
    }
}

#[derive(PartialEq, PartialOrd)]
struct IpaPaRegion {
    usage: VmMemUsage,
    ipa: IpaAddr,
    pa: AddressType,
    size: usize,
}

impl IpaPaRegion {
    pub fn pa_end(&self) -> Option<AddressType> {
        let start: u64 = self.pa.into();
        start.checked_add(self.size as u64).map(AddressType::new)
    }

    pub fn ipa_end(&self) -> Option<IpaAddr> {
        self.ipa.checked_add(self.size as u64)
    }
}

impl Display for IpaPaRegion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "ipa:{} pa:{} size:{} usage:{}",
            self.ipa, self.pa, self.size, self.usage
        )
    }
}

#[derive(PartialEq, PartialOrd)]
struct PaRegion {
    usage: VmMemUsage,
    pa: Vec<AddressType>,
}

impl Display for PaRegion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(
            f,
            "usage: {} size:{}",
            self.usage,
            self.pa.len() * PAGE_SIZE
        )?;
        for p in &self.pa {
            writeln!(f, "pa: {}", p)?;
        }

        Ok(())
    }
}

#[derive(PartialEq, PartialOrd)]
enum VmMem {
    IpaOnly(IpaOnlyRegion),
    IpaPa(IpaPaRegion),
    PaOnly(PaRegion),
}

/// Iterates over the physical regions backing one VM-memory entry.
///
/// IpaPa entries are contiguous and yield one region. Page-table entries can
/// grow one page at a time, so they yield one region for each tracked page.
struct EntryPaIter<'a> {
    entry: Option<&'a VmMem>,
    next_page: usize,
}

impl Iterator for EntryPaIter<'_> {
    type Item = RegionType;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.entry? {
                VmMem::IpaPa(region) => {
                    if self.next_page != 0 {
                        return None;
                    }
                    self.next_page = 1;
                    return RegionType::new(region.pa, region.size as u64).ok();
                }
                VmMem::PaOnly(region) => {
                    let pa = *region.pa.get(self.next_page)?;
                    self.next_page += 1;
                    if let Ok(region) = RegionType::new(pa, PAGE_SIZE as u64) {
                        return Some(region);
                    }
                }
                VmMem::IpaOnly(_) => return None,
            }
        }
    }
}

/// Iterates through all IPA-backed entries in ascending IPA order without
/// allocating a temporary collection. The number of entries is small, so a
/// linear scan for each result is preferable to heap allocation.
struct IpaRegionsIter<'a> {
    entries: &'a [VmMem],
    previous_start: Option<u64>,
}

impl Iterator for IpaRegionsIter<'_> {
    type Item = IpaRegion;

    fn next(&mut self) -> Option<Self::Item> {
        let mut next = None;

        for entry in self.entries {
            let Some(region) = (match entry {
                VmMem::IpaPa(region) => IpaRegion::new(region.ipa, region.size as u64),
                VmMem::IpaOnly(region) => IpaRegion::new(region.base, region.size as u64),
                VmMem::PaOnly(_) => None,
            }) else {
                continue;
            };

            let start = region.start().value();
            if self.previous_start.is_none_or(|previous| start > previous)
                && next.is_none_or(|candidate: IpaRegion| start < candidate.start().value())
            {
                next = Some(region);
            }
        }

        if let Some(region) = next {
            self.previous_start = Some(region.start().value());
            Some(region)
        } else {
            None
        }
    }
}

impl VmMem {
    pub fn usage(&self) -> VmMemUsage {
        match self {
            VmMem::IpaPa(v) => v.usage,
            VmMem::PaOnly(v) => v.usage,
            VmMem::IpaOnly(v) => v.usage,
        }
    }

    fn physical_pages(&self) -> usize {
        match self {
            Self::IpaOnly(_) => 0,
            Self::IpaPa(region) => region.size / PAGE_SIZE,
            Self::PaOnly(region) => region.pa.len(),
        }
    }

    fn ipa_range(&self) -> Option<(u64, u64)> {
        match self {
            Self::IpaPa(region) => Some((region.ipa.value(), region.ipa_end()?.value())),
            Self::IpaOnly(region) => Some((region.base.value(), region.end()?.value())),
            Self::PaOnly(_) => None,
        }
    }
}

impl Display for VmMem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            VmMem::IpaPa(v) => v.to_string(),
            VmMem::IpaOnly(v) => v.to_string(),
            VmMem::PaOnly(v) => v.to_string(),
        };

        write!(f, "{}", msg)
    }
}

pub struct VmCtl {
    vm_id: u8,
    vm_mem: Vec<VmMem>,
}

impl VmCtl {
    pub fn page_table_root(&self) -> Option<AddressType> {
        if let Some(VmMem::PaOnly(pa)) = self
            .vm_mem
            .iter()
            .find(|&a| a.usage() == VmMemUsage::PageTable)
        {
            Some(pa.pa[0])
        } else {
            None
        }
    }

    pub fn map(
        &mut self,
        usage: VmMemUsage,
        mem_type: Stage2MemoryType,
        access: Stage2Access,
        exec: Stage2Exec,
    ) -> Result<(), TranslationError> {
        let Some(VmMem::IpaPa(mapping)) = self.vm_mem.iter().find(|f| f.usage() == usage) else {
            return Err(TranslationError::InvalidateParameter);
        };

        let pa_bits = IdAa64Mmfr0El1::dump()
            .ok_or(TranslationError::Unexpected)?
            .pa_bits()
            .ok_or(TranslationError::Unexpected)?;
        let max_ipa = (1_u64 << IPA_BITS) - 1;
        let max_pa = (1_u64 << pa_bits) - 1;
        let ipa_end = mapping
            .ipa_end()
            .ok_or(TranslationError::AddressOverflow)?
            .value();
        let pa_end = mapping
            .pa_end()
            .ok_or(TranslationError::AddressOverflow)?
            .value();

        if ipa_end - 1 > max_ipa {
            return Err(TranslationError::VirtualAddressOutOfRange);
        }
        if pa_end - 1 > max_pa {
            return Err(TranslationError::PhysicalAddressOutOfRange);
        }

        let mut current_ipa = mapping.ipa.value();
        let mut current_pa = mapping.pa.value();

        while current_ipa < ipa_end {
            self.map_l3_page(current_ipa, current_pa, mem_type, access, exec)?;
            current_ipa += PAGE_SIZE as u64;
            current_pa += PAGE_SIZE as u64;
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
        let Some(root) = self.page_table_root() else {
            return Err(TranslationError::Unexpected);
        };
        let root_table = unsafe { &mut *(root.as_ptr() as *mut TablePage) };
        let l1_entry = root_table.entries()[l1_idx];
        let l2_pa = if l1_entry & VALID != 0 {
            if l1_entry & TABLE_OR_PAGE == 0 {
                return Err(TranslationError::ConflictingEntry);
            }
            l1_entry & ADDRESS_MASK
        } else {
            let next_pa = self
                .vm_mem_alloc_sub_page_table()
                .map_err(|_e| TranslationError::TableExhausted)?
                .value();
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
            let next_pa = self
                .vm_mem_alloc_sub_page_table()
                .map_err(|_| TranslationError::TableExhausted)?
                .value();

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

    pub fn new(vm_id: u8) -> Self {
        Self {
            vm_id,
            vm_mem: Vec::new(),
        }
    }

    pub fn vm_id(&self) -> u8 {
        self.vm_id
    }

    fn allocated_physical_pages(&self) -> usize {
        self.vm_mem.iter().map(VmMem::physical_pages).sum()
    }

    fn validate_ipa_range(&self, ipa: IpaAddr, size: usize) -> Result<(), AllocatorError> {
        let start = ipa.value();
        if size == 0 || !is_page_aligned(start as usize) {
            return Err(AllocatorError::InvalidIpa);
        }
        let end = start
            .checked_add(size as u64)
            .ok_or(AllocatorError::AddressOverflow)?;
        if end - 1 > (1_u64 << IPA_BITS) - 1 {
            return Err(AllocatorError::InvalidIpa);
        }

        if self
            .vm_mem
            .iter()
            .filter_map(VmMem::ipa_range)
            .any(|(existing_start, existing_end)| start < existing_end && existing_start < end)
        {
            return Err(AllocatorError::OverlappingIpa);
        }

        Ok(())
    }

    pub fn vm_mem_alloc(
        &mut self,
        usage: VmMemUsage,
        ipa: Option<IpaAddr>,
        size: usize,
    ) -> Result<Option<PhysAddr>, AllocatorError> {
        let size_aligned =
            align_up(size, PAGE_SIZE).ok_or(AllocatorError::AddressOverflow)? as usize;

        if self.vm_mem.iter().any(|v| v.usage() == usage) {
            return Err(AllocatorError::AlreadyAllocated);
        }

        let needs_ipa = matches!(
            usage,
            VmMemUsage::Image
                | VmMemUsage::Scratch
                | VmMemUsage::Dtb
                | VmMemUsage::Stack
                | VmMemUsage::Guard
        );
        if needs_ipa && ipa.is_none() {
            return Err(AllocatorError::InvalidateParameter);
        }
        if !needs_ipa && ipa.is_some() {
            return Err(AllocatorError::InvalidateParameter);
        }
        if let Some(ipa) = ipa {
            self.validate_ipa_range(ipa, size_aligned)?;
        }

        let mut phy = AddressType::new(0);
        if usage.need_physical_memory() {
            let pages = size_aligned / PAGE_SIZE;
            if self
                .allocated_physical_pages()
                .checked_add(pages)
                .ok_or(AllocatorError::AddressOverflow)?
                > MAX_GUEST_MEM_PAGES
            {
                return Err(AllocatorError::Exhausted);
            }

            phy = mm::allocate_contiguous_pages(size_aligned / PAGE_SIZE)?;
            unsafe {
                core::ptr::write_bytes(phy.into(), 0, size_aligned);
            }
        }

        let mem = match usage {
            VmMemUsage::PageTable => {
                let mut pa = Vec::new();
                for p in 0..size_aligned / PAGE_SIZE {
                    pa.push(AddressType::new(phy.value() + (p * PAGE_SIZE) as u64));
                }

                VmMem::PaOnly(PaRegion { usage, pa })
            }
            VmMemUsage::Image | VmMemUsage::Scratch | VmMemUsage::Dtb | VmMemUsage::Stack => {
                let ipa = ipa.expect("validated IpaPa ipa");
                VmMem::IpaPa(IpaPaRegion {
                    usage,
                    ipa,
                    pa: phy,
                    size: size_aligned,
                })
            }
            VmMemUsage::Guard => {
                let ipa = ipa.expect("validated IpaOnly ipa");
                VmMem::IpaOnly(IpaOnlyRegion {
                    usage,
                    base: ipa,
                    size: size_aligned,
                })
            }
        };

        let ret = match &mem {
            VmMem::IpaPa(v) => Ok(Some(v.pa)),
            VmMem::IpaOnly(_) => Ok(None),
            VmMem::PaOnly(v) => Ok(Some(v.pa[0])),
        };

        self.vm_mem.push(mem);

        ret
    }

    fn vm_mem_alloc_sub_page_table(&mut self) -> Result<AddressType, AllocatorError> {
        if self.allocated_physical_pages() >= MAX_GUEST_MEM_PAGES {
            return Err(AllocatorError::Exhausted);
        }

        let Some(VmMem::PaOnly(pt)) = self
            .vm_mem
            .iter_mut()
            .find(|f| f.usage() == VmMemUsage::PageTable)
        else {
            return Err(AllocatorError::Unexpected);
        };

        let phy = mm::allocate_page()?;
        unsafe {
            core::ptr::write_bytes(phy.into(), 0, PAGE_SIZE);
        }
        pt.pa.push(phy);
        Ok(phy)
    }

    pub fn entry_pa(&self, usage: VmMemUsage) -> impl Iterator<Item = RegionType> + '_ {
        EntryPaIter {
            entry: self.vm_mem.iter().find(|entry| entry.usage() == usage),
            next_page: 0,
        }
    }

    #[allow(unused)]
    pub fn entry_ipa(&self, usage: VmMemUsage) -> Option<IpaRegion> {
        let vm_mem = self.vm_mem.iter().find(|t| t.usage() == usage)?;
        match vm_mem {
            VmMem::IpaPa(v) => IpaRegion::new(v.ipa, v.size as u64),
            VmMem::PaOnly(_) => None,
            VmMem::IpaOnly(v) => IpaRegion::new(v.base, v.size as u64),
        }
    }

    #[allow(unused)]
    pub fn ipa_regions(&self) -> impl Iterator<Item = IpaRegion> + '_ {
        IpaRegionsIter {
            entries: &self.vm_mem,
            previous_start: None,
        }
    }

    pub fn release_all(&mut self) {
        for mem in &self.vm_mem {
            match mem {
                VmMem::IpaOnly(_) => {}
                VmMem::IpaPa(region) => {
                    let start = region.pa.value();
                    let pages = region.size / PAGE_SIZE;
                    for page in 0..pages {
                        let pa = PhysAddr::new(start + (page * PAGE_SIZE) as u64);
                        if let Err(e) = mm::free_page(pa) {
                            println!("Failed to free IpaPa page {}: {}", pa, e);
                        }
                    }
                }
                VmMem::PaOnly(region) => {
                    for pa in &region.pa {
                        if let Err(e) = mm::free_page(*pa) {
                            println!("Failed to free PaOnly page {}: {}", pa, e);
                        }
                    }
                }
            }
        }

        self.vm_mem.clear();
    }
}

impl Drop for VmCtl {
    fn drop(&mut self) {
        self.release_all();
    }
}

impl Display for VmCtl {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for v in self.vm_mem.iter() {
            writeln!(f, "{}", v)?;
        }

        Ok(())
    }
}
