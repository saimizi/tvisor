//! Linux arm64 `Image` parsing and guest-IPA boot-layout validation.
//!
//! The Linux Image header, rather than a tvisor-specific fixed address, is
//! authoritative for the kernel entry location and occupied image extent.

use core::fmt;

use crate::PAGE_SIZE;

pub const LINUX_GUEST_RAM_IPA: u64 = 0x4000_0000;
pub const LINUX_GUEST_RAM_SIZE: u64 = 512 * 1024 * 1024;
pub const LINUX_IMAGE_BASE_ALIGNMENT: u64 = 2 * 1024 * 1024;
pub const LINUX_DTB_CAPACITY: u64 = 2 * 1024 * 1024;
pub const LINUX_IMAGE_HEADER_SIZE: usize = 64;
const LINUX_IMAGE_MAGIC: u32 = 0x644d_5241;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxImageError {
    HeaderTooSmall,
    BadMagic,
    LegacyImageSize,
    ImageTooLarge,
    BigEndianImage,
}

impl fmt::Display for LinuxImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeaderTooSmall => f.write_str("Linux Image is shorter than its 64-byte header"),
            Self::BadMagic => f.write_str("Linux Image header has an invalid magic value"),
            Self::LegacyImageSize => f.write_str("Linux Image has no authoritative image_size"),
            Self::ImageTooLarge => {
                f.write_str("Linux Image is shorter than its declared image_size")
            }
            Self::BigEndianImage => f.write_str("big-endian arm64 Linux Images are unsupported"),
        }
    }
}

/// The Linux arm64 Image header fields that affect loading and entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxImageHeader {
    pub text_offset: u64,
    pub image_size: u64,
    pub flags: u64,
}

impl LinuxImageHeader {
    pub fn parse(image: &[u8]) -> Result<Self, LinuxImageError> {
        if image.len() < LINUX_IMAGE_HEADER_SIZE {
            return Err(LinuxImageError::HeaderTooSmall);
        }
        let magic = u32::from_le_bytes(image[56..60].try_into().expect("header length checked"));
        if magic != LINUX_IMAGE_MAGIC {
            return Err(LinuxImageError::BadMagic);
        }
        let text_offset =
            u64::from_le_bytes(image[8..16].try_into().expect("header length checked"));
        let image_size =
            u64::from_le_bytes(image[16..24].try_into().expect("header length checked"));
        let flags = u64::from_le_bytes(image[24..32].try_into().expect("header length checked"));
        if flags & 1 != 0 {
            return Err(LinuxImageError::BigEndianImage);
        }
        // Phase 10 requires an explicit extent: a legacy zero-sized header
        // cannot prove safe DTB and initrd placement.
        if image_size == 0 {
            return Err(LinuxImageError::LegacyImageSize);
        }
        if image_size > image.len() as u64 {
            return Err(LinuxImageError::ImageTooLarge);
        }
        Ok(Self {
            text_offset,
            image_size,
            flags,
        })
    }

    pub const fn page_size_encoding(self) -> u8 {
        ((self.flags >> 1) & 0x3) as u8
    }

    pub const fn requires_48bit_placement(self) -> bool {
        self.flags & (1 << 3) != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxBootLayoutError {
    Image(LinuxImageError),
    InvalidRam,
    ImageTooLarge,
    DtbTooLarge,
    InitrdTooLarge,
    AddressOverflow,
}

impl From<LinuxImageError> for LinuxBootLayoutError {
    fn from(error: LinuxImageError) -> Self {
        Self::Image(error)
    }
}

impl fmt::Display for LinuxBootLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Image(error) => write!(f, "invalid Linux Image: {error}"),
            Self::InvalidRam => f.write_str("guest RAM base must be 2-MiB aligned and page sized"),
            Self::ImageTooLarge => f.write_str("Linux Image does not fit in guest RAM"),
            Self::DtbTooLarge => f.write_str("reserved DTB window does not fit in guest RAM"),
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
    image: LinuxImageHeader,
    image_ipa: u64,
    dtb_ipa: u64,
    initrd: Option<(u64, u64)>,
}

impl LinuxBootLayout {
    pub fn default_for_image(
        image: &[u8],
        initrd_size: Option<u64>,
    ) -> Result<Self, LinuxBootLayoutError> {
        Self::new(
            image,
            LINUX_GUEST_RAM_IPA,
            LINUX_GUEST_RAM_SIZE,
            initrd_size,
        )
    }

