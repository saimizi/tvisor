//! Static guest-visible IPA layout for the tvisor virtual platform.
//!
//! This is the single source of truth for guest RAM and virtual-device
//! addresses. Host physical addresses deliberately do not appear here.

use crate::{
    PAGE_SIZE,
    stage2_translation::IPA_BITS,
    system_info::{PhysAddr, PhysRegion},
};

pub const MIB: u64 = 1024 * 1024;
pub const DEFAULT_GUEST_RAM_SIZE: u64 = 512 * MIB;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpaRegion {
    start: u64,
    size: u64,
}

impl IpaRegion {
    pub const fn new(start: u64, size: u64) -> Self {
        Self { start, size }
    }

    pub const fn start(self) -> u64 {
        self.start
    }

    pub const fn size(self) -> u64 {
        self.size
    }

    pub const fn end(self) -> Option<u64> {
        self.start.checked_add(self.size)
    }

    pub const fn overlaps(self, other: Self) -> bool {
        match (self.end(), other.end()) {
            (Some(end), Some(other_end)) => self.start < other_end && other.start < end,
            _ => true,
        }
    }
}

/// Complete RAM presented to the initial single-vCPU guest.
pub const GUEST_RAM: IpaRegion = IpaRegion::new(0x4000_0000, DEFAULT_GUEST_RAM_SIZE);
/// Guest-visible GICv2 virtual CPU interface, including GICV_DIR at +0x1000.
pub const GUEST_GICV: IpaRegion = IpaRegion::new(0x0801_0000, 2 * PAGE_SIZE as u64);
/// Trapped, virtual PL011 UART interface.
pub const GUEST_PL011: IpaRegion = IpaRegion::new(0x0900_0000, PAGE_SIZE as u64);

pub const GUEST_DEVICE_REGIONS: [IpaRegion; 2] = [GUEST_GICV, GUEST_PL011];

/// Exact device information together with the page-aligned Stage-2 mapping
/// that makes it visible in a guest IPA window.
///
/// `device_ipa` and `device` are the values a future guest DTB must publish.
/// `mapped_pa` and `mapping_size` are solely Stage-2 page-table inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestDeviceMapping {
    pub device_ipa: u64,
    pub device: PhysRegion,
    pub mapped_pa: PhysAddr,
    pub mapping_size: usize,
}

/// Fits an exact host physical device region into a page-aligned guest IPA
/// mapping window. The page offset is preserved, as required by Stage-2 page
/// mappings, without changing the region used for hardware register access.
pub fn map_device_into_window(
    window: IpaRegion,
    device: PhysRegion,
) -> Result<GuestDeviceMapping, GuestPlatformError> {
    let page_size = PAGE_SIZE as u64;
    if window.size == 0 || window.start & (page_size - 1) != 0 || window.size & (page_size - 1) != 0
    {
        return Err(GuestPlatformError::UnalignedRegion);
    }

    let offset = device.start().value() & (page_size - 1);
    let mapped_pa = PhysAddr::new(device.start().value() - offset);
    let required = offset
        .checked_add(device.size())
        .ok_or(GuestPlatformError::AddressOverflow)?;
    let mapping_size = required
        .checked_add(page_size - 1)
        .ok_or(GuestPlatformError::AddressOverflow)?
        & !(page_size - 1);
    if mapping_size > window.size {
        return Err(GuestPlatformError::DeviceDoesNotFit);
    }

    let device_ipa = window
        .start
        .checked_add(offset)
        .ok_or(GuestPlatformError::AddressOverflow)?;
    Ok(GuestDeviceMapping {
        device_ipa,
        device,
        mapped_pa,
        mapping_size: mapping_size as usize,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestPlatformError {
    EmptyRegion,
    UnalignedRegion,
    AddressOverflow,
    IpaOutOfRange,
    OverlappingRegions,
    InvalidDeviceSize,
    DeviceDoesNotFit,
}

/// Validates the complete static guest IPA ABI before guest resources are
/// allocated or described by the guest DTB.
pub const fn validate() -> Result<(), GuestPlatformError> {
    let regions = [GUEST_RAM, GUEST_GICV, GUEST_PL011];
    let max_ipa = (1_u64 << IPA_BITS) - 1;

    let mut index = 0;
    while index < regions.len() {
        let region = regions[index];
        if region.size == 0 {
            return Err(GuestPlatformError::EmptyRegion);
        }
        if region.start & (PAGE_SIZE as u64 - 1) != 0 || region.size & (PAGE_SIZE as u64 - 1) != 0 {
            return Err(GuestPlatformError::UnalignedRegion);
        }
        let Some(end) = region.end() else {
            return Err(GuestPlatformError::AddressOverflow);
        };
        if end - 1 > max_ipa {
            return Err(GuestPlatformError::IpaOutOfRange);
        }

        let mut other = index + 1;
        while other < regions.len() {
            if region.overlaps(regions[other]) {
                return Err(GuestPlatformError::OverlappingRegions);
            }
            other += 1;
        }
        index += 1;
    }

    if GUEST_GICV.size != 2 * PAGE_SIZE as u64 || GUEST_PL011.size != PAGE_SIZE as u64 {
        return Err(GuestPlatformError::InvalidDeviceSize);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout_is_page_aligned_non_overlapping_and_in_range() {
        assert_eq!(validate(), Ok(()));
        assert_eq!(GUEST_RAM.end(), Some(0x6000_0000));
        assert!(!GUEST_GICV.overlaps(GUEST_PL011));
        assert!(!GUEST_RAM.overlaps(GUEST_GICV));
    }

    #[test]
    fn region_overlap_detects_partial_and_adjacent_ranges() {
        let base = IpaRegion::new(0x1000, 0x2000);
        assert!(base.overlaps(IpaRegion::new(0x2000, 0x1000)));
        assert!(!base.overlaps(IpaRegion::new(0x3000, 0x1000)));
    }

    #[test]
    fn device_mapping_preserves_an_unaligned_physical_offset() {
        let device = PhysRegion::new(PhysAddr::new(0xff84_6100), 0x1004).unwrap();
        let mapping = map_device_into_window(GUEST_GICV, device).unwrap();

        assert_eq!(mapping.device, device);
        assert_eq!(mapping.mapped_pa, PhysAddr::new(0xff84_6000));
        assert_eq!(mapping.mapping_size, 2 * PAGE_SIZE);
        assert_eq!(mapping.device_ipa, GUEST_GICV.start() + 0x100);
    }

    #[test]
    fn device_mapping_rejects_a_region_larger_than_its_window() {
        let device = PhysRegion::new(PhysAddr::new(0xff84_6100), 2 * PAGE_SIZE as u64).unwrap();
        assert_eq!(
            map_device_into_window(GUEST_GICV, device),
            Err(GuestPlatformError::DeviceDoesNotFit)
        );
    }
}
