//! Linux arm64 boot ABI and guest-IPA layout validation.
//!
//! This module deliberately describes only the guest-visible contract. It
//! does not allocate pages, copy an image, or expose host physical addresses;
//! those remain the responsibility of `VmCtl` and the Phase 10 image loader.

use core::fmt;

use crate::PAGE_SIZE;

/// The first guest-RAM IPA. Lower IPAs remain reserved for virtual devices.
pub const LINUX_GUEST_RAM_IPA: u64 = 0x4000_0000;
/// Initial contiguous RAM capacity for the single-vCPU Linux guest (512 MiB).
pub const LINUX_GUEST_RAM_SIZE: u64 = 512 * 1024 * 1024;
/// A two-MiB DTB reservation at the beginning of guest RAM.
pub const LINUX_DTB_IPA: u64 = LINUX_GUEST_RAM_IPA;
pub const LINUX_DTB_CAPACITY: u64 = 2 * 1024 * 1024;
/// The Linux `Image` begins after the reserved DTB window, on a 2-MiB boundary.
pub const LINUX_IMAGE_IPA: u64 = LINUX_GUEST_RAM_IPA + LINUX_DTB_CAPACITY;

const LINUX_IMAGE_ALIGNMENT: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxBootLayoutError {
    InvalidRam,
    EmptyImage,
    ImageTooLarge,
    InitrdTooLarge,
    AddressOverflow,
}

impl fmt::Display for LinuxBootLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRam => {
                f.write_str("guest RAM must be page-aligned and include the DTB window")
            }
            Self::EmptyImage => f.write_str("Linux Image must not be empty"),
            Self::ImageTooLarge => f.write_str("Linux Image does not fit in guest RAM"),
            Self::InitrdTooLarge => f.write_str("initrd does not fit in guest RAM"),
            Self::AddressOverflow => f.write_str("guest boot layout address overflow"),
        }
    }
}

/// Non-overlapping placements for a standard arm64 Linux boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxBootLayout {
    ram_ipa: u64,
    ram_size: u64,
    dtb_ipa: u64,
    dtb_capacity: u64,
    image_ipa: u64,
    image_size: u64,
    initrd: Option<(u64, u64)>,
}

impl LinuxBootLayout {
    /// Uses the initial single-vCPU contiguous guest-RAM arrangement.
    pub fn default_for_image(
        image_size: u64,
        initrd_size: Option<u64>,
    ) -> Result<Self, LinuxBootLayoutError> {
        Self::new(
            LINUX_GUEST_RAM_IPA,
            LINUX_GUEST_RAM_SIZE,
            image_size,
            initrd_size,
        )
    }

    /// Creates a layout with the DTB at the start of RAM, a 2-MiB-aligned
    /// kernel entry, and an optional initrd immediately after the rounded
    /// kernel image. Every range is guest IPA space.
    pub fn new(
        ram_ipa: u64,
        ram_size: u64,
        image_size: u64,
        initrd_size: Option<u64>,
    ) -> Result<Self, LinuxBootLayoutError> {
        if ram_ipa & (PAGE_SIZE as u64 - 1) != 0
            || ram_size & (PAGE_SIZE as u64 - 1) != 0
            || ram_size < LINUX_DTB_CAPACITY
        {
            return Err(LinuxBootLayoutError::InvalidRam);
        }
        if image_size == 0 {
            return Err(LinuxBootLayoutError::EmptyImage);
        }

        let ram_end = ram_ipa
            .checked_add(ram_size)
            .ok_or(LinuxBootLayoutError::AddressOverflow)?;
        let image_ipa = ram_ipa
            .checked_add(LINUX_DTB_CAPACITY)
            .ok_or(LinuxBootLayoutError::AddressOverflow)?;
        debug_assert_eq!(image_ipa & (LINUX_IMAGE_ALIGNMENT - 1), 0);
        let image_end = image_ipa
            .checked_add(align_up(image_size, LINUX_IMAGE_ALIGNMENT)?)
            .ok_or(LinuxBootLayoutError::AddressOverflow)?;
        if image_end > ram_end {
            return Err(LinuxBootLayoutError::ImageTooLarge);
        }

        let initrd = match initrd_size {
            None | Some(0) => None,
            Some(size) => {
                let end = image_end
                    .checked_add(align_up(size, PAGE_SIZE as u64)?)
                    .ok_or(LinuxBootLayoutError::AddressOverflow)?;
                if end > ram_end {
                    return Err(LinuxBootLayoutError::InitrdTooLarge);
                }
                Some((image_end, size))
            }
        };

        Ok(Self {
            ram_ipa,
            ram_size,
            dtb_ipa: ram_ipa,
            dtb_capacity: LINUX_DTB_CAPACITY,
            image_ipa,
            image_size,
            initrd,
        })
    }