    /// Places the Image at `ram_ipa + header.text_offset`; `ram_ipa` is the
    /// 2-MiB-aligned base mandated by the arm64 Linux boot ABI. The generated
    /// DTB follows the declared Image extent, preventing an overlap with a
    /// valid `text_offset` such as the common 0x80000.
    pub fn new(
        image: &[u8],
        ram_ipa: u64,
        ram_size: u64,
        initrd_size: Option<u64>,
    ) -> Result<Self, LinuxBootLayoutError> {
        let image_header = LinuxImageHeader::parse(image)?;
        if ram_ipa & (LINUX_IMAGE_BASE_ALIGNMENT - 1) != 0 || ram_size & (PAGE_SIZE as u64 - 1) != 0
        {
            return Err(LinuxBootLayoutError::InvalidRam);
        }
        let ram_end = ram_ipa
            .checked_add(ram_size)
            .ok_or(LinuxBootLayoutError::AddressOverflow)?;
        let image_ipa = ram_ipa
            .checked_add(image_header.text_offset)
            .ok_or(LinuxBootLayoutError::AddressOverflow)?;
        let image_end = image_ipa
            .checked_add(image_header.image_size)
            .ok_or(LinuxBootLayoutError::AddressOverflow)?;
        if image_ipa < ram_ipa || image_end > ram_end {
            return Err(LinuxBootLayoutError::ImageTooLarge);
        }
        if image_header.requires_48bit_placement() && image_end > (1_u64 << 48) {
            return Err(LinuxBootLayoutError::ImageTooLarge);
        }

        let dtb_ipa = align_up(image_end, 8)?;
        let dtb_end = dtb_ipa
            .checked_add(LINUX_DTB_CAPACITY)
            .ok_or(LinuxBootLayoutError::AddressOverflow)?;
        if dtb_end > ram_end {
            return Err(LinuxBootLayoutError::DtbTooLarge);
        }
        let initrd = match initrd_size {
            None | Some(0) => None,
            Some(size) => {
                let initrd_ipa = align_up(dtb_end, PAGE_SIZE as u64)?;
                let end = initrd_ipa
                    .checked_add(align_up(size, PAGE_SIZE as u64)?)
                    .ok_or(LinuxBootLayoutError::AddressOverflow)?;
                if end > ram_end {
                    return Err(LinuxBootLayoutError::InitrdTooLarge);
                }
                Some((initrd_ipa, size))
            }
        };
        Ok(Self {
            ram_ipa,
            ram_size,
            image: image_header,
            image_ipa,
            dtb_ipa,
            initrd,
        })
    }

    pub const fn ram(&self) -> (u64, u64) {
        (self.ram_ipa, self.ram_size)
    }

    pub const fn image_header(&self) -> LinuxImageHeader {
        self.image
    }

    pub const fn image(&self) -> (u64, u64) {
        (self.image_ipa, self.image.image_size)
    }

    pub const fn dtb(&self) -> (u64, u64) {
        (self.dtb_ipa, LINUX_DTB_CAPACITY)
    }

    pub const fn initrd(&self) -> Option<(u64, u64)> {
        self.initrd
    }

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

    fn image(text_offset: u64, image_size: u64, flags: u64) -> alloc::vec::Vec<u8> {
        let mut image =
            alloc::vec![0; core::cmp::max(image_size as usize, LINUX_IMAGE_HEADER_SIZE)];
        image[8..16].copy_from_slice(&text_offset.to_le_bytes());
        image[16..24].copy_from_slice(&image_size.to_le_bytes());
        image[24..32].copy_from_slice(&flags.to_le_bytes());
        image[56..60].copy_from_slice(&LINUX_IMAGE_MAGIC.to_le_bytes());
        image
    }

    #[test]
    fn layout_uses_image_header_for_entry_and_dtb_placement() {
        let image = image(0x80000, 12 * 1024 * 1024, 0b10);
        let layout = LinuxBootLayout::default_for_image(&image, Some(3 * 1024 * 1024)).unwrap();
        assert_eq!(layout.ram(), (LINUX_GUEST_RAM_IPA, LINUX_GUEST_RAM_SIZE));
        assert_eq!(
            layout.image(),
            (LINUX_GUEST_RAM_IPA + 0x80000, 12 * 1024 * 1024)
        );
        assert_eq!(
            layout.dtb(),
            (
                LINUX_GUEST_RAM_IPA + 0x80000 + 12 * 1024 * 1024,
                LINUX_DTB_CAPACITY
            )
        );
        assert_eq!(
            layout.initrd(),
            Some((
                LINUX_GUEST_RAM_IPA + 0x80000 + 14 * 1024 * 1024,
                3 * 1024 * 1024
            ))
        );
        assert_eq!(layout.image_header().page_size_encoding(), 1);
        assert_eq!(
            layout.initial_registers(),
            LinuxBootRegisters {
                pc: LINUX_GUEST_RAM_IPA + 0x80000,
                x0: LINUX_GUEST_RAM_IPA + 0x80000 + 12 * 1024 * 1024,
                x1: 0,
                x2: 0,
                x3: 0,
            }
        );
    }

    #[test]
    fn rejects_invalid_headers_and_unaligned_ram_base() {
        assert_eq!(
            LinuxImageHeader::parse(&[]),
            Err(LinuxImageError::HeaderTooSmall)
        );
        let mut invalid = image(0x80000, 4096, 0);
        invalid[56] = 0;
        assert_eq!(
            LinuxImageHeader::parse(&invalid),
            Err(LinuxImageError::BadMagic)
        );
        let image = image(0x80000, 4096, 0);
        assert_eq!(
            LinuxBootLayout::new(
                &image,
                LINUX_GUEST_RAM_IPA + PAGE_SIZE as u64,
                LINUX_GUEST_RAM_SIZE,
                None
            ),
            Err(LinuxBootLayoutError::InvalidRam)
        );
    }

    #[test]
    fn rejects_legacy_big_endian_and_oversized_images() {
        assert_eq!(
            LinuxImageHeader::parse(&image(0x80000, 0, 0)),
            Err(LinuxImageError::LegacyImageSize)
        );
        assert_eq!(
            LinuxImageHeader::parse(&image(0x80000, 4096, 1)),
            Err(LinuxImageError::BigEndianImage)
        );
        let mut short = image(0x80000, 4096, 0);
        short.truncate(64);
        assert_eq!(
            LinuxImageHeader::parse(&short),
            Err(LinuxImageError::ImageTooLarge)
        );
    }
}