    pub const fn ram(&self) -> (u64, u64) {
        (self.ram_ipa, self.ram_size)
    }
    pub const fn dtb(&self) -> (u64, u64) {
        (self.dtb_ipa, self.dtb_capacity)
    }
    pub const fn image(&self) -> (u64, u64) {
        (self.image_ipa, self.image_size)
    }
    pub const fn initrd(&self) -> Option<(u64, u64)> {
        self.initrd
    }

    /// Produces the only register state tvisor supplies to a Linux kernel.
    pub const fn initial_registers(&self) -> LinuxBootRegisters {
        LinuxBootRegisters {
            pc: self.image_ipa,
            x0: self.dtb_ipa,
            x1: 0,
            x2: 0,
            x3: 0,
        }
    }
}

fn align_up(value: u64, alignment: u64) -> Result<u64, LinuxBootLayoutError> {
    let remainder = value % alignment;
    if remainder == 0 {
        Ok(value)
    } else {
        value
            .checked_add(alignment - remainder)
            .ok_or(LinuxBootLayoutError::AddressOverflow)
    }
}

/// AArch64 Linux boot-register contract at the EL2-to-EL1 `ERET` boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxBootRegisters {
    pub pc: u64,
    pub x0: u64,
    pub x1: u64,
    pub x2: u64,
    pub x3: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout_has_the_linux_register_abi() {
        let layout =
            LinuxBootLayout::default_for_image(12 * 1024 * 1024, Some(3 * 1024 * 1024)).unwrap();
        assert_eq!(layout.ram(), (LINUX_GUEST_RAM_IPA, LINUX_GUEST_RAM_SIZE));
        assert_eq!(layout.dtb(), (LINUX_DTB_IPA, LINUX_DTB_CAPACITY));
        assert_eq!(layout.image(), (LINUX_IMAGE_IPA, 12 * 1024 * 1024));
        assert_eq!(
            layout.initrd(),
            Some((LINUX_IMAGE_IPA + 12 * 1024 * 1024, 3 * 1024 * 1024))
        );
        assert_eq!(
            layout.initial_registers(),
            LinuxBootRegisters {
                pc: LINUX_IMAGE_IPA,
                x0: LINUX_DTB_IPA,
                x1: 0,
                x2: 0,
                x3: 0,
            }
        );
    }

    #[test]
    fn kernel_and_initrd_are_rejected_when_they_exceed_ram() {
        assert_eq!(
            LinuxBootLayout::default_for_image(LINUX_GUEST_RAM_SIZE, None),
            Err(LinuxBootLayoutError::ImageTooLarge)
        );
        assert_eq!(
            LinuxBootLayout::default_for_image(
                LINUX_GUEST_RAM_SIZE - LINUX_DTB_CAPACITY,
                Some(PAGE_SIZE as u64)
            ),
            Err(LinuxBootLayoutError::InitrdTooLarge)
        );
    }

    #[test]
    fn invalid_or_empty_layouts_are_rejected() {
        assert_eq!(
            LinuxBootLayout::default_for_image(0, None),
            Err(LinuxBootLayoutError::EmptyImage)
        );
        assert_eq!(
            LinuxBootLayout::new(
                LINUX_GUEST_RAM_IPA + 1,
                LINUX_GUEST_RAM_SIZE,
                PAGE_SIZE as u64,
                None
            ),
            Err(LinuxBootLayoutError::InvalidRam)
        );
    }
}
